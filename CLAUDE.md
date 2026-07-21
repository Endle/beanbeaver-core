# beanbeaver-core

Portable, permissive (MIT) Rust crates shared by the desktop, iOS, and Android
apps. Cross-repo rules (the license firewall, core-tag pinning) live in the
umbrella `../CLAUDE.md`; repo detail (crates, tests, model fetch) is in
`README.md` / `docs/`. This file owns one thing: **what is allowed to live here.**

## Charter — this repo parses OCR output, nothing else

beanbeaver-core exists to do **exactly one job: turn OCR output (bounding boxes +
text) into itemized receipt details as structured data / beancount *text*.**
bbox in → itemized JSON out. `receipt-core` is that job; the model-free
`parse_detections` FFI entry point is its front door.

**This is a hard boundary, not a style preference.** Do **not** grow this repo
beyond that parse step. Specifically, none of the following ever belong here, no
matter how convenient it seems in the moment:

- **UI** of any kind.
- **Linking** beancount (core only ever *emits* beancount **text** — see the
  umbrella `CLAUDE.md` license firewall).
- **Bank-statement importers** or **receipt↔transaction matching** (those are the
  desktop app's, `src/match_*.rs`).
- **New** image-capture, image-preprocessing, or OCR-*inference* responsibility.
  Core consumes detections; producing them is the consumer's problem.

If a change needs any of the above, it belongs in a **consumer**
(`beanbeaver/`, `beanbeaver-ios/`, `beanbeaver-android/`), not here. When in
doubt, keep it out — a smaller core is the whole point.

## The one deliberate exception: PP-OCRv5 (`crates/ocr-paddle`, `models/`)

The on-device OCR engine — image → bbox via ONNX (`ort`), plus the pixel-level
`receipt-image` preprocessing and the `models/` weights — **currently lives in
this repo, and that is architecturally the wrong place for it.** It violates the
charter above: core is supposed to *consume* bounding boxes, not *produce* them,
and shipping the heavy ONNX runtime inside what should be a pure parser is a
mistake we have chosen to live with.

**We keep it here on purpose, and we have no plan to remove the PP-OCRv5
dependency.** Read that as a two-sided instruction:

- **Do not extend it.** Treat `ocr-paddle` / `receipt-image` / `models/` as a
  frozen, tolerated exception. Do not add new OCR-inference or image-handling
  surface to core, and do not route new features through the OCR engine to "reuse
  what's already here." The charter above still governs everything *new*.
- **Do not rip it out either.** Evicting PP-OCRv5 is **not** a cleanup to do in
  passing, and not something to start because the charter makes it look tidy. Any
  actual removal is a separately-scoped, explicitly-approved project — until that
  decision is made, `ocr-paddle` stays exactly where it is.
