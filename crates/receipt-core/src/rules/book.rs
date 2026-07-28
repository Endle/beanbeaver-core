//! [`RuleBook`] — the one handle onto the rule corpus.
//!
//! Before this existed, callers reached for four unrelated free functions
//! (`default_parser_rule_layers`, `default_known_merchants`,
//! `default_merchant_families`, `default_merchant_vocab`), each of which re-parsed
//! its TOML from scratch on **every call** — so a single receipt paid for the
//! 730-line classifier parse more than once, and nothing owned the question "what
//! rules are in effect?".
//!
//! `RuleBook` answers that question once. It is both what the parser consumes
//! (via [`RuleBook::layers`]) and what introspection reads (via
//! [`RuleBook::item_rules`] / [`RuleBook::explain`]) — deliberately the same
//! object, so a displayed rule cannot drift from the rule that actually fired.

use std::sync::OnceLock;

use super::corpus;
use crate::merchant_match::MerchantFamily;
use crate::receipt_categories::{
    classify_item_key, classify_item_tags, list_item_categories, resolve_account_target,
    sorted_matches_for_debug,
};
use crate::receipt_parser::ParserRuleLayers;

/// One item-classifier rule, as it exists *after* layering — priorities already
/// boosted, account already resolved. This is a view of the engine's own
/// `CategoryRule`, not a re-read of the TOML, so it cannot disagree with it.
#[derive(Clone, Debug)]
pub struct ItemRule {
    /// Provenance label from the source TOML, when it had one.
    pub id: Option<String>,
    /// Position in [`ParserRuleLayers::category_rules`] — the same index a
    /// [`RuleMatchInfo::rule_index`] refers to.
    pub index: usize,
    pub keywords: Vec<String>,
    pub tags: Vec<String>,
    /// Category key (`grocery_dairy`), or `None` for a tag-only rule.
    pub category_key: Option<String>,
    /// The beancount account `category_key` resolves to. `None` for a tag-only
    /// rule, or a key with no mapping.
    pub account: Option<String>,
    /// Effective priority, including the layer boost.
    pub priority: i32,
    pub exact_only: bool,
    /// 0 = bundled defaults; 1+ = override layers, in the order supplied.
    pub layer: usize,
}

impl ItemRule {
    /// A rule that contributes tags but no account (the `semantic_tag_*` family).
    pub fn is_tag_only(&self) -> bool {
        self.category_key.is_none()
    }
}

/// A category key paired with the beancount account it resolves to.
#[derive(Clone, Debug)]
pub struct ItemCategory {
    pub key: String,
    pub account: String,
}

/// One rule that fired for a description, and how strongly.
#[derive(Clone, Debug)]
pub struct RuleMatchInfo {
    pub rule_id: Option<String>,
    pub rule_index: usize,
    /// The keyword that actually hit — the specific reason this rule matched.
    pub matched_keyword: String,
    /// False when the hit came from the fuzzy/bigram stage rather than a literal
    /// or OCR-confusable substring match.
    pub is_exact: bool,
    pub priority: i32,
    pub keyword_length: usize,
    pub tags: Vec<String>,
    pub category_key: Option<String>,
    /// True for the single match whose category won the ranking contest.
    pub is_category_winner: bool,
}

/// Why a description classified the way it did.
#[derive(Clone, Debug)]
pub struct ItemExplanation {
    pub description: String,
    pub category_key: Option<String>,
    pub account: Option<String>,
    /// The union of every matching rule's tags, in rule order — the same list the
    /// parser puts on the item.
    pub tags: Vec<String>,
    /// Every rule that fired, strongest first.
    pub matches: Vec<RuleMatchInfo>,
}

/// The rule corpus in effect for one parse: bundled defaults, plus any override
/// layers supplied on top.
#[derive(Clone, Debug)]
pub struct RuleBook {
    layers: ParserRuleLayers,
    known_merchants: Vec<String>,
    merchant_families: Vec<MerchantFamily>,
}

static BUNDLED: OnceLock<RuleBook> = OnceLock::new();

