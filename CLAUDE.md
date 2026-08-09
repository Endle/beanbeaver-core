# beanbeaver-core

Portable, permissive (MIT) Rust crates shared by the desktop, iOS, and Android
apps. Cross-repo rules (the license firewall, core-tag pinning) live in the
umbrella `../CLAUDE.md`; repo detail (crates, tests, model fetch) is in
`README.md` / `docs/`. This file owns one thing: **what is allowed to live here.**

## Charter — an umbrella of layered crates, not one flat library

beanbeaver-core is the **umbrella** for everything a phone app needs to turn a
receipt photo into beancount text. It is deliberately not one library: it is a
set of crates in a strict layering, and the layering is the product. Consumers
bind to exactly one entry point (`crates/ffi`), so the internal structure costs
them nothing — adding a crate here never adds a pin for them.

| Crate | Job | Build |
|---|---|---|
| `receipt-core` | bbox + text → itemized details / beancount **text** | device-**independent** |
| `receipt-image` | pixels → pixels (resize, pad, EXIF) | device-independent |
| `ocr-paddle` | pixels → detections, PP-OCRv5 on ONNX | device-**dependent** (links ORT) |
| `scan` | composition: pixels → detections → receipt | device-dependent (via `ocr-paddle`) |
| `ffi` | the single UniFFI entry point consumers bind to | device-dependent |

## The hard rules

These are **enforced by `crates/receipt-core/tests/layering.rs`**, which runs in
the ORT-free fast gate. If one fails, fix the dependency — do not loosen the
assertion. Changing a rule means changing the rule and its test together, on
purpose.

1. **`receipt-core` is a device-independent leaf.** No workspace dependencies, no
   `ort`, no `image`. It stays buildable and testable with no model, no ONNX
   Runtime, and no network. This is what keeps the parser portable and keeps the
   cheapest CI gate cheap.
2. **Only `ocr-paddle` may depend on `ort`.** It is the one crate whose build is
   platform-specific, and confining that is what lets everything else build
   anywhere. Crates that need to name a fallible OCR result use
   `ocr_paddle::Result`, re-exported for exactly this purpose, rather than taking
   an `ort` dependency of their own.
3. **`ocr-paddle` must not depend on `receipt-core`.** It produces detections and
   stops. Parsing behaviour must not drift into a crate that cannot be tested
   without models.
4. **`scan` is the only composition root.** There is exactly one implementation of
   prep → OCR → parse, and `ffi` reaches the engine *through* `scan`, never past
   it into `ocr-paddle`.
5. **`ffi` is a binding surface**, not a second place to assemble the pipeline.

## Two things that follow from the rules

**Whole-pipeline tooling lives in `scan/`.** `examples/device_sim.rs` and the
live E2E tests need both halves, so that is their home. `device_sim`'s entire
value is that it runs *exactly* the code path a phone runs — so it must call
`scan::process_image` rather than re-implement the composition. A second copy
would drift, and the simulator would quietly stop simulating.

**The seam is a coordinate space, not just a type.** Detections come back in
**padded-image** pixels, so the parser is handed `padded_width`, `padded_height`
and `padding` alongside them, and undoes the padding itself. Whoever composes
owns keeping those numbers consistent with the `resize_and_pad` that actually
ran. That coupling — invisible to the type system — is much of why the
composition is one shared function instead of a few lines at each call site.

## What still never belongs here

No matter how convenient it looks in the moment:

- **UI** of any kind.
- **Linking** beancount (core only ever *emits* beancount **text** — see the
  umbrella `CLAUDE.md` license firewall).
- **Bank-statement importers** or **receipt↔transaction matching** (those are the
  desktop app's, `src/match_*.rs`).

If a change needs any of the above, it belongs in a **consumer**
(`beanbeaver/`, `beanbeaver-ios/`, `beanbeaver-android/`), not here.

## On PP-OCRv5 living here

**It stays, and that is settled.** This file used to describe `ocr-paddle`,
`receipt-image` and `models/` as an architectural mistake to be evicted someday,
and instructed readers not to extend them. That framing is **retired**: the
engine lives here on purpose, and the layering above — not a future repo split —
is what keeps it from contaminating the parser. Do not re-open eviction as a
tidy-up, and do not treat the OCR crates as frozen; they are a normal part of
this repo, governed by the hard rules like everything else.

Two notes that survive from the old framing because they are still true:

- **Don't route unrelated features through the OCR engine** to "reuse what's
  already here." Rule 3 is the mechanical version of this.
- **Moving the crate would not move the platform-specific plumbing.** The Rust in
  `ocr-paddle` is platform-agnostic bar one `#[cfg(feature = "coreml")]`; the
  iOS/Android divergence is ORT *link* plumbing in each app's build script and CI
  cache. That is a fact about where the complexity actually is, and it is why
  rule 2 (confine `ort`) buys more than relocation would.
