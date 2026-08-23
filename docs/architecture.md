# Architecture

## Goals

1. **On-device by default** — image → beancount without a network call.
2. **Deterministic parsing** — rule- and layout-based, corpus-gated; not LLM core.
3. **License isolation** — MIT core safe for App Store / iOS and linkable from GPL desktop without infecting core with copyleft deps.
4. **Parity** — desktop Python flow and iOS fat-Rust path share `receipt-core` semantics.

## Layering

```text
receipt-image ──> ocr-paddle ──┐
                               ├──> scan ──> ffi
receipt-core ──────────────────┘
```

| Crate | Job | Build |
|---|---|---|
| `receipt-core` | bbox + text → itemized details / beancount **text** | device-**independent** |
| `receipt-image` | pixels → pixels (resize, pad, EXIF) | device-independent |
| `ocr-paddle` | pixels → detections (PP-OCRv5 on ONNX) | device-**dependent** (links ORT) |
| `scan` | composition: prep → OCR → parse | device-dependent |
| `ffi` | the single UniFFI entry point consumers bind to | device-dependent |

The two halves — parse and OCR — do not know about each other; `scan` is the only
place they meet. Consumers bind to `ffi` alone, so the internal structure costs
them nothing: adding a crate here never adds a pin for them. The rules that keep
this true live in `CLAUDE.md` and are asserted by
`crates/receipt-core/tests/layering.rs`.

**Why the OCR engine lives in an otherwise-portable repo.** One repo means one
tag and one pin for the apps, and it keeps the whole-pipeline integration test in
a single CI. The alternative — lifting OCR into its own repo — was reviewed and
rejected: it adds a release chain to the *most frequent* kind of change (rules
and parser work), and it lets two independently-green CIs hide a broken pair,
which is the failure this project has already been bitten by (frozen `.ocr.json`
fixtures passing while the app regressed). Confining `ort` to a single crate buys
the isolation the split was meant to provide. Note also that the iOS/Android
divergence is ORT *link* plumbing in each app's build script and CI cache —
moving the crate would move none of it.

## Workspace crates

### `receipt-core`

Pure Rust. No ONNX, no image I/O beyond types.

| Module area | Responsibility |
|-------------|----------------|
| `ocr_transform` | Raw detections → full text + spatial/helper pages |
| `ocr_line_grouping` / `detection_normalization` | Geometry cleanup before parse |
| `parser` | Orchestrates field + item extraction |
| `fields` | Merchant, date, tax, total, tenders |
| `text` | Line-oriented item extraction (dense grocery layouts) |
| `spatial` | BBox/column-aware item extraction |
| `categories` + `rules` | Classifier TOML → tags/accounts |
| `merchant_match` | Fuzzy family resolution (Exact / Corrected / Suggested / Unknown) |
| `formatter` | Beancount text + `beanbeaver-id` / `document:` metadata |
| `process` | Single entry: detections → `ProcessedReceipt` |

**Dual item paths:** spatial vs text. The parser chooses/merges based on layout quality; both are covered by cached E2E fixtures. Prefer table-driven TOML for merchant quirks over hard-coded branches when possible.

### `receipt-image`

Desktop-parity pre-OCR pipeline:

```text
decode → EXIF transpose → Lanczos cap long side → white pad → JPEG
```

Constants: `MAX_IMAGE_DIMENSION = 3000`, `OCR_IMAGE_PADDING = 50`, `JPEG_QUALITY = 95`.  
Deskew is intentionally **not** applied (historical desktop regression).

Consumers: PyO3 desktop path and (where wired) on-device prep that should match Pillow-ish dimensions.

### `ocr-paddle`

PP-OCRv5 mobile:

1. Prep (resize/pad)
2. Detection (DB post-process → quads)
3. Optional textline orientation cls
4. Recognition + CTC decode
5. Return detections — and stop there

The only crate that depends on `ort`; it re-exports `ocr_paddle::Result` so
callers need not.

**Every shipping build runs the plain CPU EP (MLAS)** — iOS, Android, CI and
`device_sim` alike. The `coreml` and `xnnpack` features exist but **no consumer
enables either**: `beanbeaver-ios` dropped CoreML in 3c580a6 (2026-07-01)
because the CPU EP won on both speed and accuracy for the shipped dynamic-shape
mobile models, and `xnnpack` does not link. This is load-bearing, because it is
what makes `device_sim` — which is CPU-only — a faithful proxy for both phones
rather than a poor one. See `crates/ocr-paddle/Cargo.toml` for the full
reasoning.

