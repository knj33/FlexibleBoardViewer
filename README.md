# FlexibleBoardViewer

A boardview application for electronics repair that doubles as a
**searchable donor-board database**: import your whole library of boardview
files, and one search (`U5300`, `PPBUS_G3H`, `TPS51225`, `net:PP3V3*`,
`pn:ISL9239`) queries every board at once — click a hit and that board opens
centered on the component, on the correct side, with its net highlighted.

Open source (MIT), written in Rust with egui. Windows is the primary target;
the code also builds on Linux/macOS.

## Features (v1, implemented)

- **Viewer**: smooth zoom-at-cursor, pan, rotate (`R`), side flip (`Space`),
  mirror (`M`), fit (`F`), net halo highlighting, hover tooltips, component
  inspector with pin→net table, refdes labels, OpenBoardView-style shortcuts.
- **Net expansion** (`X` or slider): follows a highlighted rail through
  0-ohm resistors, inductors, beads and fuses (R/L/F/FB/FL/PR/PL/PF/JP),
  up to 3 levels, color-coded — without flooding through capacitors or GND.
- **Library**: background import pipeline (parallel parse workers, SHA-256
  dedupe, incremental rescans), board identity guessed from OEM filename
  patterns (Apple `820-xxxxx`, Compal `LA-`, Lenovo `NM-`, Quanta `DA0…`),
  condition tags (working/donor/stripped), favorites, per-part
  **harvested** flags, quarantine list for unreadable files.
- **Cross-board search**: SQLite FTS5 index over every part and net of every
  imported board; field filters (`ref:`, `net:`, `pn:`, `val:`, `pkg:`,
  `board:`, `model:`), prefix matching, instant results; click-to-open-and-center.
- **Formats**: Test_Link BRD (plain + obfuscated), BRD2, Toptest BDV
  (plain + obfuscated), BVR v1/v3, PADS ASC (directory), Samsung CAD, CST,
  ASUS FZ (RC6+zlib), XZZ PCB (XOR+DES). Content-based detection —
  extensions are not trusted. Encrypted formats need user-supplied keys in
  Settings (same values OpenBoardView users configure); no keys ship in
  this repository.

## Build & run

```
cargo build --release
target/release/flexibleboardviewer     # .exe on Windows
```

Requires stable Rust (2021 edition). No system dependencies: SQLite is
bundled. Library data lives in the platform data dir
(`%APPDATA%/FlexibleBoardViewer` on Windows): `settings.json`, `library.db`,
and a disposable `cache/` of parsed boards.

Tests: `cargo test --workspace` (48 tests: parser golden files incl.
encrypted FZ/XZZ fixtures, pipeline chaos tests, FTS injection tests,
orientation round-trip tests).

## Workspace layout

| Crate | Role |
|---|---|
| `fbv-core` | normalized `BoardModel` IR, geometry, net/identity heuristics |
| `fbv-parsers` | format detection + 10 parsers (ported from OpenBoardView, MIT — see NOTICE) |
| `fbv-data` | SQLite schema + FTS5 index + parsed-board blob cache |
| `fbv-index` | parallel import pipeline, dedupe, quarantine, reconciliation |
| `fbv-search` | query grammar → FTS5, ranking, in-board search |
| `fbv-app` | egui application: canvas, panels, tabs, settings |

## Design documents

Research, specification, architecture and decisions: [docs/](docs/) —
start with [docs/01-market-research.md](docs/01-market-research.md) and
[docs/adr/0001-tech-stack.md](docs/adr/0001-tech-stack.md). Decisions log:
[docs/05-open-questions.md](docs/05-open-questions.md).

## Roadmap (phase 2 candidates)

PDF schematic sync, OpenBoardData measurements, filesystem watcher for
live incremental indexing, docking layout, OBV annotation import, more
formats (GenCAD, Teboview TVW, F2B, Fabmaster).
