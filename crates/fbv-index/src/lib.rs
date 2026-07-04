//! Import pipeline: discover candidate files, hash, dedupe, parse on worker
//! threads, write metadata + cache. Failures quarantine the file; nothing
//! here may panic the process on bad input.
//!
//! Wiring: `run_import` executes on a caller-provided thread, owns the
//! writer `Database`, fans parsing out to workers over crossbeam channels,
//! and reports `ImportEvent`s back to the UI.

use anyhow::Result;
use crossbeam_channel::{bounded, Receiver, Sender};
use fbv_core::{identity, BoardModel};
use fbv_data::{cache::BlobCache, Database, NewBoard};
use fbv_parsers::{ParseContext, ParseError, KNOWN_EXTENSIONS};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

/// Progress events streamed to the UI.
#[derive(Debug, Clone)]
pub enum ImportEvent {
    Discovered(usize),
    Imported { path: PathBuf, board_id: i64 },
    AlreadyKnown { path: PathBuf, board_id: i64 },
    Failed { path: PathBuf, reason: String },
    Skipped { path: PathBuf },
    Finished { imported: usize, failed: usize, known: usize, skipped: usize },
}

/// Decryption keys handed to parser workers.
#[derive(Debug, Clone, Copy, Default)]
pub struct Keys {
    pub fz_key: Option<[u32; 44]>,
    pub xzz_key: Option<u64>,
}

/// Maximum file size worth attempting; boardviews are 100 KB..5 MB, so 64 MB
/// catches renamed junk (ISOs, videos) without excluding any real board.
const MAX_FILE_SIZE: u64 = 64 * 1024 * 1024;

pub fn has_known_extension(path: &Path) -> bool {
    path.extension()
        .map(|e| {
            let e = e.to_string_lossy().to_ascii_lowercase();
            KNOWN_EXTENSIONS.contains(&e.as_str())
        })
        .unwrap_or(false)
}

/// Recursively lists candidate files under the given roots.
pub fn discover(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        if root.is_file() {
            out.push(root.clone());
            continue;
        }
        for entry in WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() && has_known_extension(entry.path()) {
                out.push(entry.into_path());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    let digest = h.finalize();
    let mut s = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

struct ParsedFile {
    path: PathBuf,
    size: i64,
    mtime: i64,
    result: std::result::Result<(String, String, BoardModel), String>, // (sha, format label, model)
}

fn file_mtime(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Parses one file end-to-end (read, hash, detect, parse). Panics inside
/// parsers are caught so one weird file cannot kill a worker.
fn parse_one(path: &Path, keys: &Keys) -> ParsedFile {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mtime = file_mtime(path);
    let mk = |result| ParsedFile {
        path: path.to_path_buf(),
        size: size as i64,
        mtime,
        result,
    };

    if size == 0 || size > MAX_FILE_SIZE {
        return mk(Err(format!("implausible file size {size}")));
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return mk(Err(format!("read error: {e}"))),
    };
    let sha = sha256_hex(&bytes);

    let dir = path.parent().map(|p| p.to_path_buf());
    let companion = move |name: &str| -> Option<Vec<u8>> {
        let dir = dir.as_ref()?;
        // Case-insensitive sibling lookup.
        let entries = std::fs::read_dir(dir).ok()?;
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().eq_ignore_ascii_case(name) {
                return std::fs::read(e.path()).ok();
            }
        }
        None
    };
    let ctx = ParseContext {
        fz_key: keys.fz_key,
        xzz_key: keys.xzz_key,
        companion: Some(&companion),
    };

    let parse_attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fbv_parsers::detect_and_parse(&bytes, Some(path), &ctx)
    }));

    match parse_attempt {
        Ok(Ok((format, model))) => mk(Ok((sha, format.label().to_string(), model))),
        Ok(Err(ParseError::Unrecognized)) => mk(Err("unrecognized format".into())),
        Ok(Err(e)) => mk(Err(e.to_string())),
        Err(_) => mk(Err("parser panicked (please report this file)".into())),
    }
}