Note that the iOS binary still *links* `CoreML.framework` and carries CoreML EP
symbols, because pyke's prebuilt `libonnxruntime.a` is compiled with the EP and
`build-xcframework.sh` merges the whole archive. **The framework being linked is
not evidence that the EP is used** — check the cargo feature, not the binary.

### `scan`

The composition root, and the only implementation of prep → OCR → parse:
`scan::process_image` / `process_image_timed`, plus `ScanTimings` (which spans
both halves, so it cannot live in either).

Whole-pipeline diagnostics and tests live here because they need both halves:
`examples/device_sim.rs`, `tests/public_live_e2e.rs`, `tests/phase5_e2e.rs`.

### `bb-receipt-ffi` (`crates/ffi`)

UniFFI objects for Swift:

- `OcrSession::new(model_dir, use_orientation_cls)` — load once
- `OcrSession::scan(image_bytes, today, credit_card_account)` — full pipeline

Models expected in `model_dir`:

- `PP-OCRv5_mobile_det.onnx`
- `PP-OCRv5_mobile_rec.onnx`
- `PP-LCNet_x0_25_textline_ori.onnx` (optional path when cls disabled)

Scans on one session are serialized (`Mutex` around the engine).

## Data contracts

### OCR detection (into `receipt-core`)

```text
RawDetection { points: [(x,y); 4+], text, confidence }
+ padded_width, padded_height, padding
```

This seam is a **coordinate space** as much as a type: detections come back in
*padded-image* pixels, so the parser is handed the padded dimensions and the
padding, and undoes them itself. Whoever composes owns keeping those numbers
consistent with the `resize_and_pad` that actually ran — a coupling the type
system cannot check, and much of why `scan::process_image` is one shared
function rather than a few lines repeated at each call site.

### OCR document (inside `receipt-core`)

```text
OcrDocument { lines: [OcrLine { text, words: [OcrWord { text, bbox, confidence }], height, center_y }] }
```

What `ocr_transform` produces and `parser` consumes. Everything in it is
normalized to `[0,1]` against the **de-padded** image, which is the other half of
the coordinate-space contract above: pixels stop at `transform`, and nothing
downstream of it needs the image dimensions again.

One document, not three views of one. It used to be `full_text` plus a merchant
page list plus a spatial page list, built in a single loop and passed as three
positional parameters — so the parser had to check at runtime that the line
counts still agreed before it could index one by the other. `full_text()` is now
a method over the same lines, and that check is gone because it cannot fail.

### Parse result

`ParsedReceiptData`: merchant (+ `MerchantMatch`), date, total/tax/subtotal, items (description, price, qty, category key, tags), warnings, tenders, raw text.

### Output identity

When `image_sha256` is provided:

- `beanbeaver-id`: `bb-<yyyymmdd>-<sha8>` (greppable)
- `document:` relative path under ledger documents root

The iOS app must save the **same** image bytes that were hashed so links resolve.

### Beancount

Formatter emits a transaction with item postings, tax, tender/liability, and `FIXME` markers for unknown date / unaccounted remainder. Default uncategorized account: `Expenses:FIXME`.

## Rules layering

```text
default_item_classifier.toml  ──┐
                                ├──► ParserRuleLayers (category rules + account map)
optional override TOMLs ────────┘

default_merchant_rules.toml   → known merchant keywords
default_merchant_families.toml → fuzzy MerchantFamily list
```

Public E2E uses **public rules only**. Private E2E may layer `private_rules.toml` via `parser_rule_layers_with_overrides`.

## Testing philosophy

| Layer | What it proves |
|-------|----------------|
| Unit tests | Local helpers, rounding, matchers |
| Cached public E2E | Parser stable on frozen OCR snapshots |
| Private cached E2E | Broader real-world regression (no PII in this repo) |
| Live public E2E | Full ONNX stack runs; soft parse compare (OCR nondeterminism) |
| Phase 5 / device_sim | Quality ledger and on-device attribution |

Do **not** “fix” OCR by loosening parse expectations without measuring det vs rec vs parse separately.

## Non-goals

Technical directions this repo does not take:

- Shipping a GPL beancount Python dependency inside this tree
- LLM as the primary parser
- Vendoring multi-hundred-MB ONNX weights in git

For *scope* prohibitions — what may not live in this repo at all (UI, beancount
linking, importers, receipt↔transaction matching) — see `CLAUDE.md`, which is the
single source for those.
