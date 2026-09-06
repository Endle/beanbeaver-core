//! Self-contained rule loading for the on-device pipeline.
//!
//! Mirrors the desktop runtime loaders (`runtime/item_category_rules.py`,
//! `runtime/merchant_rules.py`) but owns TOML parsing in Rust so the iOS binary
//! needs no Python. The default rule data is bundled from the canonical
//! `rules/*.toml` (single source of truth shared with the desktop build).
//!
//! # Layout
//!
//! - [`corpus`] owns *parsing*: the bundled TOML, its serde mirrors, and the
//!   loaders that turn them into engine structures.
//! - [`book`] owns *access*: [`RuleBook`], the single handle onto the corpus,
//!   used both to classify and to explain.
//!
//! Prefer [`RuleBook`] in new code. The free functions re-exported below are the
//! older entry points, still used by the E2E harnesses and the `ocr-paddle`
//! examples; each re-parses its TOML on every call, so they do not belong on a
//! hot path.

pub mod book;
pub mod corpus;

pub use book::{ItemCategory, ItemExplanation, ItemRule, RuleBook, RuleMatchInfo};
pub use corpus::{
    default_category_accounts, default_known_merchants, default_merchant_families,
    default_merchant_vocab, default_parser_rule_layers, parser_rule_layers_with_overrides,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_layers_load_and_resolve_known_categories() {
        let layers = default_parser_rule_layers();
        // Account mapping must include the ported defaults.
        assert_eq!(
            layers
                .category_rules
                .account_mapping
                .get("grocery/dairy")
                .map(String::as_str),
            Some("Expenses:Food:Grocery:Dairy")
        );
        // Rules parsed from the bundled classifier TOML are non-empty.
        assert!(!layers.category_rules.rules.is_empty());
    }

    #[test]
    fn default_known_merchants_include_bundled_keywords() {
        let merchants = default_known_merchants();
        assert!(merchants.iter().any(|m| m == "COSTCO"));
    }

    #[test]
    fn default_merchant_families_include_pharmasave() {
        let families = default_merchant_families();
        assert!(families.iter().any(|f| f.canonical == "PHARMASAVE"));
    }

    /// The cached book and the uncached loader must describe the same corpus —
    /// otherwise `RuleBook` would be a second source of truth rather than a view
    /// onto the first.
    #[test]
    fn cached_book_matches_the_uncached_loader() {
        let fresh = default_parser_rule_layers();
        let cached = RuleBook::bundled().layers();
        assert_eq!(
            fresh.category_rules.rules.len(),
            cached.category_rules.rules.len()
        );
        assert_eq!(
            fresh.category_rules.exact_only_keywords,
            cached.category_rules.exact_only_keywords
        );
        assert_eq!(
            fresh.category_rules.account_mapping,
            cached.category_rules.account_mapping
        );
        for (a, b) in fresh
            .category_rules
            .rules
            .iter()
            .zip(cached.category_rules.rules.iter())
        {
            assert_eq!(a.keywords, b.keywords);
            assert_eq!(a.category, b.category);
            assert_eq!(a.tags, b.tags);
            assert_eq!(a.priority, b.priority);
            assert_eq!(a.id, b.id);
        }
    }
}
