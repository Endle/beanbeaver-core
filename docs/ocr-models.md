# OCR models (not vendored)

PP-OCRv5 mobile ONNX weights are **not** stored in git. Download the pinned
release into `models/` at the repo root:

```bash
mkdir -p models
base="https://github.com/Endle/beanbeaver-core/releases/download/ocr-models-v2"
for m in PP-OCRv5_mobile_det.onnx PP-OCRv5_mobile_rec.onnx PP-LCNet_x0_25_textline_ori.onnx; do
  curl -sSfL -o "models/$m" "$base/$m"
done
```

| File | Role | Size |
|------|------|------|
| `PP-OCRv5_mobile_det.onnx` | Detection | 4.82 MB |
| `PP-OCRv5_mobile_rec.onnx` | Recognition | 7.87 MB |
| `PP-LCNet_x0_25_textline_ori.onnx` | Textline orientation (optional at session load) | 1.01 MB |

**Pinned tag:** `ocr-models-v2` on this repo (`Endle/beanbeaver-core`).  
CI and local live E2E must use this tag — do not follow an untagged “latest”.

## Provenance

The orientation model is converted from the official PaddlePaddle release, not
taken from a third-party ONNX mirror:

```bash
# needs python <= 3.13 (no paddlepaddle wheel for 3.14)
pip install paddlepaddle paddle2onnx
huggingface-cli download PaddlePaddle/PP-LCNet_x0_25_textline_ori --local-dir m/
paddle2onnx --model_dir m/ --model_filename inference.json \
            --params_filename inference.pdiparams --opset_version 14 \
            --save_file PP-LCNet_x0_25_textline_ori.onnx
# sha256 8aad3208eac0bda67cb68a4c03cf376f25f9638764b6aa115a2bfa1815e76600
```

## Why `x0_25` and not `x1_0`

`ocr-models-v1` shipped `PP-LCNet_x1_0_textline_ori.onnx` (6.77 MB). The `x0_25`
variant is **5.76 MB smaller and ~3× faster**, and on the 123-receipt private
corpus it is **exactly equivalent**: identical merchant/date/total/crit-items
totals and **zero per-receipt divergence**, with classify falling 206 ms → 76 ms
(`device_sim`, whole corpus). The iOS simulator agreed — 80/127 in both arms,
all 127 cases identical, mean scan 1389 ms → 1267 ms.

Its preprocessing is identical (`ResizeImage [160, 80]`, ImageNet mean/std,
labels `[0_degree, 180_degree]`) and the converted graph has the **same 15 op
types in the same counts**, so `classify.rs` needed no change and the operator
list in `beanbeaver-android/scripts/ort-required-ops.config` is unaffected.

**The known trade:** upstream rates `x0_25` at 98.85% top-1 against `x1_0`'s
99.42%, and a head-to-head on flipped line crops has it measurably weaker at
recognising 180°-rotated text. The corpus is all upright receipts, so this costs
nothing there; a receipt photographed upside down is where it would show.
Accepted deliberately — aligning the receipt is a reasonable thing to ask.

## When upgrading models

Bump the tag in:

- `.github/workflows/ci.yml` (download step)
- This file / root README (when present)
- `beanbeaver-mobile-util/scripts/fetch-models.sh` (the `shared/` submodule both
  phone apps consume — moving it is part of *their* catch-up, not this repo's)
- iOS app resource packaging docs

A consumer pinning an older core tag keeps the model set that tag expects, so it
is safe to leave behind — `ocr-models-v1` is not going away. Move it when its
core pin moves, not before.

Inside this repo the filenames have **one** definition,
[`ocr_paddle::model_files`](../crates/ocr-paddle/src/model_files.rs) (re-exported
as `scan::model_files`); everything else here is prose or a fetch script.

Prefer publishing SHA-256 sums on the GitHub release and verifying before shipping
an xcframework.
