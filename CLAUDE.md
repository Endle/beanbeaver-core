# beanbeaver-core

Portable, permissive (MIT) Rust crates shared by the desktop, iOS, and Android
apps. Cross-repo rules (the license firewall, core-tag pinning) live in the
umbrella `../CLAUDE.md`. **What the crates are and how they fit together is in
[`docs/architecture.md`](docs/architecture.md)** — read that for orientation.
This file owns one thing: **what is allowed to live here.**

## The hard rules

Parsing (`receipt-core`) and OCR (`ocr-paddle`) are separate halves that meet
only in `scan`. These rules keep them that way, and are **enforced by
`crates/receipt-core/tests/layering.rs`** in the ORT-free fast gate. If one
fails, fix the dependency — do not loosen the assertion. Changing a rule means
changing the rule and its test together, deliberately.

1. **`receipt-core` is a device-independent leaf.** No workspace dependencies, no
   `ort`, no `image`. It stays buildable and testable with no model, no ONNX
   Runtime, and no network.
2. **Only `ocr-paddle` may depend on `ort`.** To name a fallible OCR call, use
   `ocr_paddle::Result` — it is re-exported for exactly this reason.
3. **`ocr-paddle` must not depend on `receipt-core`.** It produces detections and
   stops.
4. **`scan` is the only composition root.** One implementation of prep → OCR →
   parse. Anything needing both halves — including diagnostics like `device_sim`
   — belongs in `scan` and must *call* `scan::process_image`, never re-implement
   it. A second copy would drift, and `device_sim` would stop reproducing device
   behaviour.
5. **`ffi` is a binding surface**, not a second place to assemble the pipeline.
   It reaches the engine through `scan`, never past it.

## What never belongs here

- **UI** of any kind.
- **Linking** beancount — core only ever *emits* beancount **text** (see the
  umbrella `CLAUDE.md` license firewall).
- **Bank-statement importers** or **receipt↔transaction matching** — those are
  the desktop app's, `src/match_*.rs`.

If a change needs one of these, it belongs in a consumer (`beanbeaver/`,
`beanbeaver-ios/`, `beanbeaver-android/`).

## PP-OCRv5 stays — settled, do not re-open

This file used to describe `ocr-paddle` / `receipt-image` / `models/` as an
architectural mistake to evict someday. That framing is **retired.** The engine
lives here on purpose, and the layering above — not a repo split — is what keeps
it from contaminating the parser. Do not propose eviction as a cleanup, and do
not treat the OCR crates as frozen; they are a normal part of this repo,
governed by the rules above like everything else. The reasoning is recorded
under "Layering" in [`docs/architecture.md`](docs/architecture.md).