/// Runs a full import of `roots`. Blocking; call on a dedicated thread.
/// `db` must be the writer connection; `events` receives progress.
pub fn run_import(
    db: &mut Database,
    cache: &BlobCache,
    roots: &[PathBuf],
    keys: Keys,
    events: &Sender<ImportEvent>,
    cancel: &Receiver<()>,
) -> Result<()> {
    let files = discover(roots);
    let _ = events.send(ImportEvent::Discovered(files.len()));

    // Fast-path skip: path already known with same size/mtime, or already
    // quarantined with same size/mtime.
    let mut to_parse: Vec<PathBuf> = Vec::new();
    let mut skipped = 0usize;
    {
        let conn = db.connection();
        let mut known = conn.prepare(
            "SELECT 1 FROM board_files WHERE abs_path = ?1 AND size = ?2 AND mtime = ?3 AND missing = 0",
        )?;
        for f in files {
            let size = std::fs::metadata(&f).map(|m| m.len() as i64).unwrap_or(0);
            let mtime = file_mtime(&f);
            let already = known
                .exists(rusqlite::params![f.to_string_lossy(), size, mtime])
                .unwrap_or(false)
                || db
                    .is_quarantined(&f, size, mtime, fbv_parsers::PARSER_GENERATION)
                    .unwrap_or(false);
            if already {
                skipped += 1;
                let _ = events.send(ImportEvent::Skipped { path: f });
            } else {
                to_parse.push(f);
            }
        }
    }

    // Fan out parsing; keep DB writes on this thread.
    let workers = std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).max(1))
        .unwrap_or(2)
        .min(8);
    let (work_tx, work_rx) = bounded::<PathBuf>(workers * 2);
    let (done_tx, done_rx) = bounded::<ParsedFile>(workers * 2);
    let keys = Arc::new(keys);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let work_rx = work_rx.clone();
            let done_tx = done_tx.clone();
            let keys = Arc::clone(&keys);
            scope.spawn(move || {
                while let Ok(path) = work_rx.recv() {
                    let parsed = parse_one(&path, &keys);
                    if done_tx.send(parsed).is_err() {
                        break;
                    }
                }
            });
        }
        drop(done_tx);

        // Feeder co-routine: push work while draining results, single thread
        // (this one) handles all DB writes.
        let feeder = scope.spawn(move || {
            for f in to_parse {
                if work_tx.send(f).is_err() {
                    break;
                }
            }
            drop(work_tx);
        });

        let mut imported = 0usize;
        let mut failed = 0usize;
        let mut known_count = 0usize;
        let mut cancelled = false;
        while let Ok(parsed) = done_rx.recv() {
            if cancel.try_recv().is_ok() {
                cancelled = true;
                break;
            }
            match parsed.result {
                Ok((sha, format_label, model)) => {
                    match db.board_id_for_hash(&sha) {
                        Ok(Some(existing)) => {
                            let _ = db.upsert_file(existing, &parsed.path, parsed.size, parsed.mtime);
                            // A duplicate under a better-named path can still
                            // improve a missing OEM code.
                            let guess = identity::guess_identity(&parsed.path);
                            if let Some(code) = guess.oem_code.as_deref() {
                                let _ = db.fill_missing_oem_code(existing, code);
                            }
                            known_count += 1;
                            let _ = events.send(ImportEvent::AlreadyKnown {
                                path: parsed.path,
                                board_id: existing,
                            });
                        }
                        Ok(None) => {
                            let guess = identity::guess_identity(&parsed.path);
                            let insert = db.insert_board(&NewBoard {
                                sha256: &sha,
                                display_name: &guess.display_name,
                                oem_code: guess.oem_code.as_deref(),
                                format: &format_label,
                                model: &model,
                                path: &parsed.path,
                                size: parsed.size,
                                mtime: parsed.mtime,
                            });
                            match insert {
                                Ok(board_id) => {
                                    let _ = cache.store(&sha, &model);
                                    // A file that failed under an older
                                    // parser generation now succeeds: clear
                                    // it from Problems.
                                    let _ = db.unquarantine(&parsed.path);
                                    imported += 1;
                                    let _ = events.send(ImportEvent::Imported {
                                        path: parsed.path,
                                        board_id,
                                    });
                                }
                                Err(e) => {
                                    failed += 1;
                                    let _ = db.quarantine(
                                        &parsed.path,
                                        &format!("db error: {e}"),
                                        parsed.size,
                                        parsed.mtime,
                                        fbv_parsers::PARSER_GENERATION,
                                    );
                                    let _ = events.send(ImportEvent::Failed {
                                        path: parsed.path,
                                        reason: e.to_string(),
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            failed += 1;
                            let _ = events.send(ImportEvent::Failed {
                                path: parsed.path,
                                reason: e.to_string(),
                            });
                        }
                    }
                }
                Err(reason) => {
                    failed += 1;
                    let _ = db.quarantine(
                        &parsed.path,
                        &reason,
                        parsed.size,
                        parsed.mtime,
                        fbv_parsers::PARSER_GENERATION,
                    );
                    let _ = events.send(ImportEvent::Failed {
                        path: parsed.path,
                        reason,
                    });
                }
            }
        }
        // On cancel, workers may be blocked sending into `done_rx`; dropping
        // the receiver unblocks them so the scope can join.
        drop(done_rx);
        let _ = feeder.join();
        let _ = cancelled;

        let _ = events.send(ImportEvent::Finished {
            imported,
            failed,
            known: known_count,
            skipped,
        });
    });

    Ok(())
}

/// Marks library files that no longer exist on disk (startup reconciliation).
pub fn reconcile_missing(db: &Database) -> Result<usize> {
    let conn = db.connection();
    let mut stmt = conn.prepare("SELECT file_id, abs_path FROM board_files")?;
    let rows: Vec<(i64, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let mut changed = 0usize;
    for (file_id, path) in rows {
        let missing = !Path::new(&path).exists();
        let updated = conn.execute(
            "UPDATE board_files SET missing = ?2 WHERE file_id = ?1 AND missing != ?2",
            rusqlite::params![file_id, missing as i64],
        )?;
        changed += updated;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;

    fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    const BRD: &str = "\
str_length:
800
var_data:
4 1 2 0
Format:
0 0
1000 0
1000 800
0 800
Parts:
U5300 5 2
Pins:
100 200 1 1 PPBUS_G3H
110 200 2 1 GND
";

    #[test]
    fn end_to_end_import_with_garbage_and_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lib");
        std::fs::create_dir_all(root.join("sub")).unwrap();

        write(&root, "820-00281 MLB.brd", BRD);
        write(&root.join("sub"), "duplicate copy.brd", BRD); // same content
        write(&root, "garbage.brd", "this is not a boardview at all");
        write(&root, "ignored.txt", BRD); // wrong extension: not discovered

        let mut db = Database::open_in_memory().unwrap();
        let cache = BlobCache::new(dir.path().join("cache")).unwrap();
        let (tx, rx) = unbounded();
        let (_cancel_tx, cancel_rx) = unbounded::<()>();

        run_import(
            &mut db,
            &cache,
            &[root.clone()],
            Keys::default(),
            &tx,
            &cancel_rx,
        )
        .unwrap();

        let events: Vec<ImportEvent> = rx.try_iter().collect();
        let finished = events
            .iter()
            .find_map(|e| match e {
                ImportEvent::Finished {
                    imported,
                    failed,
                    known,
                    skipped,
                } => Some((*imported, *failed, *known, *skipped)),
                _ => None,
            })
            .expect("finished event");
        assert_eq!(finished, (1, 1, 1, 0)); // 1 new, 1 garbage, 1 duplicate

        let (boards, parts, nets) = db.stats().unwrap();
        assert_eq!(boards, 1);
        assert_eq!(parts, 1);
        assert_eq!(nets, 2);

        // Cache entry exists and loads.
        let rows = db.list_boards().unwrap();
        let model = cache.load(&rows[0].sha256).unwrap().unwrap();
        assert_eq!(model.parts.iter().filter(|p| !p.is_dummy).count(), 1);
        // Identity heuristics picked up the Apple code.
        assert_eq!(rows[0].oem_code.as_deref(), Some("820-00281"));

        // Second run: everything skips (path+size+mtime known or quarantined).
        let (tx2, rx2) = unbounded();
        run_import(
            &mut db,
            &cache,
            &[root],
            Keys::default(),
            &tx2,
            &cancel_rx,
        )
        .unwrap();
        let finished2 = rx2
            .try_iter()
            .find_map(|e| match e {
                ImportEvent::Finished {
                    imported,
                    failed,
                    known,
                    skipped,
                } => Some((imported, failed, known, skipped)),
                _ => None,
            })
            .unwrap();
        assert_eq!(finished2, (0, 0, 0, 3));
    }

    const GENCAD: &str = "$HEADER
GENCAD 1.4
UNITS THOU
$ENDHEADER
$SHAPES
SHAPE S1
PIN 1 P1 0 0
$ENDSHAPES
$COMPONENTS
COMPONENT U5300
DEVICE D1
PLACE 100 100
LAYER TOP
ROTATION 0
SHAPE S1 0 0
$ENDCOMPONENTS
$SIGNALS
SIGNAL PPBUS_G3H
NODE U5300 1
$ENDSIGNALS
$DEVICES
DEVICE D1
PART ISL9239
$ENDDEVICES
";

    #[test]
    fn gencad_cad_files_import_and_index() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "quanta_board.cad", GENCAD);
        let mut db = Database::open_in_memory().unwrap();
        let cache = BlobCache::new(dir.path().join("cache")).unwrap();
        let (tx, rx) = unbounded();
        let (_ct, cr) = unbounded::<()>();
        run_import(&mut db, &cache, &[p], Keys::default(), &tx, &cr).unwrap();
        let ok = rx
            .try_iter()
            .any(|e| matches!(e, ImportEvent::Imported { .. }));
        assert!(ok, "GenCAD .cad file must import");
        let rows = db.list_boards().unwrap();
        assert_eq!(rows[0].format, "GenCAD");
        assert_eq!(rows[0].part_count, 1);
        // Part number from $DEVICES is searchable metadata.
        let pn: String = db
            .connection()
            .query_row("SELECT part_number FROM parts LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pn, "ISL9239");
    }

    #[test]
    fn old_generation_quarantine_is_retried() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "board.cad", GENCAD);
        let size = std::fs::metadata(&p).unwrap().len() as i64;
        let mtime = file_mtime(&p);

        let mut db = Database::open_in_memory().unwrap();
        // Simulate: an older app version failed on this exact file.
        db.quarantine(&p, "unrecognized format", size, mtime, 1)
            .unwrap();

        let cache = BlobCache::new(dir.path().join("cache")).unwrap();
        let (tx, rx) = unbounded();
        let (_ct, cr) = unbounded::<()>();
        run_import(&mut db, &cache, &[p], Keys::default(), &tx, &cr).unwrap();
        assert!(
            rx.try_iter()
                .any(|e| matches!(e, ImportEvent::Imported { .. })),
            "file quarantined under an older parser generation must be retried"
        );
        // And it left the Problems list.
        assert!(db.list_quarantine().unwrap().is_empty());
    }

    #[test]
    fn reconcile_marks_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "b.brd", BRD);
        let mut db = Database::open_in_memory().unwrap();
        let cache = BlobCache::new(dir.path().join("cache")).unwrap();
        let (tx, _rx) = unbounded();
        let (_ct, cr) = unbounded::<()>();
        run_import(&mut db, &cache, &[p.clone()], Keys::default(), &tx, &cr).unwrap();
        assert_eq!(reconcile_missing(&db).unwrap(), 0);
        std::fs::remove_file(&p).unwrap();
        assert_eq!(reconcile_missing(&db).unwrap(), 1);
        assert!(db.list_boards().unwrap()[0].missing);
    }
}
