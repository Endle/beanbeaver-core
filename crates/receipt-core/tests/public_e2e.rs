//! Cached public E2E: run the vendored redacted receipt corpus against
//! receipt-core using the bundled PUBLIC rules ONLY. This is the self-contained,
//! always-on analog of the desktop repo's `test_receipt_core_parity.py` gate:
//! raw `.ocr.json` -> transform -> parse -> assert against `.expected.json`.
//!
//! Unlike `private_e2e.rs`, the corpus is checked in under
//! `tests/receipts_e2e/` (real receipts, but redacted/censored — no PII) and
//! needs no token, so it runs on every `cargo test`. No `private_rules.toml`
//! overrides are applied: this proves the corpus passes on public rules alone.
//!
//! The matching/assertion engine is shared with `private_e2e.rs`; see
//! `tests/e2e_harness/mod.rs`.

mod e2e_harness;

use std::path::Path;

use e2e_harness::run_cached_corpus;

#[test]
fn public_cached_e2e() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("receipts_e2e");
    assert!(
        fixtures.is_dir(),
        "vendored corpus missing at {}",
        fixtures.display()
    );

    // Public rules only — no overrides. The vendored corpus MUST pass without any
    // private_rules.toml, which is exactly what keeps core on public rules alone.
    let result = run_cached_corpus(&fixtures, &[]);

    eprintln!(
        "public_cached_e2e: ran {} cached case(s), {} divergence(s)",
        result.ran,
        result.failures.len()
    );
    for f in &result.failures {
        eprintln!("  ✗ {f}");
    }
    assert!(
        result.failures.is_empty(),
        "{} public cached check(s) diverged from expected",
        result.failures.len()
    );
    assert!(
        result.ran > 0,
        "no cached (.ocr.json) fixtures were executed"
    );
}
