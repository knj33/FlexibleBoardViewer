# ADR-0001: Technology stack — Rust + egui/eframe + rusqlite

Date: 2026-07-04 · Status: **accepted**

## Context

The architecture proposal (docs/04) compared C++/Qt, C#/Avalonia and Rust,
and initially recommended C#/Avalonia for velocity. The owner's decision
criteria (Q-7) were explicit: **fastest, best RAM usage, safe, and scalable**
for later phases — with Windows-only as the initial target (Q-1) and open
source, personal use as the model (Q-2).

## Decision

Rust, with:

- **eframe/egui** for UI + canvas. Immediate-mode GUI is the established
  idiom of this niche (OpenBoardView and FlexBV are both Dear ImGui
  applications; egui is Rust's ImGui counterpart), and the board canvas —
  the heart of the app — is a natural immediate-mode drawing problem.
- **rusqlite (bundled SQLite, FTS5)** for the library database and search
  index; verified working in CI tests.
- **bincode** blob cache for parsed boards; **crossbeam-channel** +
  scoped threads for the import pipeline; **flate2**/**des** for the FZ/XZZ
  codecs; **rfd** for native file dialogs.

By the stated criteria: C++ fails "safe"; C#/GC loses on RAM and startup;
Rust matches C++ on speed/RAM while being memory-safe, which also directly
serves NFR-8 ("a malformed file may never crash the app").

## Consequences

- The MVVM framing in docs/04 §2 becomes an immediate-mode equivalent:
  central `App` state struct + per-panel functions; the layered crate
  architecture (core / parsers / data / index / search / app) is unchanged.
- No real docking in v1 (egui_dock deferred); fixed side panels + board
  tabs, which matches what OBV/FlexBV users have anyway.
- Parsers were ported (not FFI-wrapped) from OpenBoardView's MIT sources,
  with synthetic golden-file tests per format; see NOTICE for attribution.
- Windows is the target; the code stays cross-platform because nothing in
  the stack is Windows-specific (free portability, no extra cost).
