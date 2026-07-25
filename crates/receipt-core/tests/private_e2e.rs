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
//! tests SKIP, so plain `cargo test` stays green without the private repo.
//!
//! **One test per merchant.** The corpus is grouped one directory per merchant
//! under `receipts_e2e/`, and each gets its own `#[test]`. Two reasons:
//!
//! 1. libtest runs `#[test]`s on a thread pool, so the corpus uses every core on
//!    the runner instead of one. (A single loop over all cases did not.)
//! 2. A CI failure reports `private_cached_e2e_costco ... FAILED` — the merchant
//!    is in the test name, not buried in one long log.
//!
//! Merchants too small to be worth their own signal are swept up by
//! [`private_cached_e2e_other`], which covers everything [`NAMED`] does not. That
//! makes the split *total*: adding a merchant to the private repo needs no change
//! here and can never silently go untested. Promote it to its own test (add it to
//! `NAMED` + a `merchant_cases!` line) once it's big enough to be worth isolating.
//!
//! The matching/assertion engine is shared with `public_e2e.rs`; see
//! `tests/e2e_harness/mod.rs`.
//!
//! Run it in release — the parser is ~12x faster optimized, which is the
//! difference between a ~20-minute CI job and a ~2-minute one:
//!
//! ```text
//! BEANBEAVER_PRIVATE_TESTS_DIR=../beanbeaver-private-test \
//!   cargo test --release -p receipt-core --test private_e2e
//! ```

mod e2e_harness;

use std::fs;
use std::path::PathBuf;

use e2e_harness::{run_cached_corpus_in, top_level_dirs, Selection};

/// Merchants with a test of their own. Everything else in the corpus runs under
/// `private_cached_e2e_other`; this list is about failure attribution and
/// parallelism, not coverage.
const NAMED: &[&str] = &[
    "bestco_fresh",
    "c_c_supermarket",
    "costco",
    "foody_mart",
    "freshco",
    "jin_lian_food",
    "lcbo",
    "loblaw",
    "no_frills",
    "shoppers",
    "t_t_supermarket",
    "walmart",
];

/// Resolve the corpus root and its `private_rules.toml` overrides.
///
/// `None` ⇒ `BEANBEAVER_PRIVATE_TESTS_DIR` is unset and the caller must SKIP.
/// Absent `private_rules.toml` ⇒ public rules only (the future pure-data state).
fn corpus() -> Option<(PathBuf, Option<String>)> {
    let root = PathBuf::from(std::env::var_os("BEANBEAVER_PRIVATE_TESTS_DIR")?);
    let fixtures = root.join("receipts_e2e");
    assert!(
        fixtures.is_dir(),
        "no receipts_e2e/ under {}",
        root.display()
    );
    let overrides = fs::read_to_string(root.join("private_rules.toml")).ok();
    Some((fixtures, overrides))
}

/// Run one slice of the corpus and assert it is divergence-free.
///
/// `expect_cases` is false only for the catch-all, which is legitimately empty
/// when every merchant on disk has been promoted into [`NAMED`].
fn run_slice(label: &str, sel: &Selection, expect_cases: bool) {
    let Some((fixtures, overrides)) = corpus() else {
        eprintln!("SKIP private_cached_e2e[{label}]: BEANBEAVER_PRIVATE_TESTS_DIR unset");
        return;
    };
    let overrides: Vec<&str> = overrides.as_deref().into_iter().collect();

    let result = run_cached_corpus_in(&fixtures, &overrides, sel);

    eprintln!(
        "private_cached_e2e[{label}]: ran {} cached case(s), {} divergence(s)",
        result.ran,
        result.failures.len()
    );
    for f in &result.failures {
        eprintln!("  ✗ {f}");
    }
    assert!(
        result.failures.is_empty(),
        "{label}: {} private cached check(s) diverged from expected",
        result.failures.len()
    );
    if expect_cases {
        assert!(
            result.ran > 0,
            "{label}: no cached (.ocr.json) fixtures were executed — was the \
             corpus directory renamed? (it would still run under `other`, but \
             this test would stop guarding it)"
        );
    }
}

macro_rules! merchant_cases {
    ($($test:ident => $dir:literal),* $(,)?) => {
        $(
            #[test]
            fn $test() {
                run_slice($dir, &Selection::Dir($dir), true);
            }
        )*
    };
}

merchant_cases! {
    private_cached_e2e_bestco_fresh    => "bestco_fresh",
    private_cached_e2e_c_c_supermarket => "c_c_supermarket",
    private_cached_e2e_costco          => "costco",
    private_cached_e2e_foody_mart      => "foody_mart",
    private_cached_e2e_freshco         => "freshco",
    private_cached_e2e_jin_lian_food   => "jin_lian_food",
    private_cached_e2e_lcbo            => "lcbo",
    private_cached_e2e_loblaw          => "loblaw",
    private_cached_e2e_no_frills       => "no_frills",
    private_cached_e2e_shoppers        => "shoppers",
    private_cached_e2e_t_t_supermarket => "t_t_supermarket",
    private_cached_e2e_walmart         => "walmart",
}

/// Catch-all: every merchant directory not in [`NAMED`], plus any loose
/// root-level cases. Keeps the per-merchant split total.
#[test]
fn private_cached_e2e_other() {
    if let Some((fixtures, _)) = corpus() {
        let swept: Vec<String> = top_level_dirs(&fixtures)
            .into_iter()
            .filter(|d| !NAMED.contains(&d.as_str()))
            .collect();
        eprintln!("private_cached_e2e[other]: sweeping {swept:?}");
    }
    run_slice("other", &Selection::Excluding(NAMED), false);
}
