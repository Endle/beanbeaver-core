# Receipt contract migration

The refactor fixes the distinction between the winning classification path and
the expense account, shares extraction output between text and spatial parsing,
and splits parser internals into smaller modules. Extraction heuristics and
stage order are preserved.

## Rust consumers

- `ParsedReceiptItem.tag_path` now contains the winning path, such as
  `grocery/dairy`. Read `account` for `Expenses:Food:Grocery:Dairy`.
- The duplicate `ParserRuleLayers.account_mapping` field is removed. Use
  `layers.category_rules.account_mapping`, a `HashMap<String, String>`.
- Prefer `categories::classify_item` when both the account and tags are needed.
  Its `ItemClassification` contains `tag_path`, `account`, and `tags` from one
  matching pass. Existing `classify_item_key` and `classify_item_tags` remain.
- Prefer `process_receipt_request(page, ProcessRequest { ... }, options)` and
  `reformat_with_context(parsed, FormatContext { ... }, corrections, options)`.
  Existing positional process/reformat functions remain compatibility wrappers.
  `scan::ScanRequest` is a re-export of `ProcessRequest` with the same fields.

## Swift and Kotlin consumers

`ReceiptItem` has a new optional `tagPath` field. This changes the UniFFI record
encoding: regenerate bindings and rebuild the native library together when
adopting the core release. Old generated bindings cannot read the new record.

Preserve `tagPath` when storing or reconstructing a receipt for reformatting.
Update manually constructed records to supply it. Records loaded from older
JSON without the field should use `nil`/`null`; do not infer it from the last
semantic tag. Tags accumulate from several rules, and their final entry need
not be the path that supplied the account.

The app persistence adapters and manually constructed test/preview records need
this change when their core pin is updated. This core refactor does not change
consumer pins or generated files in the app repositories.

## Verification

The refactor was checked against the original commit using 21 public and 135
private cached OCR snapshots, with bundled rules. All receipt output fields
matched after excluding the corrected winning-path representation and the
shared warning type's debug name. This includes item order, descriptions,
amounts, tags, warnings, confidence, and Beancount text.

Contract tests cover scanned/edited classification and FFI reformatting with
unrelated semantic tags. Workspace tests pass with the private fixture corpus enabled, as do formatting
and Clippy checks. The fixture harnesses compare account expectations against
`account`, preserving their existing matching rules. The ignored FFI live-scan
smoke test was also run and passed.
A temporary iOS simulator app built against the generated Swift bindings scanned
the public Costco fixture and verified that reformatting preserved the winning
path after another tag was appended. Both Swift and Kotlin bindings were
generated; this does not constitute an Android runtime test.

## Item numbers

`ParsedReceiptItem` and FFI `ReceiptItem` now expose `item_number: Option<String>`
(`itemNumber` in Swift/Kotlin). Extraction currently recognizes Costco's leading
numeric row code, captured before description cleanup. Leading zeros survive;
missing/unreadable codes and unsupported merchants return `None`. This is a
merchant-scoped identifier, not a universal product ID. A discount row's leading
code is its own identifier; the reference following `TPD/` is not substituted.
Existing descriptions, including any printed code and expanded name, are unchanged.

`ItemCorrection` and FFI `EditedItem` also carry the optional field. Preserve it
when renaming or reconstructing an item. If omitted, reformatting preserves a
prior code for an unchanged description; a new/renamed line has no inferred code.
A reformat with no item edits preserves all item numbers.

This changes UniFFI record encoding. Regenerate Swift/Kotlin bindings and rebuild
the native library together when adopting the core release. Update persistence
adapters and manual record constructors; old saved records default to `nil`/`null`.
Consumers should read the field directly rather than parse the display name.

Validated with workspace tests (including public and private cached fixtures),
the live FFI Costco test, formatting, and Clippy. A before/after live scan of all
135 private images produced identical per-receipt quality results. Generated
Swift and Kotlin bindings; an iOS simulator smoke app scanned the public Costco
receipt, asserted Coke Zero and milk item numbers, and preserved every code
through reformatting. Android bindings were generated, not runtime-tested.
