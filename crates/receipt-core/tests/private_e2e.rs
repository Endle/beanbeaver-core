//! Cached private E2E: run the out-of-tree private receipt corpus against
//! receipt-core's parser, using the bundled PUBLIC rules plus the corpus's
//! transitional `private_rules.toml` overrides. Mirrors the desktop Python
//! `test_e2e_receipts.py --e2e-mode cached`: raw `.ocr.json` -> transform ->
//! parse -> assert (merchant fuzzy / date exact / total exact / critical items),
//! honoring `known_failures`. Item `category` is a HARD assertion — the
//! `category_optional` escape is banned (mirrors the desktop harness).
//!
//! The corpus lives in a separate PRIVATE repo (real receipts contain PII), so it
//! is NOT vendored here. Point `BEANBEAVER_PRIVATE_TESTS_DIR` at a checkout of it
//! (CI clones it with a read-only token — see
//! `.github/workflows/private-regression.yml`). When the env var is unset the
//! test SKIPS, so plain `cargo test` stays green without the private repo.
//!
//! The matching/assertion engine is shared with `public_e2e.rs`; see
//! `tests/e2e_harness/mod.rs`.

mod e2e_harness;

use std::fs;
use std::path::PathBuf;

use e2e_harness::run_cached_corpus;

#[test]
fn private_cached_e2e() {
    let Some(root) = std::env::var_os("BEANBEAVER_PRIVATE_TESTS_DIR") else {
        eprintln!("SKIP private_cached_e2e: BEANBEAVER_PRIVATE_TESTS_DIR unset");
        return;
    };
    let root = PathBuf::from(root);
    let fixtures = root.join("receipts_e2e");
    assert!(
        fixtures.is_dir(),
        "no receipts_e2e/ under {}",
        root.display()
    );

    // Overrides: the corpus's transitional `private_rules.toml`. Absent => public
    // rules only (the future pure-data state).
    let private_rules = fs::read_to_string(root.join("private_rules.toml")).ok();
    let overrides: Vec<&str> = private_rules.as_deref().into_iter().collect();

    let result = run_cached_corpus(&fixtures, &overrides);

    eprintln!(
        "private_cached_e2e: ran {} cached case(s), {} divergence(s)",
        result.ran,
        result.failures.len()
    );
    for f in &result.failures {
        eprintln!("  ✗ {f}");
    }
    assert!(
        result.failures.is_empty(),
        "{} private cached check(s) diverged from expected",
        result.failures.len()
    );
    assert!(
        result.ran > 0,
        "no cached (.ocr.json) fixtures were executed"
    );
}
