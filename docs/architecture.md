# Architecture

## Goals

1. **On-device by default** — image → beancount without a network call.
2. **Deterministic parsing** — rule- and layout-based, corpus-gated; not LLM core.
3. **License isolation** — MIT core safe for App Store / iOS and linkable from GPL desktop without infecting core with copyleft deps.
4. **Parity** — desktop Python flow and iOS fat-Rust path share `receipt-core` semantics.

## Workspace crates

### `receipt-core`

Pure Rust. No ONNX, no image I/O beyond types.

| Module area | Responsibility |
|-------------|----------------|
| `ocr_transform` | Raw detections → full text + spatial/helper pages |
| `ocr_line_grouping` / `detection_normalization` | Geometry cleanup before parse |
| `receipt_parser` | Orchestrates field + item extraction |
| `receipt_fields` | Merchant, date, tax, total, tenders |
| `receipt_text` | Line-oriented item extraction (dense grocery layouts) |
| `receipt_spatial` | BBox/column-aware item extraction |
| `receipt_categories` + `rules` | Classifier TOML → tags/accounts |
| `merchant_match` | Fuzzy family resolution (Exact / Corrected / Suggested / Unknown) |
| `receipt_formatter` | Beancount text + `beanbeaver-id` / `document:` metadata |
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
5. Hand off detections to `receipt-core::process_receipt`

Feature `coreml` enables Apple Neural Engine / GPU via `ort` (iOS xcframework). Linux/desktop CI uses CPU ORT.

Diagnostics: `examples/device_sim.rs`, scripts under `scripts/`.

### `bb-receipt-ffi` (`crates/ffi`)

UniFFI objects for Swift:

- `OcrSession::new(model_dir, use_orientation_cls)` — load once
- `OcrSession::scan(image_bytes, today, credit_card_account)` — full pipeline

Models expected in `model_dir`:

- `PP-OCRv5_mobile_det.onnx`
- `PP-OCRv5_mobile_rec.onnx`
- `PP-LCNet_x1_0_textline_ori.onnx` (optional path when cls disabled)

Scans on one session are serialized (`Mutex` around the engine).

## Data contracts

### OCR detection (into `receipt-core`)

```text
RawDetection { points: [(x,y); 4+], text, confidence }
+ padded_width, padded_height, padding
```

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

- Shipping a GPL beancount Python dependency inside this tree
- LLM as the primary parser
- Vendoring multi-hundred-MB ONNX weights in git
