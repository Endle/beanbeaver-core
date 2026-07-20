//! Live public E2E: run the FULL on-device pipeline (image -> ONNX det/rec/cls
//! -> `receipt-core` parse -> beancount) over the vendored redacted receipt
//! corpus, on real hardware. This is the always-on companion to the cached
//! `public_e2e` in `receipt-core`: cached proves the parser is stable against a
//! frozen OCR snapshot; this proves the whole stack actually *runs* end to end.
//!
//!   cargo test -p ocr-paddle --test public_live_e2e -- --nocapture
//!
//! Contract (live OCR is nondeterministic across platforms; PP-OCRv5 mobile
//! output can differ mac vs. linux):
//!   * HARD (pipeline): every run fixture must finish — `process_image` Ok and
//!     non-empty beancount.
//!   * HARD (parse fixtures): a fixed short list of fixtures must match
//!     merchant + total against `.expected.json` with stricter merchant
//!     matching and price tolerance. Catches real stack breakage without
//!     requiring full corpus determinism. Runs on every CI OS (Linux + macOS);
//!     keep the list short and historically stable to limit flake surface.
//!   * SOFT: other merchant / date / total / critical_items divergences on the
//!     random sample are WARNING only. Stricter ledger: `phase5_e2e.rs`
//!     (`--ignored`, `KNOWN_ON_DEVICE_GAPS`).
//!
//! Random sample: default 2 fixtures (`LIVE_E2E_COUNT` / `LIVE_E2E_SEED`).
//! Hard-parse fixtures always run in addition to the random sample.
//! Pin `LIVE_E2E_SEED` in CI when bisecting soft-sample flakiness.
//!
//! Skips (does not fail) when the ONNX models are absent, so a plain
//! `cargo test -p ocr-paddle` stays green without `models/` provisioned. CI
//! downloads them from the `ocr-models-v1` release first.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ocr_paddle::engine::OcrEngine;
use ocr_paddle::process::process_image;
use receipt_core::receipt_categories::resolve_account_target;
use receipt_core::rules::default_parser_rule_layers;
use serde_json::Value;

/// Reference "today"; the corpus uses explicit full dates, so this is inert.
const TODAY: (i32, u32, u32) = (2026, 7, 2);
const CREDIT_CARD_ACCOUNT: &str = "Liabilities:CreditCard";

/// Fixed fixtures that **must** hard-pass merchant + total on live OCR
/// (not an allow-to-fail list). Keep short and historically stable; expand only
/// when a fixture has proven reliable across Linux/macOS CPU ORT in CI.
const HARD_PARSE_FIXTURES: &[&str] = &[
    "costco_20260218_redact",
    "walmart_20260218_redact",
    "freshco_redact",
];