impl RuleBook {
    /// The bundled defaults, parsed once per process and shared thereafter.
    ///
    /// Prefer this over [`corpus::default_parser_rule_layers`] on any hot path:
    /// that function re-parses all four TOML documents on every call.
    pub fn bundled() -> &'static RuleBook {
        BUNDLED.get_or_init(|| RuleBook {
            layers: corpus::default_parser_rule_layers(),
            known_merchants: corpus::default_known_merchants(),
            merchant_families: corpus::default_merchant_families(),
        })
    }

    /// Bundled defaults plus zero or more override classifier documents, later
    /// layers winning. Merchant rules stay at the bundled defaults, matching
    /// [`corpus::parser_rule_layers_with_overrides`].
    ///
    /// Returns `Err` on malformed override TOML — never panics on user input, so
    /// the FFI path can surface it as a typed error.
    pub fn with_overrides(override_classifier_tomls: &[&str]) -> Result<RuleBook, String> {
        if override_classifier_tomls.is_empty() {
            return Ok(Self::bundled().clone());
        }
        Ok(RuleBook {
            layers: corpus::parser_rule_layers_with_overrides(override_classifier_tomls)?,
            known_merchants: corpus::default_known_merchants(),
            merchant_families: corpus::default_merchant_families(),
        })
    }

    pub fn layers(&self) -> &ParserRuleLayers {
        &self.layers
    }

    pub fn known_merchants(&self) -> &[String] {
        &self.known_merchants
    }

    pub fn merchant_families(&self) -> &[MerchantFamily] {
        &self.merchant_families
    }

    /// Every item-classifier rule in effect, in layer order.
    pub fn item_rules(&self) -> Vec<ItemRule> {
        let mapping = &self.layers.category_rules.account_mapping;
        self.layers
            .category_rules
            .rules
            .iter()
            .enumerate()
            .map(|(index, rule)| ItemRule {
                id: rule.id.clone(),
                index,
                keywords: rule.keywords.clone(),
                tags: rule.tags.clone(),
                category_key: rule.category.clone(),
                account: resolve_account_target(rule.category.as_deref(), mapping, None),
                priority: rule.priority,
                exact_only: rule.exact_only,
                layer: rule.layer,
            })
            .collect()
    }

    /// Every category key paired with its account, sorted by key.
    pub fn categories(&self) -> Vec<ItemCategory> {
        list_item_categories(&self.layers.category_rules)
            .into_iter()
            .map(|(key, account)| ItemCategory { key, account })
            .collect()
    }

    /// The tag vocabulary, derived by unioning every rule's tags and sorting.
    ///
    /// Derived, because the rule format has no vocabulary declaration: a tag
    /// exists only because some rule mentions it, so a typo silently invents one.
    pub fn tags(&self) -> Vec<String> {
        let mut tags: Vec<String> = self
            .layers
            .category_rules
            .rules
            .iter()
            .flat_map(|rule| rule.tags.iter().cloned())
            .collect();
        tags.sort();
        tags.dedup();
        tags
    }

    /// Why `description` classifies the way it does: the resolved account and
    /// tags, plus every rule that fired, strongest first.
    ///
    /// The account and tags are computed with the same `classify_*` calls the
    /// parser makes, so this can never report something the parser would not do.
    pub fn explain(&self, description: &str) -> ItemExplanation {
        let layers = &self.layers.category_rules;
        let category_key = classify_item_key(description, layers, None);
        let tags = classify_item_tags(description, layers);
        let account =
            resolve_account_target(category_key.as_deref(), &layers.account_mapping, None);

        // `sorted_matches_for_debug` sorts by the same ranking the classifier
        // uses, strongest first. `rule_index` is unique per match (one match per
        // rule) and is the final tiebreak, so the ranking is total: the first
        // match carrying a category is exactly the one `classify_item_key` chose.
        let mut winner_seen = false;
        let matches = sorted_matches_for_debug(description, layers)
            .into_iter()
            .map(|matched| {
                let is_category_winner = if !winner_seen && matched.category.is_some() {
                    winner_seen = true;
                    true
                } else {
                    false
                };
                RuleMatchInfo {
                    rule_id: layers
                        .rules
                        .get(matched.rule_index)
                        .and_then(|rule| rule.id.clone()),
                    rule_index: matched.rule_index,
                    matched_keyword: matched.matched_keyword,
                    is_exact: matched.is_exact,
                    priority: matched.priority,
                    keyword_length: matched.keyword_length,
                    tags: matched.tags,
                    category_key: matched.category,
                    is_category_winner,
                }
            })
            .collect();

        ItemExplanation {
            description: description.to_string(),
            category_key,
            account,
            tags,
            matches,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RuleBook;

    #[test]
    fn bundled_book_exposes_rules_categories_and_tags() {
        let book = RuleBook::bundled();
        assert!(!book.item_rules().is_empty());
        assert!(book
            .categories()
            .iter()
            .any(|c| c.key == "grocery_dairy" && c.account == "Expenses:Food:Grocery:Dairy"));
        assert!(book.tags().iter().any(|t| t == "dairy"));
        assert!(book.known_merchants().iter().any(|m| m == "COSTCO"));
        assert!(book
            .merchant_families()
            .iter()
            .any(|f| f.canonical == "PHARMASAVE"));
    }

    /// Rule ids used to be dropped by serde, so a match could not be traced back
    /// to the rule that caused it.
    #[test]
    fn bundled_rules_carry_their_provenance_ids() {
        let rules = RuleBook::bundled().item_rules();
        assert!(rules.iter().any(|r| r.id.as_deref() == Some("legacy_0000")));
        assert!(rules
            .iter()
            .any(|r| r.id.as_deref() == Some("semantic_tag_0101") && r.is_tag_only()));
        assert!(rules.iter().all(|r| r.layer == 0));
    }

    /// `explain` must agree with the classifier it is explaining.
    #[test]
    fn explain_agrees_with_the_parser_and_names_the_winner() {
        let book = RuleBook::bundled();
        let layers = &book.layers().category_rules;
        for description in [
            "KS ORG 2% MILK",
            "ROTISSERIE CHICKEN",
            "ORGANIC SPINACH",
            "ZZZ NOTHING MATCHES THIS",
        ] {
            let explained = book.explain(description);
            assert_eq!(
                explained.category_key,
                crate::receipt_categories::classify_item_key(description, layers, None),
                "category disagreed for {description:?}"
            );
            assert_eq!(
                explained.tags,
                crate::receipt_categories::classify_item_tags(description, layers),
                "tags disagreed for {description:?}"
            );
            let winners: Vec<_> = explained
                .matches
                .iter()
                .filter(|m| m.is_category_winner)
                .collect();
            assert!(
                winners.len() <= 1,
                "more than one winner for {description:?}"
            );
            if let Some(winner) = winners.first() {
                assert_eq!(winner.category_key, explained.category_key);
            }
        }
    }

    #[test]
    fn overrides_stack_on_top_of_the_bundled_layer() {
        let book = RuleBook::with_overrides(&[r#"
[[rules]]
id = "user_0001"
keywords = ["ZZZ TEST WIDGET"]
category = "grocery_dairy"
tags = ["grocery", "dairy"]
priority = 5
"#])
        .expect("override parses");
        let rule = book
            .item_rules()
            .into_iter()
            .find(|r| r.id.as_deref() == Some("user_0001"))
            .expect("override rule present");
        assert_eq!(rule.layer, 1);
        // Layer 1 boosts by 200, on top of the declared 5.
        assert_eq!(rule.priority, 205);
        assert_eq!(rule.account.as_deref(), Some("Expenses:Food:Grocery:Dairy"));
        assert_eq!(
            book.explain("ZZZ TEST WIDGET").account.as_deref(),
            Some("Expenses:Food:Grocery:Dairy")
        );
    }

    #[test]
    fn malformed_override_is_an_error_not_a_panic() {
        assert!(RuleBook::with_overrides(&["this is not valid toml ="]).is_err());
    }
}
