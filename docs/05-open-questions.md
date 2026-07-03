# Open Questions — Answers Needed Before Implementation

Each question lists my recommendation so you can reply tersely ("Q-1: agree, Q-6: exclude, …").
Q-1, Q-2, Q-6 and Q-7 gate the architecture; the rest gate scope/priorities.

---

**Q-1 · Platforms.** Windows-first is assumed (repair shops are overwhelmingly Windows). Are macOS
and Linux required at v1, later, or never? *Recommendation: build cross-platform from day one
(costs little with stack B), ship Windows installers first.*

**Q-2 · Business model & source model.** Commercial (FlexBV-style perpetual license ~$150), freemium
(free viewer + paid library/search), or open source? This decides license-key infrastructure,
telemetry policy, whether we can vendor GPL code (currently we only rely on MIT code, so all doors
are open), and how much polish v1 needs. *Recommendation: freemium — free full viewer builds the
user base OBV-style; the donor-search library is the paid tier.*

**Q-3 · PDF schematic sync.** FlexBV's flagship (board ⇄ schematic click-sync) is a big, separate
subsystem (PDF rendering, text extraction, net-name matching). In v1, in phase 2, or out?
*Recommendation: phase 2. V1 wins on the donor search; shipping a worse clone of FlexBV's PDF sync
in v1 dilutes both.*

**Q-4 · Donor inventory depth.** Spec includes only condition tags + per-part "harvested" flags
(FR-IDX-8). Do you want real inventory (shelf/bin locations, quantities, harvest history, maybe
pricing) in v1? *Recommendation: keep the v1 slice; design the schema so inventory can grow.*

**Q-5 · Single-user or shop-shared library?** Local single-user DB is assumed. Do shops need a
shared library (several benches, one index) in v1 — via a shared network folder, or a real
server? This materially changes the data layer. *Recommendation: v1 single-user local; make the
library DB relocatable so "shared folder, one writer" works informally; real multi-user is phase 2.*

**Q-6 · XZZ (`.pcb`) format stance.** OpenBoardView ships support; XinZhiZao is a live subscription
business and their files are DRM'd. Options: (a) support like OBV does, (b) exclude entirely,
(c) support only community-converted derivatives. This is a legal-risk-appetite call that only you
can make — especially if the product is commercial. *Recommendation if commercial: (c) at launch,
revisit with legal advice; if open source: (a) has OBV precedent.*

**Q-7 · Tech stack.** Architecture doc recommends **C#/.NET + Avalonia + SkiaSharp** and keeps
Qt/C++ as the fallback. Two things decide it: What is *your* language background (who maintains
this)? And do you have any hard requirement (existing code, team, employer constraints) pushing
C++/Qt or something else? *Recommendation: stack B unless you're personally a C++ developer.*

**Q-8 · Enrichment data sharing.** Part-number enrichment (BOMs, marking-code aliases) is what makes
`TPS51225`-style searches useful on value-less formats. Should users be able to export/import
enrichment packs to share with the community (a differentiator, but creates an implicit format we
must maintain)? *Recommendation: yes, as a simple versioned JSON pack; no cloud service in v1.*

**Q-9 · Net-expansion tracing.** FlexBV's Mycelium (follow a rail through 0R/fuses/coils) is the
best diagnostic aid in the market. V1 parity feature or phase 2? *Recommendation: phase 2, but the
net model in Core is designed for it now (cheap to prepare, expensive to retrofit).*

**Q-10 · OBV annotation compatibility.** OpenBoardView users have per-board sidecar annotation DBs.
Import them (one-way) so switchers keep their notes? *Recommendation: yes in M4 — cheap goodwill.*

**Q-11 · "BoardMaster".** Your brief listed it; I found no distinct boardview product under that
exact name (only generic listicles). Did you mean a specific regional tool (link/copy welcome), or
the general category? If it's a tool you use, I'll analyze it before the spec is finalized.

**Q-12 · Your library today.** To size things honestly: roughly how many files, which formats
dominate (`.brd`? `.fz`? `.tvw`?), typical folder layout, and are they on local disk or NAS?
If you can share a handful of representative *non-sensitive* files (or even just extensions +
sizes), the parser corpus and the import heuristics start from reality instead of guesses.

**Q-13 · Name check.** Working name "FlexibleBoardViewer" collides half with "FlexBV" (established,
same niche, trademark-ish gray zone). Keep, or rename before anything ships publicly?
*Recommendation: rename; happy to shortlist options.*