fn manifest_rel(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

// --- tolerant matchers (same semantics as phase5_e2e.rs / the Python harness) --

fn normalize_merchant(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect()
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

/// Soft merchant check: normalized substring either way, else similarity >= 0.85.
fn merchant_matches(expected: &str, actual: &str) -> bool {
    let (e, a) = (normalize_merchant(expected), normalize_merchant(actual));
    if e.is_empty() || a.is_empty() {
        return false;
    }
    if a.contains(&e) || e.contains(&a) {
        return true;
    }
    merchant_similarity(&e, &a) >= 0.85
}

fn merchant_similarity(e: &str, a: &str) -> f64 {
    let maxlen = e.len().max(a.len());
    if maxlen == 0 {
        return 0.0;
    }
    (maxlen - levenshtein(e.as_bytes(), a.as_bytes())) as f64 / maxlen as f64
}

/// Hard merchant check: reject one-character / tiny substring short-circuits.
/// Accept exact normalized equality, substantial containment (min 4 chars or
/// full expected length), or similarity >= 0.85.
fn merchant_matches_hard(expected: &str, actual: &str) -> bool {
    let (e, a) = (normalize_merchant(expected), normalize_merchant(actual));
    if e.is_empty() || a.is_empty() {
        return false;
    }
    if e == a {
        return true;
    }
    let min_contain = 4.min(e.len()).min(a.len());
    if min_contain > 0 && (a.contains(&e) || e.contains(&a)) {
        // Require the shorter side to be at least `min_contain` so "C" ⊄ hard-pass.
        let shorter = e.len().min(a.len());
        if shorter >= min_contain {
            return true;
        }
    }
    merchant_similarity(&e, &a) >= 0.85
}

/// Decimal-equal (expected "6.97" vs on-device "6.9700").
fn price_matches(expected: &str, actual: &str) -> bool {
    match (expected.parse::<f64>(), actual.parse::<f64>()) {
        (Ok(e), Ok(a)) => (e - a).abs() < 0.005,
        _ => expected == actual,
    }
}

/// Uppercase, collapse letter-O / digit-0, strip a leading item code, drop spaces.
fn normalize_item(s: &str) -> String {
    let upper: String = s.to_uppercase().replace('O', "0");
    let stripped = match upper.find(|c: char| !c.is_ascii_digit()) {
        Some(i) if i > 0 && upper[i..].starts_with(char::is_whitespace) => upper[i..].trim_start(),
        _ => upper.as_str(),
    };
    stripped.chars().filter(|c| !c.is_whitespace()).collect()
}

fn item_desc_matches(actual: &str, expected: &str) -> bool {
    let (a, e) = (normalize_item(actual), normalize_item(expected));
    !e.is_empty() && (a.contains(&e) || e.contains(&a))
}

/// Expected `category` is an account; parsed `category` is an internal key.
fn category_matches(expected: &str, actual: &str, mapping: &HashMap<String, String>) -> bool {
    let (e, a) = (expected.to_uppercase(), actual.to_uppercase());
    if e.contains(&a) || a.contains(&e) {
        return true;
    }
    resolve_account_target(Some(expected), mapping, Some(expected))
        == resolve_account_target(Some(actual), mapping, Some(actual))
}

// --- randomized pick (no rng dependency) --------------------------------------

/// Choose `k` distinct indices in `[0, n)` via a seeded partial Fisher-Yates.
/// `seed` comes from `LIVE_E2E_SEED` when set, else wall-clock nanos, so the pick
/// rotates run-to-run but stays reproducible on demand.
fn pick_indices(n: usize, k: usize, seed: u64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    let mut state = seed | 1; // xorshift* must be non-zero
    let mut next = || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545F4914F6CDD1D)
    };
    let take = k.min(n);
    for i in 0..take {
        let j = i + (next() as usize) % (n - i);
        order.swap(i, j);
    }
    let mut chosen = order[..take].to_vec();
    chosen.sort_unstable();
    chosen
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok().and_then(|v| v.parse().ok())
}

