//! Gift-card metadata assertions, shared by the cached E2E harness and
//! `device_sim` so live OCR checks the same fields.
//!
//! An expected tender or `critical_items` entry may carry a `gift_card`
//! object. Every key in it is compared against the parsed metadata: JSON
//! `null` asserts absence, an omitted key is unasserted, arrays compare
//! whole. Item metadata is positional (`item_index`) and anchored to the
//! entry's description/price, so repeated identical products cannot satisfy
//! each other's assertion.
//!
//! **Markers.** Inside the `gift_card` object, `known_failure` /
//! `known_failure_core` name the fields whose divergence is tolerated —
//! `["normalized_identifier"]` — or `true` for every field. A marked field
//! that matches is reported so the stale marker gets removed, the same
//! rot-proofing every other check has. The entry-level `known_failure` bool
//! on the tender/item umbrellas its `gift_card` too, and the check-level
//! `known_failures: ["tenders" | "critical_items"]` umbrellas the block; both
//! are applied by the caller in `mod.rs`. `device_sim` calls [`check`], which
//! is marker-blind like the rest of its scorecard.
use receipt_core::parser::{ParsedReceiptData, ParsedReceiptItem, ParsedReceiptTender};
use serde_json::Value;
use std::collections::HashSet;

/// Outcome of one entry's `gift_card` assertion.
#[derive(Default)]
pub struct EntryOutcome {
    /// Unmarked divergences — these fail the case.
    pub errors: Vec<String>,
    /// Marked fields that matched — always reported, so the marker is removed.
    pub stale: Vec<String>,
}

impl EntryOutcome {
    #[allow(dead_code)] // `device_sim` includes this file and uses only `check`
    pub fn failed(&self) -> bool {
        !self.errors.is_empty()
    }
}

/// Marker keys are metadata, not fields to compare. Suffixed markers for the
/// other consumers (`_ios`, `_desktop`, …) are skipped too: only core reads
/// these objects today, and an unknown suffix must not become a field lookup.
fn is_marker_key(key: &str) -> bool {
    key.starts_with("known_failure")
}

/// `known_failure` ∪ `known_failure_core` inside the object. `None` = no
/// marker, `Some(None)` = every field, `Some(Some(set))` = those fields.
fn markers(want: &Value) -> Option<Option<HashSet<String>>> {
    let mut all = false;
    let mut fields = HashSet::new();
    let mut any = false;
    for key in ["known_failure", "known_failure_core"] {
        match want.get(key) {
            Some(Value::Bool(true)) => {
                all = true;
                any = true;
            }
            Some(Value::Array(list)) => {
                any = true;
                fields.extend(list.iter().filter_map(Value::as_str).map(String::from));
            }
            Some(Value::Bool(false)) | None => {}
            Some(other) => {
                panic!("gift_card {key} must be true or a list of field names, got {other}")
            }
        }
    }
    any.then_some(if all { None } else { Some(fields) })
}

fn divergence(expected: &Value, actual: Option<&Value>, path: &str) -> Option<String> {
    match actual {
        None => Some(format!("{path}: missing (expected {expected})")),
        Some(got) if got != expected => Some(format!("{path}: expected {expected}, got {got}")),
        Some(_) => None,
    }
}

/// Compare every asserted field of `want` against the parsed metadata.
fn compare(want: &Value, got: &Value, path: &str, honor_markers: bool) -> EntryOutcome {
    let mut out = EntryOutcome::default();
    let Some(object) = want.as_object() else {
        // `"gift_card": null` asserts that no metadata was extracted.
        out.errors.extend(divergence(want, Some(got), path));
        return out;
    };
    let marked = if honor_markers { markers(want) } else { None };
    for (key, value) in object {
        if is_marker_key(key) {
            continue;
        }
        let field_path = format!("{path}.{key}");
        let diverged = divergence(value, got.get(key), &field_path);
        let tolerated = match &marked {
            None => false,
            Some(None) => true,
            Some(Some(fields)) => fields.contains(key),
        };
        match (diverged, tolerated) {
            (Some(msg), false) => out.errors.push(msg),
            (Some(_), true) => {}
            (None, true) => out.stale.push(format!(
                "{field_path} marked known_failure but matched — remove the marker"
            )),
            (None, false) => {}
        }
    }
    out
}

