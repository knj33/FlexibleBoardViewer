# Boardview File Formats — Landscape, Parsing Strategy, Legal Stance

> Phase-1 deliverable. Companion to `01-market-research.md`.

## 1. Format inventory

Grouped by how much data they give the donor-search index. "Values/PN" = component values and/or
manufacturer part numbers present in the file.

| Format | Ext | Origin | Type | Outline | Parts+pins | Nets | Values/PN | Parsing difficulty |
|---|---|---|---|---|---|---|---|---|
| Test_Link / Landrex | `.brd` | factory test (Apple boards etc.) | binary+text hybrid | ✅ | ✅ | ✅ | ❌ | Low — OBV parser exists |
| BRD2 | `.brd` | variant | text | ✅ | ✅ | ✅ | ❌ | Low (OBV) |
| Toptest | `.bdv` | factory test | text | ✅ | ✅ | ✅ | ❌ | Low (OBV) |
| BV / GR | `.bv` `.gr` | legacy Access-DB dumps | text | ✅ | ✅ | ✅ | ❌ | Low (OBV) |
| Boardview Raw | `.bvr`/`.bvr2`/`.bvr3` | open community format (inflex) | text | ✅ | ✅ | ✅ | partial | Low — documented |
| ASUS FZ | `.fz` | ASUS OEM | encrypted+compressed | ✅ | ✅ | ✅ | ✅ | Medium — key is public in OBV source |
| Teboview | `.tvw` | Tebo ICT (Lenovo ecosystem) | binary | ✅ | ✅ | ✅ | ✅ | Medium — inflex/teboviewformat RE notes |
| XZZ / XinZhiZao | `.pcb` | subscription vendor | encrypted | ✅ | ✅ | ✅ | ✅ | Medium tech / **high legal risk** (§4) |
| Samsung CAD | `.cad` | Samsung OEM | text | ✅ | ✅ | ✅ | ❌ | Low (OBV) |
| GenCAD | `.cad` | industry standard | text, documented | ✅ | ✅ | ✅ | ✅ | Medium — real spec exists |
| CST | `.cst` | CST BoardViewer | text | ✅ | ✅ | ✅ | ❌ | Low (OBV) |
| F2B, FAB (Fabmaster) | `.f2b` `.fab` | factory CAM | text | ✅ | ✅ | ✅ | partial | Medium |
| PADS ASCII | `.asc` | PADS exports | text | ✅ | ✅ | ✅ | partial | Medium |
| KiCad | `.kicad_pcb` | open EDA | text (s-expr) | ✅ | ✅ | ✅ | ✅ | Medium — full EDA data, subset needed |
| EasyEDA Pro | `.epro` | open-ish EDA | zip/json | ✅ | ✅ | ✅ | ✅ | Medium |
| ODB++ | dir/`.tgz` | manufacturing standard | structured tree | ✅ | ✅ | ✅ | ✅ | High — large spec, phase 2+ |
| Allegro binary | `.brd` (Cadence) | EDA | proprietary binary | ✅ | ✅ | ✅ | ✅ | Very high — even FlexBV marks it beta; out of v1 |
| ZXW / Wuxinji drawings | various | subscription vendors | DRM content | — | — | — | — | **Out of scope** (§4) |

Notes:
- `.brd` is claimed by three unrelated formats (Test_Link, EAGLE, Allegro) and `.cad` by two
  (Samsung, GenCAD) — **detection must be content-based, never extension-based.** OBV's approach
  (each parser reports a confidence score for a byte-buffer, highest wins) is the right one.
- File sizes are small: 100 KB – 5 MB. Whole-file in-memory parsing is fine; the performance
  problem is *volume* (thousands of files), not file size.

## 2. Canonical internal model (the IR every parser targets)

All parsers normalize into one `BoardModel`; the renderer, index, and UI never see format-specific
structures. This is what makes the parser layer a plugin architecture instead of N viewers.