#[test]
fn public_live_e2e() {
    let fixtures = manifest_rel("../receipt-core/tests/receipts_e2e");
    let models = manifest_rel("../../models");
    let (det, rec, cls) = (
        models.join("PP-OCRv5_mobile_det.onnx"),
        models.join("PP-OCRv5_mobile_rec.onnx"),
        models.join("PP-LCNet_x1_0_textline_ori.onnx"),
    );
    if !det.exists() || !rec.exists() || !cls.exists() {
        eprintln!(
            "SKIP public_live_e2e: OCR models missing under {} (download the ocr-models-v1 release)",
            models.display()
        );
        return;
    }

    // Fixtures runnable live: those shipping both an image and an expectation.
    let mut names: Vec<String> = fs::read_dir(&fixtures)
        .expect("read fixtures dir")
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".expected.json").map(str::to_string))
        .filter(|stem| fixtures.join(format!("{stem}.jpg")).exists())
        .collect();
    names.sort();
    assert!(
        !names.is_empty(),
        "no live fixtures (.jpg + .expected.json) under {}",
        fixtures.display()
    );

    let count = env_u64("LIVE_E2E_COUNT").unwrap_or(2) as usize;
    let seed = env_u64("LIVE_E2E_SEED").unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    });
    let mut picks: Vec<String> = pick_indices(names.len(), count, seed)
        .into_iter()
        .map(|i| names[i].clone())
        .collect();

    // Always exercise the hard-parse fixtures (in addition to the random sample).
    for stem in HARD_PARSE_FIXTURES {
        assert!(
            names.iter().any(|n| n == *stem),
            "hard-parse fixture missing from corpus: {stem}"
        );
        if !picks.iter().any(|n| n == *stem) {
            picks.push((*stem).to_string());
        }
    }
    picks.sort();
    picks.dedup();
    eprintln!(
        "public_live_e2e: seed={seed}, fixtures {:?} ({} total; hard-parse {:?})",
        picks,
        picks.len(),
        HARD_PARSE_FIXTURES
    );

    let mut engine = OcrEngine::from_paths(&det, &rec, Some(&cls)).expect("load PP-OCRv5 models");
    let account_mapping: HashMap<String, String> = default_parser_rule_layers()
        .account_mapping
        .into_iter()
        .collect();

    let mut warnings: Vec<String> = Vec::new();
    let mut ran = 0usize;

    for name in &picks {
        let jpg = fixtures.join(format!("{name}.jpg"));
        let img = image::open(&jpg)
            .unwrap_or_else(|e| panic!("decode {name}.jpg: {e}"))
            .to_rgb8();

        // HARD gate: the full pipeline must finish successfully.
        let processed = process_image(
            &mut engine,
            &img,
            &format!("{name}.jpg"),
            TODAY,
            CREDIT_CARD_ACCOUNT,
            "CAD",
            "Expenses:Tax:HST",
            None,
        )
        .unwrap_or_else(|e| panic!("{name}: pipeline did not finish: {e}"));
        assert!(
            !processed.beancount.trim().is_empty(),
            "{name}: pipeline finished but produced no beancount output",
        );
        ran += 1;

        let d = &processed.parsed;
        eprintln!(
            "✓ {name} ran: merchant={:?} date={:?} total={:?} items={}",
            d.merchant,
            d.date.map(|(y, m, dd)| format!("{y:04}-{m:02}-{dd:02}")),
            d.total,
            d.items.len(),
        );

        let expected: Value = serde_json::from_str(
            &fs::read_to_string(fixtures.join(format!("{name}.expected.json"))).unwrap(),
        )
        .unwrap();
        let hard = HARD_PARSE_FIXTURES.iter().any(|s| *s == name.as_str());

        if hard {
            // HARD parse gate: both merchant and total are required on the
            // expectation, then must match (stricter merchant + price tolerance).
            let m = expected
                .get("merchant")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("{name}: hard-parse fixture missing string 'merchant' in .expected.json")
                });
            let t = expected
                .get("total")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("{name}: hard-parse fixture missing string 'total' in .expected.json")
                });
            assert!(
                merchant_matches_hard(m, &d.merchant),
                "{name}: HARD merchant expected '{m}', got '{}'",
                d.merchant
            );
            assert!(
                price_matches(t, &d.total),
                "{name}: HARD total expected '{t}', got '{}'",
                d.total
            );
            eprintln!("  HARD parse ok: merchant+total");
        }

        // SOFT gate: compare to the baseline; divergence -> warning, never failure
        // (except hard-parse merchant/total already asserted above).
        let mut warn = |msg: String| warnings.push(format!("{name}: {msg}"));

        if let Some(m) = expected.get("merchant").and_then(Value::as_str) {
            if !hard && !merchant_matches(m, &d.merchant) {
                warn(format!("merchant expected '{m}', got '{}'", d.merchant));
            }
        }
        if let Some(dt) = expected.get("date").and_then(Value::as_str) {
            let actual = d.date.map(|(y, m, dd)| format!("{y:04}-{m:02}-{dd:02}"));
            if actual.as_deref() != Some(dt) {
                warn(format!("date expected '{dt}', got {actual:?}"));
            }
        }
        if let Some(t) = expected.get("total").and_then(Value::as_str) {
            if !hard && !price_matches(t, &d.total) {
                warn(format!("total expected '{t}', got '{}'", d.total));
            }
        }
        if let Some(items) = expected.get("critical_items").and_then(Value::as_array) {
            for ci in items {
                let desc = ci
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let price = ci.get("price").and_then(Value::as_str).unwrap_or_default();
                let want_cat = ci.get("category").and_then(Value::as_str);
                let matched: Vec<_> = d
                    .items
                    .iter()
                    .filter(|it| item_desc_matches(&it.description, desc))
                    .collect();
                let price_ok = matched.iter().any(|it| price_matches(price, &it.price));
                let cat_ok = want_cat.is_none_or(|c| {
                    matched
                        .iter()
                        .filter(|it| price_matches(price, &it.price))
                        .any(|it| {
                            it.category
                                .as_deref()
                                .is_some_and(|k| category_matches(c, k, &account_mapping))
                        })
                });
                if matched.is_empty() || !price_ok || !cat_ok {
                    warn(format!(
                        "item '{desc}' (price {price}, cat {want_cat:?}) not reproduced live"
                    ));
                }
            }
        }
    }

    eprintln!(
        "\npublic_live_e2e: {ran} fixture(s) ran end-to-end, {} soft warning(s)",
        warnings.len()
    );
    for w in &warnings {
        eprintln!("  ⚠ {w}");
    }

    assert_eq!(
        ran,
        picks.len(),
        "not every picked fixture completed the pipeline"
    );
    assert!(ran > 0, "no fixtures ran");
}