/// The `gift_card` assertion of expected tender `i`, or `None` when the entry
/// carries no such key. The caller has already compared kind and amount.
pub fn tender_metadata(
    i: usize,
    entry: &Value,
    got: Option<&ParsedReceiptTender>,
    honor_markers: bool,
) -> Option<EntryOutcome> {
    let want = entry.get("gift_card")?;
    let path = format!("tender[{i}].gift_card");
    Some(match got {
        Some(t) => compare(
            want,
            &serde_json::to_value(&t.gift_card).unwrap(),
            &path,
            honor_markers,
        ),
        None => EntryOutcome {
            errors: vec![format!("{path}: no tender parsed at this index")],
            stale: Vec::new(),
        },
    })
}

/// The `gift_card` assertion of one expected `critical_items` entry, or
/// `None` when it carries no such key. Requires `item_index`; the description
/// and price at that index must match the entry, so a shifted item list fails
/// here rather than asserting another line's metadata.
pub fn item_metadata(
    entry: &Value,
    items: &[ParsedReceiptItem],
    honor_markers: bool,
) -> Option<EntryOutcome> {
    let want = entry.get("gift_card")?;
    let Some(index) = entry.get("item_index").and_then(Value::as_u64) else {
        return Some(EntryOutcome {
            errors: vec!["gift_card item expectation requires item_index".into()],
            stale: Vec::new(),
        });
    };
    let path = format!("item[{index}].gift_card");
    let Some(got) = items.get(index as usize) else {
        return Some(EntryOutcome {
            errors: vec![format!("{path}: no item parsed at this index")],
            stale: Vec::new(),
        });
    };
    let desc = entry
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    let anchored = got
        .description
        .to_ascii_uppercase()
        .contains(&desc.to_ascii_uppercase())
        && entry
            .get("price")
            .and_then(Value::as_str)
            .map_or(true, |p| p == got.price.to_string());
    if !anchored {
        return Some(EntryOutcome {
            errors: vec![format!(
                "{path}: anchor mismatch — parsed '{}' {} at this index",
                got.description, got.price
            )],
            stale: Vec::new(),
        });
    }
    Some(compare(
        want,
        &serde_json::to_value(&got.gift_card).unwrap(),
        &path,
        honor_markers,
    ))
}

/// Marker-blind whole-receipt check for `device_sim`, whose scorecard ignores
/// `known_failure` everywhere else too. Tender kind/amount anchors are checked
/// here because `device_sim` has no tenders block of its own.
pub fn check(parsed: &ParsedReceiptData, expected: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    if let Some(tenders) = expected.get("tenders").and_then(Value::as_array) {
        if tenders.iter().any(|t| t.get("gift_card").is_some())
            && tenders.len() != parsed.tenders.len()
        {
            errors.push(format!(
                "gift-card tender count: expected {}, got {}",
                tenders.len(),
                parsed.tenders.len()
            ));
        }
        for (i, entry) in tenders.iter().enumerate() {
            let got = parsed.tenders.get(i);
            if let Some(t) = got {
                if entry.get("gift_card").is_some()
                    && (entry
                        .get("kind")
                        .and_then(Value::as_str)
                        .is_some_and(|k| k != t.kind)
                        || entry
                            .get("amount")
                            .and_then(Value::as_str)
                            .is_some_and(|a| a != t.amount.to_string()))
                {
                    errors.push(format!(
                        "tender[{i}].gift_card: anchor mismatch — parsed \"{}\" {}",
                        t.kind, t.amount
                    ));
                }
            }
            if let Some(outcome) = tender_metadata(i, entry, got, false) {
                errors.extend(outcome.errors);
            }
        }
    }
    if let Some(items) = expected.get("critical_items").and_then(Value::as_array) {
        for entry in items {
            if let Some(outcome) = item_metadata(entry, &parsed.items, false) {
                errors.extend(outcome.errors);
            }
        }
    }
    errors
}
