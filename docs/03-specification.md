# FlexibleBoardViewer — Software Specification

> **Status: ACCEPTED 2026-07-04.** All `[Q-n]` markers are resolved in
> `05-open-questions.md` (decision table at the top). Headline changes from
> the draft: Windows-only target, open source, XZZ format included,
> net expansion (FR-VIEW-5 phase-2 item) pulled INTO v1, PDF schematic sync
> stays phase 2, no enrichment sharing, single-user local library.

## 1. Product definition

A professional desktop boardview application for electronics repair technicians that is
simultaneously: (a) a full-featured, standards-conforming boardviewer, and (b) a **searchable
donor-board database** — every imported file's parts and nets are indexed, and one search box
queries the entire library instantly, opening any hit centered on the exact component.

**Personas**
- *P1 — Board repair tech (primary)*: fixes MacBooks/laptops/consoles daily. Knows OBV/FlexBV
  muscle memory. Has 200–5,000 boardview files in messy folders and a shelf of donor boards.
- *P2 — Shop owner / senior tech*: same, plus cares about the shop's shared library, donor stock,
  and onboarding juniors. `[Q-5: multi-user scope]`
- *P3 — Microsoldering hobbyist*: smaller library, free-tier expectations. `[Q-2: business model]`

**Top jobs-to-be-done**
1. "Board dead on `PPBUS_G3H`. Show me every pad on that rail. Now flip the board." (viewer parity)
2. "I need an ISL9239 / a U5300 donor. **Which boards on my shelf have one?**" (core feature)
3. "Center this cap in view, mirrored, so it matches what's under my microscope." (viewer parity)

## 2. Functional requirements

### 2.1 Board viewer (module VIEW)
- **FR-VIEW-1** Render a `BoardModel`: outline, parts, pads/pins, test points, net labels; dark theme default. 60 fps interaction on integrated graphics.
- **FR-VIEW-2** Navigation: wheel zoom at cursor (10,000:1 range), drag + keyboard pan, rotate 90° steps, fit-to-screen, `Space` flip side, `M` mirror, remembered per-tab view state. Default keymap = OpenBoardView conventions (see research §3); fully remappable.
- **FR-VIEW-3** Side handling: top/bottom rendering with hidden-side ghosting option; correct handedness under every flip/mirror/rotate combination (golden-image tests).
- **FR-VIEW-4** Selection: click pin → select pin + its net; click part → select part; hover tooltips (refdes, net). Esc clears. Selection history back/forward.
- **FR-VIEW-5** Net highlight: selecting a net highlights *all* member pins on both sides ("halo"), with count in status bar; optional pad-to-pad web lines (FlexBV "netweb" style). Phase 2: net expansion through 0R/fuse/inductor (Mycelium-style) `[Q-9]`.
- **FR-VIEW-6** Component inspector panel: refdes, value, part number, package/footprint, side, x/y, rotation, pin table (pin → net, click-to-highlight), source-format extras. Copy-all button.
- **FR-VIEW-7** In-board search: incremental, case-insensitive substring over refdes/net/value/PN; result list + cycle-through; `Ctrl+F` and `/`.
- **FR-VIEW-8** Multi-board: dockable tabs; ≥10 boards open without degradation; per-tab state; split view (two boards side by side) for donor-vs-patient comparison.
- **FR-VIEW-9** Annotations: per-board notes pinned to part/net/point; stored in the app database, never modifying source files; exportable. `[Q-10: OBV sidecar-format compatibility]`

### 2.2 Library & import (module LIB / IDX)
- **FR-LIB-1** Library panel: tree/list of imported boards with name, model, format, part/net counts, tags, favorite pin, missing-file badge; recents; filter box.
- **FR-IDX-1** Import sources: add folders (watched, recursive) and individual files; drag-and-drop anywhere. Non-blocking: UI stays fully responsive during import of thousands of files.
- **FR-IDX-2** Pipeline per file: hash → dedupe → content-based format detection (confidence-scored) → parse → normalize → index metadata (parts/nets/pins) → write parsed cache blob. Unparseable/encrypted files are recorded and listed (with reason), never retried in a loop, never crash the app.
- **FR-IDX-3** Incremental: filesystem watcher + startup reconciliation (new/changed/moved/deleted; moved files re-matched by hash). Re-index only what changed; index schema version bump triggers lazy background re-index.
- **FR-IDX-4** Throughput target: ≥ 20 files/s sustained bulk import on a mid-range machine (parallel workers); progress UI with per-file failures visible, cancel/pause.
- **FR-IDX-5** Board identity: auto-derive display name + model from filename/path patterns (Apple `820-xxxxx`, Compal `LA-`, Quanta `DA…`, Lenovo `NM-` …) and in-file hints; always user-editable; user metadata survives re-index.
- **FR-IDX-6** Dedup: identical-hash files appear as one board with N locations; near-duplicate (same model, different revision) grouped but distinct.
- **FR-IDX-7** Enrichment: attach BOM/CSV sidecars mapping refdes → value/PN/package; per-part user edits; global part-alias table (e.g. marking code ↔ `TPS51225`) applied at query time. `[Q-8: community-shared enrichment]`
- **FR-IDX-8** Donor status (differentiator, small v1 slice): per-board condition tag (working / donor / stripped) and per-part "harvested" flag shown in search results — so results say *"U5300 on 820-00281 (donor #3, still present)"*. Full inventory management is out of v1. `[Q-4]`

