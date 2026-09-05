//! Measurement helper (not a gate) — prints per-fixture pairing health so a
//! line-grouping change can be compared before/after across the *whole* cached
//! corpus, not just where `critical_items` happens to assert.
//!
//! The corpus asserts a handful of items per receipt, so a grouping change can
//! shift dozens of item/price pairings without moving a single assertion. These
//! two invariants catch that: the item column should sum to the printed
//! subtotal, and subtotal + tax should equal the printed total. Neither is
//! asserted here — receipts legitimately fail both today — so diff two runs
//! rather than reading one.
//!
//! Ignored so it never runs as part of `cargo test`:
//!
//! ```text
//! BEANBEAVER_PRIVATE_TESTS_DIR=../beanbeaver-private-test \
//!   cargo test --release -p receipt-core --test pairing_metrics -- --ignored --nocapture
//! ```

mod e2e_harness;

use std::fs;
use std::path::{Path, PathBuf};

use receipt_core::ocr_transform::{transform, RawDetection, RawDetectionPage};
use receipt_core::parser::parse_receipt;
use receipt_core::rules::{
    default_known_merchants, default_merchant_families, parser_rule_layers_with_overrides,
};
use serde_json::Value;

const TODAY_YEAR: i32 = 2026;
const PADDING: i64 = 50;

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
            let tc = fields[1].as_array();
            let text = tc
                .and_then(|t| t.first())
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let confidence = tc
                .and_then(|t| t.get(1))
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

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".expected.json"))
        {
            out.push(p);
        }
    }
}

fn run_dir(root: &Path, overrides: &[&str], label: &str) {
    let layers = parser_rule_layers_with_overrides(overrides).expect("rules");
    let merchants = default_known_merchants();
    let families = default_merchant_families();

    let mut files = Vec::new();
    collect(root, &mut files);
    files.sort();

    let (mut n, mut items_total, mut sum_ok, mut sum_seen, mut tot_ok, mut tot_seen) =
        (0, 0usize, 0, 0, 0, 0);
    for ep in &files {
        let stem = ep
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap()
            .strip_suffix(".expected.json")
            .unwrap();
        let ocr_path = ep.with_file_name(format!("{stem}.ocr.json"));
        if !ocr_path.exists() {
            continue;
        }
        n += 1;
        let raw: Value = serde_json::from_str(&fs::read_to_string(&ocr_path).unwrap()).unwrap();
        let (dets, w, h) = detections_from_ocr(&raw);
        let page = RawDetectionPage::try_new(dets, w, h, PADDING)
            .unwrap_or_else(|e| panic!("{stem}: cached detections are not a valid page: {e}"));
        let ocr = transform(page);
        let p = parse_receipt(
            &ocr,
            &layers,
            &format!("{stem}.jpg"),
            &merchants,
            &families,
            TODAY_YEAR,
        );

        // Kept in f64 with the original 0.02 tolerance: this is a reporting
        // metric, and switching it to exact integer arithmetic would change what
        // it counts. That is a separate decision from the Money refactor.
        let dollars = |m: receipt_core::money::Money| m.cents() as f64 / 100.0;
        let sum: f64 = p.items.iter().map(|i| dollars(i.price)).sum();
        let subtotal = p.subtotal.map(dollars);
        let tax = p.tax.map(dollars);
        let total = Some(dollars(p.total));

        // Does the item column reconcile with the printed subtotal?
        let s_ok = match subtotal {
            Some(st) => {
                sum_seen += 1;
                let ok = (sum - st).abs() < 0.02;
                if ok {
                    sum_ok += 1;
                }
                Some(ok)
            }
            None => None,
        };
        // Does subtotal + tax reconcile with the printed total?
        let t_ok = match (subtotal, tax, total) {
            (Some(st), Some(tx), Some(t)) => {
                tot_seen += 1;
                let ok = (st + tx - t).abs() < 0.02;
                if ok {
                    tot_ok += 1;
                }
                Some(ok)
            }
            _ => None,
        };
        items_total += p.items.len();

        let flag = |o: Option<bool>| match o {
            Some(true) => "ok  ",
            Some(false) => "BAD ",
            None => "-   ",
        };
        println!(
            "{label} {:<48} items={:<3} sum={:<9.2} sub={:<9} tot={:<9} sum/sub={} sub+tax/tot={}",
            stem,
            p.items.len(),
            sum,
            p.subtotal
                .map(|m| m.to_string())
                .unwrap_or_else(|| "-".into()),
            p.total,
            flag(s_ok),
            flag(t_ok),
        );
    }
    println!(
        "{label} TOTALS cases={n} items={items_total} sum/sub={sum_ok}/{sum_seen} sub+tax/tot={tot_ok}/{tot_seen}"
    );
}

#[test]
#[ignore = "measurement helper, not a gate — see module docs"]
fn pairing_metrics() {
    let public = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/receipts_e2e");
    run_dir(&public, &[], "PUB");

    if let Some(root) = std::env::var_os("BEANBEAVER_PRIVATE_TESTS_DIR") {
        let root = PathBuf::from(root);
        let private_rules = fs::read_to_string(root.join("private_rules.toml")).ok();
        let overrides: Vec<&str> = private_rules.as_deref().into_iter().collect();
        run_dir(&root.join("receipts_e2e"), &overrides, "PRV");
    }
}
