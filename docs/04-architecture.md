# FlexibleBoardViewer — Proposed Architecture

> **Status: PROPOSAL — the tech-stack choice (Q-7) and platform targets (Q-1) gate this.**
> Everything below is written to survive any of the three candidate stacks; stack-specific notes
> are marked. No code will be written until this document is approved.

## 1. Technology stack — candidates and recommendation

The workload profile: 2D vector canvas with tens of thousands of static primitives per board
(small by GPU standards), heavy background I/O + parsing, a disk-backed search index, and a
docking-panel desktop UI. This is *not* a rendering-bound problem — it is a **product-velocity and
robustness** problem. Candidates:

| | A: C++20 + Qt 6 | B: C# / .NET 9 + Avalonia + SkiaSharp | C: Rust + egui/wgpu |
|---|---|---|---|
| Canvas rendering | custom QOpenGLWidget / QPainter — excellent | SkiaSharp (GPU-backed) — excellent for this scale | wgpu — excellent |
| Docking UI | Qt Advanced Docking System — best in class | Dock.Avalonia — good, less polished | immature |
| Dev velocity | slowest of the three | **fastest** (async/await for pipeline, LINQ, hot reload) | slow (UI ecosystem) |
| Background pipeline | QThread/std — fine, more ceremony | Channels + async — natural fit | tokio/rayon — natural fit |
| SQLite + FTS5 | fine | first-class (`Microsoft.Data.Sqlite`) | fine |
| Parser port from OBV (C) | trivial (same family) | straightforward (they're simple tokenizers) | straightforward |
| Crash robustness | manual discipline | memory-safe (GC) | memory-safe |
| Distribution | large-ish, Qt LGPL dynamic-link obligations | self-contained single dir, trimmed ~60–90 MB | small binary |
| Native look/feel & a11y | best | good | weakest |
| Precedent in this niche | all serious EDA tools | — | — |
| Hiring/handoff | hard | easy | medium |

**Recommendation: B (C#/.NET + Avalonia + SkiaSharp), MVVM.** Rationale: the differentiating work
is the indexing/search/library product layer, where .NET's velocity is 2–3× C++; rendering load is
far below Skia's ceiling (a full MacBook boardview is ~3k parts / ~15k pads — a trivial frame);
memory safety directly serves NFR-8 ("bad file never crashes the app"); one codebase covers
Win/macOS/Linux. **Fallback A (Qt/C++)** if you (the owner) are a C++ developer or want maximal
native pedigree — the architecture below is unchanged. C is not recommended as primary.

Rejected: Electron/web-tech (RAM footprint and pro-tool feel conflict with NFR-4 and the market's
expectations; browser viewers already exist and haven't displaced native tools).

## 2. Architectural style

**MVVM + layered core with strict one-way dependencies.** MVVM over MVC because docking-panel apps
are state-synchronization problems (selection in one panel updates five others) — observable
view-models + a message bus handle that cleanly, and view-models are unit-testable without UI.

```
┌─────────────────────────── App (composition root, DI) ───────────────────────────┐
│  Views (XAML/panels)  →  ViewModels  →  Application services                     │
└──────────┬─────────────────────┬──────────────────────────┬──────────────────────┘
           │                     │                          │
     Rendering            Search / Query              Indexing pipeline
  (scene, hit-test,     (query parser, ranking,     (watcher, hasher, queue,
   themes, overlays)      FTS access)                 workers, progress)
           │                     │                          │
           └──────────► Core domain model ◄─────────────────┘
                    (BoardModel IR, geometry, units)
                              ▲
                    Parsers (plugin contract + built-ins)   ← no dependency on anything above
                              ▲
                    Data layer (SQLite repos, FTS5 index, blob cache)
```

Selection/navigation events (part selected, net highlighted, board opened, search activated) flow
over an in-process event bus; panels subscribe. No panel talks to another panel directly.

## 3. Repository / folder structure

```
FlexibleBoardViewer/
├── docs/                         # these documents, ADRs (docs/adr/NNNN-*.md)
├── src/
│   ├── Fbv.Core/                 # BoardModel IR, geometry, units, event contracts. Zero deps.
│   ├── Fbv.Parsers/              # IBoardParser + one sub-namespace per format + detection registry
│   ├── Fbv.Data/                 # SQLite schema/migrations, repositories, FTS5, blob cache store
│   ├── Fbv.Indexing/             # import pipeline, file watcher, hashing, workers, enrichment
│   ├── Fbv.Search/               # query language parser, ranking, result streaming
│   ├── Fbv.Rendering/            # scene graph, tessellation, spatial index, themes (UI-toolkit-thin)
│   └── Fbv.App/                  # Avalonia app: Views/, ViewModels/, Services/, Docking/, Assets/
├── tests/
│   ├── Fbv.Parsers.Tests/        # golden-file corpus per format
│   ├── Fbv.Indexing.Tests/       # pipeline: dedupe, moves, corruption, cancellation
│   ├── Fbv.Search.Tests/         # query grammar + ranking + latency benchmarks
│   └── Fbv.Rendering.Tests/      # golden-image flip/mirror/rotate correctness
├── corpus/                       # redistributable/synthetic test boardviews only
└── tools/                        # corpus generator, index inspector CLI
```

## 4. Data architecture

Two stores, deliberately separated:

**(1) Library database — one SQLite file** (`library.db`, WAL mode). Source of truth for
everything *searchable and user-owned*. Survives cache deletion.

```sql
boards(board_id PK, sha256 UNIQUE, display_name, model, oem_code, format, format_version,
       part_count, net_count, pin_count, side_count, condition,        -- working|donor|stripped|unknown
       favorite, imported_at, indexed_at, index_version, user_notes)
board_files(file_id PK, board_id FK, abs_path, size, mtime, missing)   -- N paths per identical hash
parts(part_id PK, board_id FK, refdes, side, x, y, rotation, pin_count,
      value, part_number, package,                                     -- from file, nullable
      u_value, u_part_number, u_package, harvested)                    -- user enrichment overlay
nets(net_id PK, board_id FK, name, pin_count, is_power_hint)
pins(pin_id PK, board_id FK, part_id FK NULL, net_id FK, name, x, y, side)
part_aliases(alias, canonical)              -- marking codes, family equivalents
tags(board_id FK, tag)  ·  annotations(board_id FK, kind, anchor, text, created_at)
saved_searches(...)  ·  schema_meta(version)

-- Search index: contentless FTS5, one row per part and per net
CREATE VIRTUAL TABLE search_fts USING fts5(
    kind UNINDEXED, board_id UNINDEXED, row_id UNINDEXED,
    refdes, value, part_number, package, net, board_name, model, tag,
    tokenize = "unicode61 tokenchars '_-.+'", prefix = '2 3 4');
```

- Scale check: 5,000 boards × ~1,000 parts ≈ 5M part rows + ~15M pin rows ≈ 1.5–2.5 GB SQLite —
  well within its comfort zone. FTS5 prefix indexes give <10 ms prefix queries at this size.
  Pins are the bulky table; they're only *needed* for click-hit-testing and net centering, which
  the parsed cache also serves — if disk becomes a complaint, pin rows become an optional tier
  (config), with zero schema change elsewhere.
- Enrichment lives in `u_*` columns / separate rows, never overwritten by re-index (FR/NFR-9).
- All writes go through one writer connection (queue); readers use WAL snapshots — the search box
  never blocks on an import.

**(2) Parsed-board cache** (`cache/{sha256}.fbb`): the normalized `BoardModel` serialized in a
zero-copy format (FlatBuffers or equivalent), memory-mapped at open → FR-SRCH-5's < 300 ms open.
Disposable by design: deleting the cache folder loses nothing but re-parse time. Versioned header;
mismatch → lazy re-parse.

## 5. Indexing pipeline

```
[Watcher/Scanner] → discovery queue → [Hasher ×2] → dedupe check (sha256 in DB?)
   → parse queue → [Parser workers ×(cores−2), per-file try/catch + timeout]
   → BoardModel → [Extractor] → batched DB writer (single thread, 500-row txns)
                → [Cache writer] → {sha256}.fbb
   failures → quarantine table (path, reason, first-seen) → "Unreadable files" UI
```

- Producer/consumer via bounded channels; back-pressure keeps RAM flat during 10k-file imports.
- Cancellation-safe at file granularity; a killed app resumes by reconciling DB vs filesystem.
- Startup reconciliation: stat known paths (missing → badge), scan watched roots for new/changed
  (size+mtime fast path, hash to confirm), moved files re-attach by hash.
- Index schema evolution: `index_version` per board; a new app version re-indexes stale boards
  lazily in the background, oldest-opened-first.

## 6. Rendering pipeline

- **Retained scene per open board**: parse-once → tessellate outline/pads into vertex buffers /
  a recorded Skia picture (static layer). Dynamic layers stacked above: hover, selection, net
  halo, annotations, debug. Pan/zoom/flip/mirror are matrix-only operations — no re-tessellation.
- Frame budget: ~15k pads is a single draw batch territory; target headroom is 20× that (dense
  server boards) via instanced circles/rects and LOD (labels culled below pixel threshold).
- **Hit-testing**: per-board R-tree (or uniform grid — decided by benchmark) over pins and part
  bboxes, queried on hover (throttled) and click; independent of the GPU path.
- **Transform discipline** (the flip/mirror bug farm): one canonical world space ("viewed from
  top, Y-up"); view = `Projection × ViewportPan/Zoom × SideTransform(flip/mirror/rotate)`. All
  transforms in one tested module; golden-image tests lock handedness for all 16 combinations.
- Text: SDF/cached glyph atlas via the toolkit's text stack; labels drawn only above zoom
  thresholds (matches OBV/FlexBV behavior and keeps frames flat).

## 7. Search engine

- **Library search**: query string → tiny grammar (`field:term`, `*`, quotes, implicit AND) →
  FTS5 `MATCH` with prefix terms + alias expansion (query-time join on `part_aliases`) →
  rank: exact-field > prefix > substring; boosts for favorites/recents/condition=donor →
  stream first page immediately (keystroke-debounced ~60 ms, cancels superseded queries).
- **In-board search**: pure in-memory scan of the open `BoardModel` (thousands of rows — no index
  needed), shares the same query grammar.
- Both return the same `SearchHit` shape → one results panel, one activation path (FR-SRCH-5).

## 8. Cross-cutting decisions

- **Plugin parsers**: contract from formats doc §3. In v1 all parsers are in-repo assemblies
  discovered via registration; the boundary is kept clean so out-of-process/third-party loading
  can be added without core changes. Parser crashes/timeouts quarantine the file only (NFR-8).
- **Settings & layouts**: single JSON settings file + docking-layout file in the app-data dir,
  next to `library.db` and `cache/`. Everything relocatable for portable installs.
- **Error policy**: parsers return warnings, never throw across the boundary; every quarantined
  file is user-visible with a reason string. The app must be *boringly* stable — bench tools get
  one chance with technicians.
- **Testing strategy**: parser golden corpus; pipeline chaos tests (truncated/garbage/renamed
  files mid-import); search latency benchmarks in CI against a synthetic 5M-part library;
  golden-image rendering tests for orientation correctness.
- **ADRs**: every decision in this doc that gets approved becomes `docs/adr/NNNN-*.md`, starting
  with 0001-tech-stack.

## 9. Phasing (proposed)

- **M0 — skeleton**: app shell, docking, settings, empty library DB, CI.
- **M1 — viewer parity**: BRD/BDV/BVR/FZ parsers → render, navigate, flip/mirror, net highlight,
  in-board search, inspector. *Exit: daily-drivable as an OBV replacement.*
- **M2 — the library**: import pipeline, watcher, dedupe, library panel, board identity heuristics.
- **M3 — the differentiator**: FTS index, global search, open-and-center, cross-board net lookup,
  donor condition/harvested flags. *Exit: acceptance scenario in spec §5 passes.*
- **M4 — breadth & polish**: TVW/CAD/GenCAD/ASC/CST (+XZZ pending Q-6), enrichment/BOM import,
  aliases UI, saved searches, themes, keymap editor, packaging/installers.
- **Phase 2 candidates (gated on Q-3/Q-9/Q-5)**: PDF schematic sync, OBData measurements,
  Mycelium-style net expansion, shared/team library.
