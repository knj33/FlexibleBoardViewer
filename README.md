# FlexibleBoardViewer

A professional boardview application for electronics repair technicians that doubles as a
**searchable donor-board database**: import your whole library of boardview files, and one search
(`U5300`, `PPBUS_G3H`, `TPS51225`, …) queries every board at once — click a hit and that board
opens centered on the component.

## Status

**Design phase — no code yet.** Research, draft specification, and architecture proposal are
complete and awaiting review/approval:

| Doc | Contents |
|---|---|
| [docs/01-market-research.md](docs/01-market-research.md) | Analysis of OpenBoardView, FlexBV5, BoardViewer, ZXW/Wuxinji/XZZ, etc.; industry-standard behavior; competitive gap analysis |
| [docs/02-file-formats.md](docs/02-file-formats.md) | Boardview format landscape, parser plugin strategy, legal stance on encrypted formats |
| [docs/03-specification.md](docs/03-specification.md) | Draft functional + non-functional specification (v1 scope, acceptance scenario) |
| [docs/04-architecture.md](docs/04-architecture.md) | Tech-stack candidates & recommendation, data/index/rendering/search architecture, milestones |
| [docs/05-open-questions.md](docs/05-open-questions.md) | **Decisions needed before implementation starts** |
