# Market Research — Existing Boardview Software

> Phase-1 deliverable. Status: complete. See `05-open-questions.md` for decisions needed before implementation.

## 1. What a boardview is, and who uses it

A *boardview* file is a stripped-down representation of a PCB used for repair, not manufacturing:
board outline, component reference designators, pad/pin positions, and the net name attached to
each pin. It usually does **not** contain copper traces, inner layers, or full geometry. Repair
technicians use boardviews together with PDF schematics to:

1. Find a component physically ("where is U5300 on this board?").
2. Find everything on a net ("show me every pad on `PPBUS_G3H` so I can find the shorted cap").
3. Orient themselves under the microscope (flip/mirror the view to match how the board sits on the bench).
4. Compare measurements (diode-mode readings) against known-good values.

The files come from leaked factory test data (Test_Link/Landrex `.brd`), OEM tools (ASUS `.fz`,
Tebo `.tvw`), community conversions (`.bvr`), or commercial repair-content vendors (ZXW, Wuxinji,
XinZhiZao). Nearly every format in circulation was reverse-engineered by the community.

## 2. Tool-by-tool analysis

### 2.1 OpenBoardView (OBV) — the open-source baseline
- **Source**: https://github.com/OpenBoardView/OpenBoardView — MIT license, C/C++, SDL2 + Dear ImGui, CMake. Windows/macOS/Linux.
- **Positioning**: drop-in replacement for the ancient Landrex "Test_Link" factory tool.
- **Formats**: BRD (Test_Link), BRD2, BDV (Toptest), BV, BVR/BVR2/BVR3 (its own open text format), ASC, CAD, CST, F2B, FZ (ASUS — encrypted; the community extracted the key from the official viewer), and in 10.x: GenCAD, XZZ `.pcb`, ASRock PCBRepair Pro CAE.
- **Interaction model** (this is the muscle memory the industry has):
  - `Space` = flip board side, `M` = mirror across Y, `R` / `,` = rotate 90°
  - Mouse wheel = zoom at cursor, drag or `WASD` = pan, `-`/`=` = zoom
  - `Ctrl+F` or `/` = search, `K` = part list panel, `L` = net list panel, `P` = toggle pins
  - Click a pin/part → net-mates get a "halo" highlight across the whole board
- **Extras**: per-board annotations stored in a sidecar SQLite file, color themes, DPI scaling.
- **Limits**: one board per window (multiple instances allowed), no library concept, no schematic linking, no cross-board anything. Search is within the open board only.
- **Why it matters to us**: its parsers are MIT-licensed and are the de-facto public documentation
  of most formats. Its keybindings are the standard we should default to.

### 2.2 FlexBV5 — the professional benchmark (and closest competitor)
- **Source**: https://pldaniels.com/flexbv5/ — proprietary, by Paul Daniels ("inflex"). C + SDL3 + ImGui, pdfium PDF engine. Win/macOS/Linux. Free tier (legacy formats) / Professional US$150 perpetual.
- **Killer feature**: boardview ⇄ PDF schematic synchronization. Click a pin on the board → jump to that net on the schematic page, and back. This is why professionals (Rossmann et al.) pay for it.
- **Net tools**: "Constellation view" (minimal spanning-tree of a net), "Netweb" (pad-to-pad fan lines), "Mycelium" expansion — follows a net *through* 0-ohm resistors, fuses, and inductors up to 3 levels. Excellent for power-rail tracing.
- **Formats**: 15+ — legacy set (BRD/BDV/BV/GR), OEM (FZ, Samsung CAD, Teboview TVW, XZZ), manufacturing (ODB++, GenCAD, Fabmaster), modern EDA (KiCad, EasyEDA Pro, Eagle, Allegro binary beta).
- **OpenBoardData (OBData)**: crowdsourced known-good diode/voltage measurements per board, shown next to pins.
- **Library / donor search — the part we must beat** (verified against the manual):
  - The *Library* is a **filename cache**: it recursively scans user-chosen folders and lets you open files by partial *filename* match from the File menu. Nothing inside the files is indexed.
  - *Part Find* is a **PDF text scan across a folder of schematics** (with compound criteria like "1uF 16V 0201" within a radius on the page). It does not search boardview component data at all; the manual itself notes "there's no way of correlating the respective board part number from this information" — you land on a schematic hit and must manually cross-reference into the boardview.
  - So: no structured index of parts/nets across boards, no instant-as-you-type results, no
    "click result → board opens centered on the component". **The core feature of this project
    does not exist in FlexBV in the form we intend.**

### 2.3 BoardViewer (boardviewer.net)
- Freeware Windows tool of Chinese origin, notable for the **broadest format support**:
  XZZ `.pcb`, TVW, BRD, BDV, GR, CST, ASC, BVR/BVR2/BVRE, CAD, FZ, FAB, HYP (HyperLynx),
  ODB++, GenCAD, Altium/Protel.
- Single-board viewer; conventional feature set (search, net highlight, flip). No library/database.
- Often the tool techs reach for when a file won't open elsewhere.

