# Gift-card receipt metadata (core v0.15.0)

This change extracts receipt evidence in `receipt-core` and carries it through
`scan`, `process`, and UniFFI. App persistence and UI are deferred at the user's
request. No live issuer verification, card matching, or balance history is
implemented here.

## Supported evidence

- LCBO gift-card tenders: the whole masked identifier, authorization reference,
  expiry (`unknown`, `no_expiry`, or `printed_date`), and post-payment balance.
- Costco Shop Card tenders: the preceding authorization block's masked
  identifier, `APP` reference, and remaining balance.
- Costco DoorDash and LCBO purchases: the associated `PC` reference and
  receipt-reported `ACTIVATED` annotation. Repeated product occurrences remain
  separate; missing/ambiguous occurrences leave their association unresolved.
  Occurrences are keyed on the description *without* its item number, because
  the parser keeps the number on one emitted line and strips it from the next
  (`costco_biz_20260125` emits `DOORDASH2X50` and `399 DOORDASH2X50` for two
  identical packs). The item-number fallback runs only when that key finds no
  row: identical products share a number, so a fallback that always ran made
  them permanently ambiguous — both real two-pack receipts in the corpus were
  `unresolved: ["association"]` on clean OCR until this was measured.
- Explicit pack notation such as `DOORDASH2X50`: two cards, denomination 5000
  cents, and derived total face value 10000 cents. The paid price stays on the
  item. A price of 40000 cents on `LCBO CARD` does not establish loaded value.

A `PC` reference is preserved under that label. It is not an individual card
identity and is not linked to redemption identifiers.

## Data contract

`ParsedReceiptItem.gift_card` / `ReceiptItem.gift_card` contain an optional
`GiftCardPurchase`. `ParsedReceiptTender.gift_card` / `ReceiptTender.gift_card`
contain an optional `GiftCardRedemption`. All new records and enums cross UniFFI
in both directions. The leaf metadata records also derive Serde serialization
and deserialization; missing metadata properties default to unknown/absent.

Money in metadata is an optional signed 64-bit count of cents. `Some(0)` is a
reported zero, not an absent balance. Currency is copied from the scan's explicit
currency context; a later Beancount reformat does not reinterpret that original
observation in its export currency. Direct text parsing without a scan context
leaves currency absent. No balance is modified to reconcile a tender mismatch.

Identifier normalization only maps `x`, `X`, `×`, and `*` to `*`. Visible digits,
mask positions, and a masked final digit are retained. `printed_identifier`
keeps the original OCR spelling. OCR character errors are not corrected by
inventing digits from receipt arithmetic.

Each metadata record retains `evidence` with a field name, source line index,
and the source text. Indices address **nonempty, trimmed lines of `raw_text`**.
Text is retained inside the evidence so storing a metadata object does not
require keeping every detection. `unresolved_fields` names observed but
unreadable/conflicting fields. Missing evidence alone means absent/unknown.

`source_id` is an opaque, receipt-local extraction token. It must travel with
metadata through edits and serialization. It includes the document/anchor
fingerprint and occurrence, so different instances of identical products are
not conflated. It is not a card ID, not globally unique, and not a stable key
across OCR reprocessing or core versions. Reprocessing requires reconciling
previous corrections against new evidence rather than guessing by position.

## Corrections

The item edit list carries `ItemCorrection.gift_card` / `EditedItem.gift_card`
explicitly. Carry the existing metadata object when editing or moving an item;
`None` clears it. New ordinary rows use `None` and cannot inherit a reference
from their description or position. An unchanged item block remains untouched
when `ReceiptCorrections.items` / `ReceiptEdits.items` is absent.

`ReceiptCorrections.tenders` / `ReceiptEdits.tenders` is an optional replacement
payment list. Absent preserves existing tenders; a present list carries each
payment's metadata. Tender amounts at the FFI correction boundary use strict
decimal parsing. Source tokens must identify existing metadata in the previous
receipt, and duplicate/stale sources are rejected.

Corrections preserve original evidence, record changed property names in
`corrected_fields`, and clear those properties' unresolved markers. Identifier
normalization and derived pack totals are recomputed from corrected values.
Invalid negative balances/face values, zero card counts, missing printed expiry
dates, and overflowing derived totals are rejected. Tender reconciliation is
refreshed when payment amounts or the receipt total change. Merely adding gift
metadata does not change Beancount output.

## Validation

Synthetic tests live in `src/gift_cards/tests.rs` and `tests/gift_cards.rs`.
FFI tests exercise conversion, correction, retained evidence, and strict payment
amounts. Private expectations are transcribed from the LCBO August 27 receipt
and Costco July 2 / August 26 receipts; private identifiers stay in that repo.

The shared `tests/e2e_harness/gift_cards.rs` checks optional `gift_card` objects
on expected `tenders` and `critical_items`. Item metadata requires an explicit
`item_index` and checks the description/price anchor, so identical products
cannot satisfy each other's metadata assertion. Omitted keys are unasserted;
JSON `null` asserts absence. The same checker runs in cached E2E and
`device_sim`, including fresh OCR. `device_sim --dump` shows the evidence and
individual metadata failures. Committed OCR snapshots are not regenerated.

A divergence is recorded, not omitted: inside the `gift_card` object,
`known_failure_core` / `known_failure` list the tolerated fields (or `true`),
a marked field that matches is reported as stale, and the entry-level and
check-level markers umbrella it like any other assertion. The cached harness
honours these; `device_sim` is marker-blind for `gift_card` exactly as it is
for every other check, so a recorded defect shows as `✗ gift_card` there.
Schema and the recorded cases are in `beanbeaver-private-test/CLAUDE.md` §2e.

This is a breaking native record change. Future consumer updates must regenerate
bindings and adapt record constructors before adopting the core tag. Neither
mobile app is modified by this core-only change.

Run the core gates from the repository root:

```sh
RUSTC_WRAPPER= BEANBEAVER_PRIVATE_TESTS_DIR=../beanbeaver-private-test cargo test --release --workspace
RUSTC_WRAPPER= cargo clippy --workspace --all-targets -- -D clippy::correctness -D clippy::style
cargo fmt --all -- --check
```

On a Mac with Swift installed, check the actual generated Swift/UniFFI wire
contract without building an app or loading OCR models:

```sh
RUSTC_WRAPPER= ./crates/ffi/scripts/gift-card-smoke.sh
```

Validation on 2026-09-09: workspace tests including the private cached corpus
passed, as did the Swift wire smoke. The association fix above was measured
the same way: per-receipt scorecards identical to baseline in both modes, and
the two two-pack receipts went from unresolved to their photo-read references. Kotlin bindings were also generated from
the new library. Full 144-receipt before/after scorecards had identical
per-receipt outcomes in both modes: cached totals 144/144 and critical items
1216/1265; fresh OCR totals 142/144 and critical items 1153/1265. Those baseline
OCR misses remain visible. All three photo-grounded gift-card metadata cases
pass in both modes, including the August Costco activation on a receipt with
an existing unrelated item-count failure.
