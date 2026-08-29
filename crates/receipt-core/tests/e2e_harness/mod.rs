//! Shared cached-E2E harness for the receipt corpora.
//!
//! Both `private_e2e.rs` (out-of-tree PII corpus, env-gated) and `public_e2e.rs`
//! (vendored redacted corpus, always-on) drive the same pipeline: raw `.ocr.json`
//! -> `ocr_transform::transform` -> `parse_receipt` -> assert against
//! `.expected.json` (merchant fuzzy / date exact / total exact / critical items),
//! honoring `known_failures`. Item `category` is a HARD assertion — the
//! `category_optional` escape is banned (mirrors the desktop Python harness).
//!
//! This file lives under `tests/e2e_harness/` (a subdirectory), so Cargo does NOT
//! compile it as its own test binary; each integration test `mod`-includes it.

#![allow(dead_code)] // each test binary uses only the entry point it needs

use receipt_core::money::Money;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use receipt_core::categories::resolve_account_target;
use receipt_core::ocr_transform::{transform, RawDetection};
use receipt_core::parser::parse_receipt;
use receipt_core::rules::{
    default_known_merchants, default_merchant_families, parser_rule_layers_with_overrides,
};
use serde_json::Value;

/// Reference date (year, month, day). Only affects placeholder/2-digit-year
/// inference; the corpora use explicit full dates, so the exact value is inert.
pub const TODAY_YEAR: i32 = 2026;
/// Padding the desktop pre-OCR resize adds; the raw `.ocr.json` coordinates are in
/// this padded space (matches the PyO3 `receipt_process_receipt` default).
pub const PADDING: i64 = 50;

// --- tolerant matchers (faithful to the Python cached harness) ----------------

fn normalize_merchant(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect()
}

/// Merchant details come directly from OCR, so typography is not semantic:
/// phone punctuation, postal-code spacing, address punctuation, and case may
/// differ while the extracted value is the same. OCR character substitutions
/// are deliberately *not* corrected here.
fn detail_matches(key: &str, expected: &str, actual: &str) -> bool {
    let normalized_expected = normalize_merchant(expected);
    let normalized_actual = normalize_merchant(actual);
    if normalized_expected == normalized_actual {
        return true;
    }
    if key != "region" {
        return false;
    }
    fn canonical_region(value: &str) -> &str {
        match value {
            "ALBERTA" => "AB",
            "BRITISHCOLUMBIA" => "BC",
            "MANITOBA" => "MB",
            "NEWBRUNSWICK" => "NB",
            "NEWFOUNDLAND" | "NEWFOUNDLANDANDLABRADOR" => "NL",
            "NOVASCOTIA" => "NS",
            "ONTARIO" => "ON",
            "PRINCEEDWARDISLAND" => "PE",
            "QUEBEC" => "QC",
            "SASKATCHEWAN" => "SK",
            other => other,
        }
    }
    canonical_region(&normalized_expected) == canonical_region(&normalized_actual)
}

fn levenshtein(a: &[u8], b: &[u8]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Normalized substring either way, else similarity ratio >= 0.85.
fn merchant_matches(expected: &str, actual: &str) -> bool {
    let (e, a) = (normalize_merchant(expected), normalize_merchant(actual));
    if e.is_empty() || a.is_empty() {
        return false;
    }
    if a.contains(&e) || e.contains(&a) {
        return true;
    }
    let maxlen = e.len().max(a.len());
    (maxlen - levenshtein(e.as_bytes(), a.as_bytes())) as f64 / maxlen as f64 >= 0.85
}

/// Exact. This used to be float-tolerant (`|e - a| < 0.005`) because the
/// spatial path rendered `"6.9700"` where the fixture said `"6.97"`. `Money`
/// removed the second format, so the tolerance has nothing left to absorb and
/// its removal is the proof of that: if this ever needs loosening again,
/// something has started emitting two formats.
fn price_matches(expected: &str, actual: Money) -> bool {
    Money::from_decimal_str(expected) == actual
}

/// Case-insensitive substring either way (the Python cached item/desc match).
fn item_desc_matches(actual: &str, expected: &str) -> bool {
    let (a, e) = (actual.to_uppercase(), expected.to_uppercase());
    !e.is_empty() && (a.contains(&e) || e.contains(&a))
}

/// Expected `category` is an account; parsed `category` is an internal key.
/// Match on substring, else resolve both through the account mapping and compare.
fn category_matches(expected: &str, actual: &str, mapping: &HashMap<String, String>) -> bool {
    let (e, a) = (expected.to_uppercase(), actual.to_uppercase());
    if e.contains(&a) || a.contains(&e) {
        return true;
    }
    resolve_account_target(Some(expected), mapping, Some(expected))
        == resolve_account_target(Some(actual), mapping, Some(actual))
}

// --- fixtures ------------------------------------------------------------------

/// Recursively collect `*.expected.json` under `dir`.
fn collect_expected(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_expected(&path, out);
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".expected.json"))
        {
            out.push(path);
        }
    }
}