### 2.3 Cross-board search (module SEARCH — the core feature)
- **FR-SRCH-1** One global search box (always visible, `Ctrl+Shift+F` from anywhere) querying the whole library.
- **FR-SRCH-2** Searchable fields: refdes, net name, value, part number (incl. aliases), package/footprint, board name/model, tags. Bare terms search all fields; prefixes narrow: `ref:U5300 net:PPBUS* val:1uF pkg:0201 board:820-00281 pn:ISL9239`. Wildcards `*`, quoted phrases.
- **FR-SRCH-3** Latency: first results < 50 ms per keystroke against 5,000 boards / ~5M part rows; results stream in ranked (exact refdes > prefix > substring; user's favorite/recent boards boosted).
- **FR-SRCH-4** Result rows show: board name + model, hit type (part/net), refdes, value, PN, package, side, coordinates, board condition/harvested status. Grouped by board with per-board hit counts; virtualized list for 100k+ hits.
- **FR-SRCH-5** Activation: click/Enter opens the board (from parsed cache, < 300 ms) in a tab, centers and zooms on the component (or fits the net's bounding box, flashing the highlight), on the correct side.
- **FR-SRCH-6** Cross-board net lookup: searching `PPBUS_G3H` lists every board containing that net (with pin counts) — this is how techs find *comparison* boards for measurements, not just donors.
- **FR-SRCH-7** Saved searches + search history.

### 2.4 Application shell (module APP)
- **FR-APP-1** Dockable panels: Library, Search results, Component inspector, Net list, Part list, Annotations, Import status; layouts save/restore; sensible default layout.
- **FR-APP-2** Status bar: cursor position (board units), current side/rotation/mirror state, selected net + pin count, index status (N boards / M parts, indexing spinner).
- **FR-APP-3** Dark theme default + light theme; UI scale for HiDPI; color-blind-safe highlight palette option (bench reality: many techs are color-blind).
- **FR-APP-4** Command palette (`Ctrl+K`) exposing every action; full keyboard operability.
- **FR-APP-5** Crash safety: open-tab/session restore; the library DB is never corrupted by a crash (WAL); telemetry **opt-in only**, offline-safe. `[Q-2]`

## 3. Non-functional requirements

| # | Requirement | Target |
|---|---|---|
| NFR-1 | Cold start to interactive | < 2 s (library of 5,000 boards) |
| NFR-2 | Open board from cache | < 300 ms; from cold parse < 1.5 s typical file |
| NFR-3 | Search keystroke → results | < 50 ms @ 5M part rows |
| NFR-4 | RAM | < 500 MB with 10 boards open + full index; index is disk-backed, not in-RAM |
| NFR-5 | Disk | metadata+cache ≤ ~2× source library size |
| NFR-6 | Fully offline | all features work with zero network access |
| NFR-7 | Platforms | Windows 10+ x64 first-class; macOS/Linux `[Q-1]` |
| NFR-8 | Robustness | malformed file can never crash the app or poison the index |
| NFR-9 | Data safety | user metadata (names, tags, annotations, enrichment) survives re-index, file moves, and app updates; export/backup of the whole library DB |

## 4. Explicit non-goals (v1)

- PCB *editing* of any kind; Gerber/manufacturing output.
- Trace/copper rendering beyond what source formats provide (most boardview formats have no traces).
- Breaking ZXW/Wuxinji DRM (see formats doc §4). XZZ stance pending `[Q-6]`.
- PDF schematic sync (FlexBV's flagship) — phase 2 candidate, decision pending `[Q-3]`.
- Cloud sync / team server `[Q-5]`; mobile/web clients.
- Full donor *inventory/ERP* (bins, pricing, sales) — only the FR-IDX-8 slice.

## 5. Acceptance scenario (the demo that defines "done" for v1)

1. Fresh install. Drag a folder with 3,000 mixed files (`.brd`, `.bdv`, `.fz`, `.tvw`, `.cad`, junk).
2. UI stays responsive; within ~3 minutes the status bar shows ~2,900 boards indexed, 100 listed as unreadable with reasons.
3. Type `ISL9239` → under 50 ms, grouped hits across 14 boards appear, including alias hits (`U7090`, marking-code enriched).
4. Click the hit on "820-00840 (donor, shelf B)" → board opens in < 300 ms, view centered and zoomed on U7090, bottom side auto-selected, part flashing.
5. Click a pin of U7090 → `PPBUS_G3H` halo lights across the board; press `Space` — board flips, highlight persists; press `M` — mirrors to match the physical board on the bench.
6. Global search `net:PPBUS_G3H` → every board with that rail, with pin counts, for known-good comparison.
