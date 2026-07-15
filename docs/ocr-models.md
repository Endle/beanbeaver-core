# OCR models (not vendored)

PP-OCRv5 mobile ONNX weights are **not** stored in git. Download the pinned
release into `models/` at the repo root:

```bash
mkdir -p models
base="https://github.com/Endle/beanbeaver/releases/download/ocr-models-v1"
for m in PP-OCRv5_mobile_det.onnx PP-OCRv5_mobile_rec.onnx PP-LCNet_x1_0_textline_ori.onnx; do
  curl -sSfL -o "models/$m" "$base/$m"
done
```

| File | Role |
|------|------|
| `PP-OCRv5_mobile_det.onnx` | Detection |
| `PP-OCRv5_mobile_rec.onnx` | Recognition |
| `PP-LCNet_x1_0_textline_ori.onnx` | Textline orientation (optional at session load) |

**Pinned tag:** `ocr-models-v1` on `Endle/beanbeaver`.  
CI and local live E2E must use this tag — do not follow an untagged “latest”.

When upgrading models, bump the tag in:

- `.github/workflows/ci.yml` (download step)
- This file / root README (when present)
- iOS app resource packaging docs

Prefer publishing SHA-256 sums on the GitHub release and verifying before shipping
an xcframework.