/// Which top-level subdirectories of a corpus root a run covers.
///
/// The private corpus is grouped one directory per merchant, and each merchant
/// gets its own `#[test]` so libtest runs them across cores and a CI failure
/// names the merchant. `Excluding` is the catch-all that keeps that split
/// *total*: any merchant directory not claimed by a named test still runs.
pub enum Selection<'a> {
    /// Everything under the root (the vendored public corpus).
    All,
    /// Only this top-level subdirectory.
    Dir(&'a str),
    /// Every top-level subdirectory NOT named here, plus loose root-level cases.
    Excluding(&'a [&'a str]),
}

impl Selection<'_> {
    fn takes_dir(&self, name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Dir(d) => name == *d,
            Self::Excluding(named) => !named.contains(&name),
        }
    }

    /// Loose `*.expected.json` sitting directly in the root belong to whichever
    /// selection is the catch-all, so they can never fall through the split.
    fn takes_root_files(&self) -> bool {
        matches!(self, Self::All | Self::Excluding(_))
    }
}

/// Collect the `*.expected.json` under `root` that `sel` covers. Filtering
/// applies to the top level only; a selected directory is walked in full.
fn collect_selected(root: &Path, sel: &Selection, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if sel.takes_dir(&name) {
                collect_expected(&path, out);
            }
        } else if sel.takes_root_files() && name.ends_with(".expected.json") {
            out.push(path);
        }
    }
}

/// Top-level subdirectory names under `root`, sorted. Lets the catch-all test
/// report which merchants it swept up, so a corpus that grows a new merchant is
/// visible in the CI log rather than silently folded into `other`.
pub fn top_level_dirs(root: &Path) -> Vec<String> {
    let mut dirs: Vec<String> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    dirs
}

/// Parse the raw PaddleOCR `.ocr.json` (`{image_width, image_height,
/// detections:[[points], [text, conf]]}`) into detections + padded dims. Mirrors
/// the PyO3 `extract_detections` / `receipt_process_receipt`.
fn detections_from_ocr(raw: &Value) -> (Vec<RawDetection>, i64, i64) {
    let w = raw["image_width"].as_i64().expect("image_width");
    let h = raw["image_height"].as_i64().expect("image_height");
    let mut dets = Vec::new();
    if let Some(list) = raw["detections"].as_array() {
        for entry in list {
            let Some(fields) = entry.as_array() else {
                continue;
            };
            if fields.len() < 2 {
                continue;
            }
            let points = fields[0].as_array().map_or_else(Vec::new, |pts| {
                pts.iter()
                    .filter_map(|p| {
                        let p = p.as_array()?;
                        Some((p.first()?.as_f64()?, p.get(1)?.as_f64()?))
                    })
                    .collect()
            });
            let text_conf = fields[1].as_array();
            let text = text_conf
                .and_then(|tc| tc.first())
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let confidence = text_conf
                .and_then(|tc| tc.get(1))
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            dets.push(RawDetection {
                points,
                text,
                confidence,
            });
        }
    }
    (dets, w, h)
}

fn str_set(v: &Value, key: &str) -> HashSet<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Outcome of a corpus run.
pub struct CorpusResult {
    /// Number of cached cases (`.expected.json` with a sibling `.ocr.json`) run.
    pub ran: usize,
    /// One human-readable line per divergence from expected (empty == all good).
    pub failures: Vec<String>,
}