```
BoardModel
├─ meta:      source path, format id, format version, sha256, parse warnings
├─ identity:  display name, model hints (from file and filename), OEM code guesses
├─ outline:   polyline segments / closed polygons (per side if format splits)
├─ parts[]:   refdes, side (TOP|BOTTOM), centroid x/y, rotation, bbox,
│             value?, part_number?, package?, footprint?, pin_count
├─ pins[]:    part_ref?, pin name/number, x/y, side, radius/shape, net_ref, test_point?
├─ nets[]:    name, pin refs, class hints (power/gnd heuristic)
└─ units:     normalized to fixed internal unit (nm or 0.01 mil), Y-up, origin bottom-left
```

Normalization rules that bite in practice (each parser must handle):
- **Units** differ (mils, mm, 0.1 mil ints) → convert once at parse time.
- **Y-axis and mirroring** differ (some formats store the bottom side pre-mirrored) → normalize to
  "as viewed from top", let the renderer apply flip/mirror.
- **Two-sided files vs pre-split "butterfly" files** (XZZ ships boards pre-split side-by-side) →
  detect and split into logical sides.
- **Net name junk**: `NC`, `NOCONNECT`, `GND` variants, per-format dummy nets → canonical
  no-connect marker; keep original string for display.

## 3. Parser plugin architecture (requirement, not implementation)

- Contract: `detect(bytes) -> confidence 0..1` + `parse(bytes, context) -> BoardModel + warnings`.
- Parsers are pure and sandbox-friendly: no UI, no filesystem access beyond the given buffer
  (companion-file lookups go through the context object) — so the indexer can run them on worker
  threads, and a crashing parser fails one file, not the app.
- Built-in parsers ship in-process; the contract is stable so third-party parsers can be added
  (dynamic load — mechanism per final tech stack) without touching core.
- Every parser carries a golden-file test corpus (real files where redistributable, synthesized
  fixtures where not).

**Reuse decision to make** (open question Q-7): OpenBoardView's parsers are MIT-licensed — legally
reusable even in a proprietary app (attribution required). Options: (a) port them to our language
(they are mostly straightforward tokenizers; porting also gives us tests and full control), or
(b) wrap the C code behind FFI. Recommendation: **port**, format by format, validating output
against OBV rendering of the same file.

## 4. Legal / ethical stance on encrypted formats

- **ASUS FZ**: decryption keys have shipped in open-source OBV for years without incident; the
  format serves files users already legitimately hold. **Include.** (Ship keys the way OBV does;
  document provenance.)
- **XZZ `.pcb`**: files circulate widely and OBV 10.x added support, but XinZhiZao is a *live
  subscription business* and their files are DRM'd content — a commercial product shipping a
  decryptor invites DMCA §1201 / equivalent claims. **Decision needed** (Q-6): follow OBV precedent
  vs. exclude vs. "reads only files the community has already converted".
- **ZXW / Wuxinji**: pure DRM'd content platforms; supporting their formats means breaking their
  dongle DRM. **Excluded.** Their *content* is not portable; their *workflow ideas* (measurements
  in-place, part info popups) are fair game to reimplement.
- We never bundle or distribute boardview files themselves. The user brings their own library; we
  index it. This keeps the product clear of the murky provenance of the files.

## 5. Metadata reality check (drives the enrichment feature)

For the donor-search index, per-field availability across a typical technician library
(mostly `.brd`/`.bdv` Apple + `.fz` ASUS + `.tvw` Lenovo + misc):

| Field | Availability | Consequence |
|---|---|---|
| refdes, pins, nets, coordinates | ~100% of parseable files | refdes/net search works everywhere — the headline demo (`U5300`, `PPBUS_G3H`) is safe |
| package/footprint | ~40–60% | show when present, searchable, never required |
| component value | ~30–50% | same |
| manufacturer part number (`TPS51225`) | ~20–40% | **needs enrichment layer**: user/community BOM sidecars, per-board annotations, and a part-alias table (marking codes ↔ full PN) to make PN search useful |
| board model/name | rarely inside the file | **derive from filename/path**: OEM code patterns are strong signals — Apple `820-xxxxx[-A]`, Compal `LA-xxxxx`, Quanta `DA0…/DAx…`, Wistron/Inventec codes, Lenovo `NM-xxxxx`; plus the containing folder name. Must stay user-overridable |
