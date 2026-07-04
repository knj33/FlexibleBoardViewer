//! Parsed-board blob cache: `cache/{sha256}.fbb`, bincode-serialized
//! `BoardModel` behind a small versioned header. Disposable by design —
//! deleting the directory only costs re-parse time.

use anyhow::{bail, Context, Result};
use fbv_core::{BoardModel, MODEL_VERSION};
use std::io::{Read, Write};
use std::path::PathBuf;

const MAGIC: &[u8; 4] = b"FBVC";

pub struct BlobCache {
    dir: PathBuf,
}

impl BlobCache {
    pub fn new(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating cache dir {}", dir.display()))?;
        Ok(Self { dir })
    }

    fn path_for(&self, sha256: &str) -> PathBuf {
        self.dir.join(format!("{sha256}.fbb"))
    }

    pub fn store(&self, sha256: &str, model: &BoardModel) -> Result<()> {
        let tmp = self.dir.join(format!("{sha256}.tmp"));
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(MAGIC)?;
            f.write_all(&MODEL_VERSION.to_le_bytes())?;
            let payload = bincode::serialize(model)?;
            f.write_all(&payload)?;
            f.sync_all().ok();
        }
        // Atomic-enough on the same filesystem; a torn cache entry is
        // detected by the header/bincode checks and simply re-parsed.
        std::fs::rename(&tmp, self.path_for(sha256))?;
        Ok(())
    }

    pub fn load(&self, sha256: &str) -> Result<Option<BoardModel>> {
        let path = self.path_for(sha256);
        let mut f = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut header = [0u8; 8];
        if f.read_exact(&mut header).is_err() {
            return Ok(None); // torn write: treat as absent
        }
        if &header[..4] != MAGIC {
            bail!("bad cache magic in {}", path.display());
        }
        let version = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        if version != MODEL_VERSION {
            return Ok(None); // stale generation: re-parse
        }
        let mut payload = Vec::new();
        f.read_to_end(&mut payload)?;
        match bincode::deserialize(&payload) {
            Ok(model) => Ok(Some(model)),
            Err(_) => Ok(None), // corrupt: re-parse
        }
    }

    pub fn remove(&self, sha256: &str) {
        let _ = std::fs::remove_file(self.path_for(sha256));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbv_core::{BoardBuilder, BoardFormat};

    #[test]
    fn store_load_roundtrip_and_corruption_tolerance() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(dir.path().to_path_buf()).unwrap();

        let mut b = BoardBuilder::new();
        b.add_outline_segment(
            fbv_core::Point::new(0.0, 0.0),
            fbv_core::Point::new(100.0, 0.0),
        );
        let model = b.finish(BoardFormat::Brd);

        assert!(cache.load("deadbeef").unwrap().is_none());
        cache.store("deadbeef", &model).unwrap();
        let loaded = cache.load("deadbeef").unwrap().unwrap();
        assert_eq!(loaded.outline.len(), 1);
        assert_eq!(loaded.format_label, model.format_label);

        // Corrupt payload: silently treated as a miss, not an error.
        std::fs::write(
            dir.path().join("deadbeef.fbb"),
            [MAGIC.as_slice(), &fbv_core::MODEL_VERSION.to_le_bytes(), b"garbage"].concat(),
        )
        .unwrap();
        assert!(cache.load("deadbeef").unwrap().is_none());
    }
}