/// Run the cached corpus under `receipts_dir` (a directory of `<stem>.ocr.json` +
/// `<stem>.expected.json` pairs, possibly nested), layering the optional
/// `overrides` (raw `private_rules.toml` contents) over the bundled public rules.
/// An empty `overrides` slice ⇒ public rules only.
pub fn run_cached_corpus(receipts_dir: &Path, overrides: &[&str]) -> CorpusResult {
    run_cached_corpus_in(receipts_dir, overrides, &Selection::All)
}

/// [`run_cached_corpus`], restricted to the part of the corpus `sel` covers.
/// Case ids stay relative to `receipts_dir`, so a failure reads the same
/// (`costco/<stem>: …`) whether the run was sliced or whole.
pub fn run_cached_corpus_in(
    receipts_dir: &Path,
    overrides: &[&str],
    sel: &Selection,
) -> CorpusResult {
    let layers = parser_rule_layers_with_overrides(overrides)
        .unwrap_or_else(|e| panic!("override classifier TOML: {e}"));
    let mapping: HashMap<String, String> = layers.account_mapping.iter().cloned().collect();
    let merchants = default_known_merchants();
    let merchant_families = default_merchant_families();

    let mut expected_files = Vec::new();
    collect_selected(receipts_dir, sel, &mut expected_files);
    expected_files.sort();

    let mut ran = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for ep in &expected_files {
        let file = ep.file_name().and_then(|n| n.to_str()).unwrap();
        let stem = file.strip_suffix(".expected.json").unwrap();
        let ocr_path = ep.with_file_name(format!("{stem}.ocr.json"));
        if !ocr_path.exists() {
            continue; // cached mode needs the OCR snapshot
        }
        ran += 1;
        let rel_dir = ep.parent().and_then(|p| p.strip_prefix(receipts_dir).ok());
        let id = match rel_dir {
            Some(d) if !d.as_os_str().is_empty() => format!("{}/{stem}", d.display()),
            _ => stem.to_string(),
        };

        let expected: Value = serde_json::from_str(&fs::read_to_string(ep).unwrap()).unwrap();
        let raw: Value = serde_json::from_str(&fs::read_to_string(&ocr_path).unwrap()).unwrap();
        let (dets, w, h) = detections_from_ocr(&raw);
        let ocr = transform(dets, w, h, PADDING);
        let parsed = parse_receipt(
            &ocr,
            &layers,
            &format!("{stem}.jpg"),
            &merchants,
            &merchant_families,
            TODAY_YEAR,
        );

        // A tolerated divergence describes ONE engine at ONE version, and this
        // corpus is replayed by four consumers on four different core pins —
        // desktop sits on v0.3.2, the phone apps on their own tags and against
        // live OCR rather than these snapshots. A flat marker asserts one
        // engine's failures against all of them. So the bare key still means
        // "every consumer" and `known_failures_core` is ours; we read the union
        // and ignore the other consumers' keys. Schema lives in
        // beanbeaver-private-test's CLAUDE.md.
        let mut known = str_set(&expected, "known_failures");
        known.extend(str_set(&expected, "known_failures_core"));
        let mut failed: HashSet<&'static str> = HashSet::new();
        let mut case_fail: Vec<String> = Vec::new();

        // merchant (optional/any_of tolerated inline, like the Python harness)
        if let Some(m) = expected.get("merchant").and_then(Value::as_str) {
            let optional = expected
                .get("merchant_optional")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let any_of = str_set(&expected, "merchant_any_of");
            let ok = merchant_matches(m, &parsed.merchant)
                || any_of
                    .iter()
                    .any(|alt| merchant_matches(alt, &parsed.merchant));
            if !ok && !optional {
                failed.insert("merchant");
                if !known.contains("merchant") {
                    case_fail.push(format!(
                        "merchant expected '{m}', got '{}'",
                        parsed.merchant
                    ));
                }
            }
        }

        // Merchant contact/branch details are opt-in fixture assertions. They
        // stay out of Beancount and UI, but the private corpus records what the
        // receipt actually printed so extraction regressions are visible.
        if let Some(details) = expected.get("merchant_details").and_then(Value::as_object) {
            for (key, actual) in [
                ("street_address", &parsed.merchant_details.street_address),
                ("city", &parsed.merchant_details.city),
                ("region", &parsed.merchant_details.region),
                ("postal_code", &parsed.merchant_details.postal_code),
                ("phone_number", &parsed.merchant_details.phone_number),
                ("store_number", &parsed.merchant_details.store_number),
            ] {
                let Some(want) = details.get(key) else {
                    continue;
                };
                let matches = match want {
                    Value::Null => actual.is_none(),
                    Value::String(want) => actual
                        .as_deref()
                        .is_some_and(|actual| detail_matches(key, want, actual)),
                    _ => panic!("merchant_details.{key} must be a string or null"),
                };
                if !matches {
                    failed.insert("merchant_details");
                    if !known.contains("merchant_details") {
                        case_fail.push(format!(
                            "merchant_details.{key} expected {want}, got {actual:?}"
                        ));
                    }
                }
            }
        }

        // date (exact)
        if let Some(dt) = expected.get("date").and_then(Value::as_str) {
            let actual = parsed.date.map(|d| d.to_string());
            if actual.as_deref() != Some(dt) {
                failed.insert("date");
                if !known.contains("date") {
                    case_fail.push(format!("date expected '{dt}', got {actual:?}"));
                }
            }
        }

        // total (exact/decimal) — always checked
        if let Some(t) = expected.get("total").and_then(Value::as_str) {
            if !price_matches(t, parsed.total) {
                failed.insert("total");
                if !known.contains("total") {
                    case_fail.push(format!("total expected '{t}', got '{}'", parsed.total));
                }
            }
        }

        // subtotal / tax (exact/decimal). Printed on nearly every receipt, so
        // they are cheap to author from the photo and unambiguous. `subtotal`
        // earns its place independently of `total`: a mis-priced item shifts it
        // while leaving the total — which the parser reads directly — intact.
        for (key, actual) in [("subtotal", &parsed.subtotal), ("tax", &parsed.tax)] {
            let Some(want) = expected.get(key).and_then(Value::as_str) else {
                continue;
            };
            let ok = actual.is_some_and(|a| price_matches(want, a));
            if !ok {
                failed.insert(key);
                if !known.contains(key) {
                    case_fail.push(format!("{key} expected '{want}', got {actual:?}"));
                }
            }
        }

        // line_item_count — how many line items the parse should emit.
        //
        // This is the only check that can see a line the parser INVENTED or
        // DROPPED. `critical_items` is a subset assertion: every expected item
        // must be found, but extra items are not flagged and nothing counts
        // them, so a receipt whose amount is claimed twice still passes. Home
        // Hardware 2026-08-09 is the case — two items on paper, three emitted,
        // 9.99 claimed twice, and the fixture green.
        //
        // It counts LINE ITEMS THE PARSER EMITS, not units purchased, and it
        // includes discount/negative lines. Deliberately not the "Item Count"
        // some receipts print: Foody Mart 2026-08-15 prints `Item Count: 15`
        // for a parse of 11 lines, because `2 @ $3.59` is one line and two
        // units. Count the lines on the photo, never a printed total and never
        // a parser dump.
        if let Some(want) = expected.get("line_item_count").and_then(Value::as_u64) {
            let got = parsed.items.len() as u64;
            if got != want {
                failed.insert("line_item_count");
                if !known.contains("line_item_count") {
                    case_fail.push(format!("line_item_count expected {want}, got {got}"));
                }
            }
        }

        // critical items — description + price + HARD category. Each item may
        // carry `"known_failure": true` to tolerate *its own* divergence (finer
        // than the check-level `known_failures: ["critical_items"]`, which
        // umbrellas the whole block); a marked item that unexpectedly matches is
        // reported so the stale marker gets removed.
        if let Some(items) = expected.get("critical_items").and_then(Value::as_array) {
            let mut msgs: Vec<String> = Vec::new();
            let mut real_failure = false;
            for ci in items {
                assert!(
                    ci.get("category_optional").is_none(),
                    "{id}: 'category_optional' is banned; drop the category or add a private_rules.toml rule"
                );
                let desc = ci
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let price = ci.get("price").and_then(Value::as_str).unwrap_or_default();
                let want_cat = ci.get("category").and_then(Value::as_str);
                // Bare key = every consumer; `_core` = ours only. See the
                // note on `known_failures` above.
                let item_known = ci
                    .get("known_failure")
                    .or_else(|| ci.get("known_failure_core"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let matched: Vec<_> = parsed
                    .items
                    .iter()
                    .filter(|it| item_desc_matches(&it.description, desc))
                    .collect();
                let price_ok = matched.iter().any(|it| price_matches(price, it.price));
                let cat_ok = want_cat.map_or(true, |c| {
                    matched
                        .iter()
                        .filter(|it| price_matches(price, it.price))
                        .any(|it| {
                            it.category
                                .as_deref()
                                .is_some_and(|k| category_matches(c, k, &mapping))
                        })
                });
                let item_failed = matched.is_empty() || !price_ok || !cat_ok;
                if item_failed && !item_known {
                    let got: Vec<_> = matched
                        .iter()
                        .map(|it| {
                            (
                                it.description.as_str(),
                                it.price.to_string(),
                                it.category.as_deref(),
                            )
                        })
                        .collect();
                    msgs.push(format!("item '{desc}' (price {price}, cat {want_cat:?}) unmatched; candidates {got:?}"));
                    real_failure = true;
                } else if !item_failed && item_known {
                    msgs.push(format!(
                        "item '{desc}' marked known_failure but matched — remove the marker"
                    ));
                }
            }
            if real_failure {
                failed.insert("critical_items");
            }
            if !known.contains("critical_items") {
                for m in msgs {
                    case_fail.push(m);
                }
            }
        }

        // tenders — how the receipt was PAID, positionally: kind + amount, in
        // printed order. Checked only when the key is present.
        //
        // Worth asserting separately from `total` because the two fail
        // independently and only one of them was ever visible. A split-tender
        // receipt can report the right grand total while losing a tender line
        // entirely (costco_46668 drops its MasterCard to a merged CHANGE row),
        // or invent one that isn't a payment at all (freshco's `Gift Card
        // Balance:` echo). Both look perfect through a total-only assertion.
        //
        // The count is part of the check: a spurious tender is exactly the
        // defect worth catching, so an extra parsed tender fails even when
        // every expected one matched. Like `critical_items`, an entry may carry
        // `"known_failure": true` for its own index; the check-level
        // `known_failures: ["tenders"]` umbrellas the whole block, including
        // the count.
        if let Some(want) = expected.get("tenders").and_then(Value::as_array) {
            let mut msgs: Vec<String> = Vec::new();
            let mut real_failure = false;
            for (i, wt) in want.iter().enumerate() {
                let want_amount = wt.get("amount").and_then(Value::as_str).unwrap_or_default();
                let want_kind = wt.get("kind").and_then(Value::as_str);
                let entry_known = wt
                    .get("known_failure")
                    .or_else(|| wt.get("known_failure_core"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let got = parsed.tenders.get(i);
                let entry_failed = match got {
                    None => true,
                    Some(t) => {
                        !price_matches(want_amount, t.amount)
                            || want_kind.is_some_and(|k| k != t.kind)
                    }
                };
                if entry_failed && !entry_known {
                    real_failure = true;
                    msgs.push(match got {
                        None => format!(
                            "tender[{i}] expected {want_kind:?} {want_amount}, but only {} tender(s) parsed",
                            parsed.tenders.len()
                        ),
                        Some(t) => format!(
                            "tender[{i}] expected {want_kind:?} {want_amount}, got \"{}\" {} ({})",
                            t.kind, t.amount, t.raw_label
                        ),
                    });
                } else if !entry_failed && entry_known {
                    msgs.push(format!(
                        "tender[{i}] marked known_failure but matched — remove the marker"
                    ));
                }
            }
            for (i, extra) in parsed.tenders.iter().enumerate().skip(want.len()) {
                real_failure = true;
                msgs.push(format!(
                    "tender[{i}] unexpected: \"{}\" {} ({}) — receipt prints {} tender(s)",
                    extra.kind,
                    extra.amount,
                    extra.raw_label,
                    want.len()
                ));
            }
            if real_failure {
                failed.insert("tenders");
            }
            if !known.contains("tenders") {
                for m in msgs {
                    case_fail.push(m);
                }
            }
        }

        // A known_failure that unexpectedly passed must be removed.
        for k in &known {
            if !failed.contains(k.as_str()) {
                case_fail.push(format!(
                    "known_failure '{k}' unexpectedly passed — remove the marker"
                ));
            }
        }

        for m in case_fail {
            failures.push(format!("{id}: {m}"));
        }
    }

    CorpusResult { ran, failures }
}