### 2.4 ZXW (Zillion x Work), Wuxinji, XinZhiZao — the phone-repair content platforms
- These are **content subscriptions, not general viewers** (~US$50/yr, dongle- or account-locked):
  the vendor supplies its own encrypted drawing library (bitmap "point maps" + vector boardviews +
  schematics) for phones/tablets, continuously updated per new device model.
- Workflow innovations worth copying: integrated schematic + board bitmaps per model, per-pin
  diode-mode readings and voltages displayed in-place, part-number popup with function description,
  built-in repair notes/case libraries.
- Their formats are DRM'd subscription content; XinZhiZao's `.pcb` encryption was cracked by the
  community (GBAtemp), but shipping a decryptor for a live commercial service is a legal risk —
  see `02-file-formats.md` §4.

### 2.5 Others
- **Phoneboard** (phoneboard.co): free phone-repair viewer (`.bvr` ecosystem), click-pin→schematic
  linking, side-by-side board comparison. Windows/macOS/Linux.
- **NextBV, boardviewer.app, BoardView Trace**: new browser-based viewers; org accounts, team
  sharing of files. Signal: the market is moving toward *library/team* features, but nobody has
  a deep local index, and browser delivery conflicts with repair-shop offline/latency needs.
- **Landrex Test_Link**: the ancient factory tool `.brd` came from; only relevant as the origin of
  UI conventions and the format.
- **Tebo ICT / Teboview**: Lenovo-ecosystem viewer; `.tvw` was reverse-engineered by the FlexBV
  author (github.com/inflex/teboviewformat).
- **"BoardMaster"**: no widely-known boardview product by this exact name was found; closest hits
  are generic listicles. Assumed to refer to the general category. (Flagged in open questions.)

## 3. Industry-standard behavior (what techs' hands already know)

| Action | Convention (OBV/FlexBV lineage) |
|---|---|
| Flip to other board side | `Space` (or middle-click in FlexBV) |
| Mirror view (view board "from the back") | `M` |
| Rotate 90° | `R`, `<` / `>` |
| Zoom | wheel, centered on cursor; `-`/`=` |
| Pan | left-drag and/or `WASD`/arrows |
| Search | `Ctrl+F` and `/`, incremental, matches refdes and net |
| Part / net list panels | `K` / `L` |
| Select pin or part | left-click → highlight the whole net everywhere ("halo") |
| Annotations | right-click → note pinned to part/net/location |
| Theme | dark background, bright net highlight (yellow/white), red selection |

Non-negotiables distilled from how these tools are used:
1. **Net highlight is the product.** Techs live in "click pad → see every net-mate light up".
   It must be instant and visually unmissable, including net-mates on the hidden side (shown when flipping).
2. **Flip and mirror must be exact and fast** — the tech is matching the screen to a physical board
   under a microscope, sometimes upside down. Any lag or wrong-handedness destroys trust.
3. **Search must be incremental** (results narrow per keystroke) and forgiving (case-insensitive,
   substring, `PPBUS` finds `PPBUS_G3H`).
4. **Files are messy.** Real-world libraries are folders of rar'd leaks with inconsistent names.
   Import must tolerate garbage, duplicates, and encrypted files it can't read, without stalling.
5. **Offline is mandatory.** Shops run these tools on benches with no/blocked internet, often on
   modest Windows machines.

## 4. Gap analysis — where FlexibleBoardViewer wins

| Capability | OBV | FlexBV5 Pro | ZXW/XZZ | **FlexibleBoardViewer (target)** |
|---|---|---|---|---|
| Multi-format viewer | ✅ | ✅ | own content only | ✅ (parity set) |
| Net highlight / tracing | ✅ basic | ✅ best-in-class | ✅ | ✅ (parity + Mycelium-style later) |
| Schematic PDF link | ❌ | ✅ | ✅ | phase 2 (open question) |
| Multiple boards open (tabs/docking) | ❌ | ❌ (windows) | ❌ | ✅ |
| **Structured index of every part/net in every file** | ❌ | ❌ (filename cache + PDF grep) | ❌ | ✅ **core feature** |
| **Instant cross-board search → open & center** | ❌ | ❌ | ❌ | ✅ **core feature** |
| Donor workflow (which of *my* boards carries ISL9239?) | manual | semi-manual, PDF-based | n/a | ✅ first-class |
| Known-good measurements | ❌ | ✅ OBData | ✅ | phase 2 (OBData is an open format — adoptable) |

**The one honest caveat**: the most common format (Test_Link `.brd`) often carries *no component
values or part numbers* — only refdes + net + coordinates. Cross-board search by part number
(e.g. `TPS51225`) will only hit boards whose files carry that data (FZ, TVW, XZZ, CAD/EDA formats)
or that the user/community has enriched. The spec addresses this with a metadata-enrichment layer
(see `03-specification.md` FR-IDX-7). Search by refdes and net name works on **all** formats.
