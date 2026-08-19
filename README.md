# beanbeaver-core

MIT-licensed Rust workspace for **on-device receipt OCR → structured parse → beancount text**.

This is the permissive “core island” shared by:

| Consumer | License | How it uses core |
|----------|---------|------------------|
| **beanbeaver-ios** | MIT | UniFFI (`bb-receipt-ffi`, via `bb-mobile-ffi`) + ONNX PP-OCRv5 on device |
| **beanbeaver-android** | MIT | Same seam, Kotlin bindings; arm64-v8a only |
| **beanbeaver** (desktop) | GPL | Parser + rules via PyO3 / native path; no copyleft deps flow *into* this repo. **Pinned at v0.3.2 and sunset** — its on-device scanning is not tracking this repo |

License policy is enforced by [`deny.toml`](deny.toml): only permissive (and MPL-2.0 via UniFFI) licenses are allowed.

## Crate map

```
crates/
  receipt-core/   Pure parse + categorize + format (no Python, no ONNX)
  receipt-image/  Pre-OCR: EXIF transpose → resize → white pad → JPEG
  ocr-paddle/     PP-OCRv5 det/rec/cls via ONNX Runtime (pixels -> detections)
  scan/           Composition: prep -> OCR -> parse (device_sim, live E2E)
  ffi/            UniFFI Swift seam (staticlib + cdylib)
rules/            Bundled default TOML (item classifier, merchants, families)
```

Pipeline (image path):

```text
JPEG/PNG bytes
  → decode (+ EXIF on desktop path)
  → resize / pad
  → PP-OCRv5 det → cls → rec
  → receipt-core (normalize → group → parse → format)
  → structured receipt + beancount fragment
```

See [docs/architecture.md](docs/architecture.md) for stage ownership and data contracts.

## Quick start

```bash
# Unit tests (pure Rust, fast)
cargo test -p receipt-core --lib

# Public cached E2E: replay checked-in .ocr.json (no models)
cargo test -p receipt-core --test public_e2e -- --nocapture

# Live OCR E2E: needs models under ./models (see below)
cargo test -p scan --test public_live_e2e -- --nocapture
```

### OCR models

PP-OCRv5 mobile weights are **not** vendored (size). Download the pinned release into `models/`:

```bash
mkdir -p models
base="https://github.com/Endle/beanbeaver-core/releases/download/ocr-models-v2"
for m in PP-OCRv5_mobile_det.onnx PP-OCRv5_mobile_rec.onnx PP-LCNet_x0_25_textline_ori.onnx; do
  curl -sSfL -o "models/$m" "$base/$m"
done
```

**Pinned release:** `ocr-models-v2` on this repo (`Endle/beanbeaver-core`).  
CI uses the same URL. Prefer verifying checksums when available in that release’s notes; do not point production builds at an untagged “latest.”

Optional orientation classifier can be disabled at session load (iOS) to skip ~23% of scan time when captures are upright.

### Environment variables

| Variable | Used by | Meaning |
|----------|---------|---------|
| `BEANBEAVER_PRIVATE_TESTS_DIR` | `private_e2e` | Path to private fixture tree; test **skips** if unset |
| `LIVE_E2E_SEED` | `public_live_e2e` | Reproducible fixture pick |
| `LIVE_E2E_COUNT` | `public_live_e2e` | How many fixtures per live run (default 2) |

Private corpus is token-gated in CI (`.github/workflows/private-regression.yml`); never commit PII here.

## Test matrix

| Gate | Command | Models? | Network? | When |
|------|---------|---------|----------|------|
| Unit (`receipt-core`) | `cargo test -p receipt-core --lib` | No | No | Every change |
| Public cached E2E | `cargo test -p receipt-core --test public_e2e -- cached` | No | No | Parser / rules |
| Private cached E2E | `cargo test -p receipt-core --test private_e2e` + env | No | Clone private repo | Regression corpus |
| Live public E2E | `cargo test -p scan --test public_live_e2e` | Yes | Models download | OCR + full stack |
| Phase 5 (strict, ignored) | `cargo test -p scan --test phase5_e2e -- --ignored` | Yes | — | Local quality ledger |
| FFI smoke (ignored) | `cargo test -p bb-receipt-ffi -- --ignored` | Yes | — | Swift seam |

CI (`.github/workflows/ci.yml`) runs unit tests on Linux + macOS, public cached E2E on Linux, and live E2E on both after downloading models.

## Rules

Default rule files live under [`rules/`](rules/) and are `include_str!`’d into `receipt-core` so iOS needs no filesystem rule pack for the defaults.

| File | Role |
|------|------|
| `default_item_classifier.toml` | Line-item → multi-tag / category key |
| `default_merchant_rules.toml` | Keyword → expense account (coarse) |
| `default_merchant_families.toml` | Canonical merchant + aliases + corroborators |

Project-local overrides belong in the consumer app (or private test `private_rules.toml`), not in this repo’s public defaults.

## Fixtures

Public redacted receipts: [`crates/receipt-core/tests/receipts_e2e/`](crates/receipt-core/tests/receipts_e2e/).  
How to add cases without leaking PII: [docs/contributing-fixtures.md](docs/contributing-fixtures.md).

## Releasing

Consumers (`beanbeaver/`, `beanbeaver-ios/`, `beanbeaver-android/`) pin this repo by **git tag**, so the tag is the version identifier — and `[workspace.package] version` in the root `Cargo.toml` must agree with it. Every crate here inherits that one value via `version.workspace = true`.

1. In the PR that will be released, bump `[workspace.package] version` and run `cargo update --workspace` to refresh `Cargo.lock`.
2. After merge, tag the merge commit `vX.Y.Z` with the **same** version and push the tag.
3. `.github/workflows/release-tag.yml` runs on that push and fails if any workspace crate's version differs from the tag (it also rejects a `v*` tag that isn't `vMAJOR.MINOR.PATCH`). Fix by bumping `Cargo.toml` and re-cutting the tag.
4. Update the pinned tag in each consumer. For `beanbeaver-ios/`, rerun `./build-xcframework.sh` after the bump or it compiles against stale generated Swift bindings.

## License

[MIT](LICENSE). Copyright © Zhenbo Li and contributors.
