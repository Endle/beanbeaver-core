# Contributing public E2E fixtures

Public fixtures live under:

```text
crates/receipt-core/tests/receipts_e2e/
  <stem>.jpg              # redacted receipt image
  <stem>.ocr.json         # frozen Paddle/OCR snapshot for cached mode
  <stem>.expected.json    # merchant, date, total, critical_items, …
```

They run on every CI via `public_e2e` (**cached** mode only — no models). Live mode uses the `.jpg` when models are present.

## Hard rules (PII)

1. **Only your own receipts** (or rights you fully control).
2. **Redact personal data** before commit: card last4 if sensitive, barcode of membership, address, phone, loyalty IDs, names on the slip, etc. Prefer solid black bars / censor blocks over blur if blur is reversible.
3. **Do not** put private corpus paths, tokens, or unredacted scans in this repo.
4. Copyright: fixtures are MIT under the repo license when contributed by the rights holder — see the corpus [README](../crates/receipt-core/tests/receipts_e2e/README.md).

## When to add a public fixture vs private-only

| Situation | Where |
|-----------|--------|
| Common Canadian grocery layout, fully redacted | Public corpus |
| One-off OCR disaster, PII-hard layout, employer store | Private regression repo only |
| Reproduces a parser bug fix | Prefer public if redaction is clean — locks the fix for everyone |

Private corpus (~100+ cases) is the product moat; public fixtures are the **always-on, no-token** smoke pack.

## How to add a case

### 1. Capture and redact

Produce a JPEG under ~3–5 MB after redaction. Prefer the same prep the app uses (long side ≤ 3000, modest padding) so live OCR matches production.

### 2. Choose a stem name

```text
<merchant>_<YYYYMMDD>_<redact|censor>[_note]
```

Examples: `costco_20260218_redact`, `tnt_20260316_food_section`.

### 3. Generate `.ocr.json` (cached snapshot)

With models available (see root README):

```bash
# Snapshot generator (ocr-paddle tests); adjust paths as in gen_ocr_snapshot.rs
cargo test -p ocr-paddle --test gen_ocr_snapshot -- --ignored --nocapture
```

Or export detections from the desktop Paddle path in the same schema the harness expects (list of boxes + text + confidence). The cached E2E must not call ONNX.

### 4. Write `.expected.json`

Minimum useful fields (see existing files for full shape):

```json
{
  "merchant": "COSTCO",
  "date": "2026-02-18",
  "total": "221.97",
  "tax": "4.44",
  "critical_items": [
    {
      "description": "KS …",
      "price": "12.99",
      "category": "Expenses:Food:Grocery:…"
    }
  ]
}
```

Assert **critical** items, not every SKU — keeps fixtures stable when OCR wobbles on fluff lines.

### 5. Run gates before PR

```bash
cargo test -p receipt-core --lib
cargo test -p receipt-core --test public_e2e -- --nocapture cached
```

Optional with models:

```bash
LIVE_E2E_COUNT=1 LIVE_E2E_SEED=0 cargo test -p scan --test public_live_e2e -- --nocapture
```

### 6. Rules changes

If the fixture only passes with a new public rule:

- Prefer a **generally reusable** keyword/tag in `rules/default_*.toml`.
- Do **not** encode one person’s loyalty naming or private store nicknames in public rules.
- Category keys map through `default_category_accounts()` in `rules.rs`.

## Dual path awareness

Some bugs are text-path only, some spatial only. When fixing:

1. Add/adjust the fixture expectation for the field that regressed.
2. Note in the PR whether the fix is text, spatial, fields, or rules — helps avoid silent dual-path drift.

## Checklist

- [ ] Image redacted; no card PAN / membership / address / phone
- [ ] `.jpg` + `.ocr.json` + `.expected.json` same stem
- [ ] `public_e2e` cached green with **public rules only**
- [ ] No secrets or private paths in commit
- [ ] License/provenance OK (own receipt or documented rights)
