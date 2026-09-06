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
use crate::categories::{list_item_categories, resolve_account_target, TagNode};
use crate::merchant_match::MerchantFamily;
use crate::parser::ParserRuleLayers;

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
    /// Tag paths this rule subtracts when it matches.
    pub remove_tags: Vec<String>,
    /// Rule ids this rule voids when it matches.
    pub disables: Vec<String>,
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
    /// Every rule that fired **and survived subtraction**, strongest first. A
    /// rule voided by another rule's `disables` does not appear; the surviving
    /// rule's own `disables` list says why.
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
                remove_tags: rule.remove_tags.clone(),
                disables: rule.disables.clone(),
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

    /// The declared tag vocabulary, in file order.
    ///
    /// Previously this had to be *derived* by unioning whatever tags the rules
    /// happened to mention, which meant a typo silently invented a tag. It is now
    /// declared data, and a rule naming an undeclared path is rejected at load.
    pub fn tag_vocabulary(&self) -> &[TagNode] {
        &self.layers.category_rules.tag_vocabulary
    }

    /// The authored display name for a tag path, falling back to the leaf
    /// segment for a path the vocabulary does not declare.
    pub fn tag_display(&self, path: &str) -> String {
        self.tag_vocabulary()
            .iter()
            .find(|node| node.path == path)
            .map(|node| node.display.clone())
            .unwrap_or_else(|| TagNode::segments(path).pop().unwrap_or_default())
    }

    /// Why `description` classifies the way it does: the resolved account and
    /// tags, plus every rule that fired, strongest first.
    ///
    /// The classification and explanation share one resolved match set, using
    /// the same ranking and tag accumulation as the parser.
    pub fn explain(&self, description: &str) -> ItemExplanation {
        let layers = &self.layers.category_rules;
        let (classification, resolved_matches) =
            crate::categories::explain_classification(description, layers);
        let category_key = classification.tag_path;
        let tags = classification.tags;
        let account = classification.account;

        // Resolved matches are sorted by the same ranking the classifier
        // uses, strongest first. `rule_index` is unique per match (one match per
        // rule) and is the final tiebreak, so the ranking is total: the first
        // match carrying a category is exactly the one `classify_item_key` chose.
        let mut winner_seen = false;
        let matches = resolved_matches
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
            .any(|c| c.key == "grocery/dairy" && c.account == "Expenses:Food:Grocery:Dairy"));
        assert!(book
            .tag_vocabulary()
            .iter()
            .any(|t| t.path == "grocery/dairy" && t.display == "Dairy"));
        // The display name is authored, not derived: capitalizing the segment is
        // what put an underscore on screen for this one.
        assert_eq!(
            book.tag_display("grocery/drink/energy_drink"),
            "Energy Drink"
        );
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
                crate::categories::classify_item_key(description, layers, None),
                "category disagreed for {description:?}"
            );
            assert_eq!(
                explained.tags,
                crate::categories::classify_item_tags(description, layers),
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
tags = ["grocery/dairy"]
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

    /// `remove_tags` subtracts at PATH level, so a shared ancestor survives when
    /// another path still implies it. This is the case a flat tag-name
    /// subtraction could not express.
    #[test]
    fn remove_tags_subtracts_a_path_without_orphaning_its_ancestor() {
        let book = RuleBook::with_overrides(&[r#"
[[rules]]
id = "user_no_snacks"
keywords = ["CHOCOLATE MILK"]
tags = ["grocery/dairy"]
remove_tags = ["grocery/snacks"]
priority = 50
"#])
        .expect("override loads");
        let explained = book.explain("CHOCOLATE MILK");
        assert!(
            !explained.tags.iter().any(|t| t == "grocery/snacks"),
            "snacks should be subtracted, got {:?}",
            explained.tags
        );
        // The shared ancestor is still justified by grocery/dairy.
        assert!(explained.tags.iter().any(|t| t == "grocery"));
        assert!(explained.tags.iter().any(|t| t == "grocery/dairy"));
        // Baseline: without the override, snacks is present.
        assert!(RuleBook::bundled()
            .explain("CHOCOLATE MILK")
            .tags
            .iter()
            .any(|t| t == "grocery/snacks"));
    }

    /// Subtracting a rule's account-claiming path also drops its claim — filing
    /// an item under an account while refusing to tag it that way is incoherent.
    #[test]
    fn removing_the_claiming_path_also_drops_the_account() {
        let book = RuleBook::with_overrides(&[r#"
[[rules]]
id = "user_untag_dairy"
keywords = ["ZZZ PLAIN YOGURT"]
tags = ["grocery/staple"]
remove_tags = ["grocery/dairy"]
priority = 50
"#])
        .expect("override loads");
        let explained = book.explain("ZZZ PLAIN YOGURT");
        assert_eq!(
            explained.account.as_deref(),
            Some("Expenses:Food:Grocery:Staple")
        );
        assert!(!explained.tags.iter().any(|t| t == "grocery/dairy"));
    }

    /// `disables` voids a rule's match by id.
    #[test]
    fn disables_voids_a_matching_rule_by_id() {
        let before = RuleBook::bundled().explain("347937 CHICKEN");
        assert!(before
            .matches
            .iter()
            .any(|m| m.rule_id.as_deref() == Some("semantic_tag_0100")));

        let book = RuleBook::with_overrides(&[r#"
[[rules]]
id = "user_no_chicken_tag"
keywords = ["CHICKEN"]
tags = ["grocery/meat"]
disables = ["semantic_tag_0100"]
priority = 50
"#])
        .expect("override loads");
        let after = book.explain("347937 CHICKEN");
        assert!(
            !after
                .matches
                .iter()
                .any(|m| m.rule_id.as_deref() == Some("semantic_tag_0100")),
            "disabled rule should not appear among matches"
        );
        assert!(!after.tags.iter().any(|t| t == "grocery/meat/chicken"));
        // The meat rule is untouched, so the account still resolves.
        assert_eq!(after.account.as_deref(), Some("Expenses:Food:Grocery:Meat"));
    }

    /// Every matching rule's `disables` apply at once, including a rule that is
    /// itself disabled. That keeps the disabled set a pure function of which
    /// rules matched, rather than something that depends on evaluation order.
    #[test]
    fn disables_apply_simultaneously_in_one_pass() {
        let book = RuleBook::with_overrides(&[r#"
[[rules]]
id = "chain_a"
keywords = ["ZZZ CHAIN ITEM"]
tags = ["grocery/staple"]
disables = ["chain_b"]
priority = 50

[[rules]]
id = "chain_b"
keywords = ["ZZZ CHAIN ITEM"]
tags = ["grocery/snacks"]
disables = ["chain_c"]
priority = 40

[[rules]]
id = "chain_c"
keywords = ["ZZZ CHAIN ITEM"]
tags = ["grocery/bakery"]
priority = 30
"#])
        .expect("override loads");
        let explained = book.explain("ZZZ CHAIN ITEM");
        let ids: Vec<&str> = explained
            .matches
            .iter()
            .filter_map(|m| m.rule_id.as_deref())
            .collect();
        assert!(ids.contains(&"chain_a"));
        assert!(!ids.contains(&"chain_b"), "b is disabled by a");
        // c is disabled by b even though b is itself disabled: all matching
        // rules' `disables` are collected before any are applied.
        assert!(
            !ids.contains(&"chain_c"),
            "expected one-pass semantics, got {ids:?}"
        );
    }

    /// A typo in `disables` silently did nothing before it was validated.
    #[test]
    fn unknown_disable_id_is_rejected() {
        let err = RuleBook::with_overrides(&[r#"
[[rules]]
id = "user_typo"
keywords = ["ZZZ"]
tags = ["grocery/staple"]
disables = ["legacy_9999"]
"#])
        .expect_err("unknown id must not load");
        assert!(
            err.contains("legacy_9999"),
            "error should name the id: {err}"
        );
    }

    /// `QUAIL` must match literally. I/L is a confusable pair, so before this was
    /// `exact_only` the OCR-noise stage aligned QUAIL against QUALI inside
    /// "QUALITY" and hung `grocery/dairy` on two corpus lines. The account was
    /// never wrong — priority kept it on the seafood rule — so only the tag list
    /// showed it, and the app leads with the LAST tag, which is why a squid
    /// displayed as "Dairy". Nothing the rule exists for is lost: the OCR damage
    /// it survives is in EGGS, never in QUAIL.
    #[test]
    fn quail_matches_literally_and_does_not_claim_quality() {
        let book = RuleBook::bundled();
        for description in ["Beat Quality - Squid Tent", "*Best Quality Frozen Boi"] {
            let explained = book.explain(description);
            assert!(
                !explained.tags.iter().any(|t| t == "grocery/dairy"),
                "{description:?} must not be dairy, got {:?}",
                explained.tags
            );
        }
        assert_eq!(
            book.explain("Beat Quality - Squid Tent").account.as_deref(),
            Some("Expenses:Food:Grocery:Seafood:Shrimp")
        );
        // The cases the rule exists for still work, including the OCR damage in
        // EGGS that motivated matching on QUAIL in the first place.
        for description in ["LA - Quail Eggs", "#Foojoy Quail Eggs", "LA - Quail E8g8"] {
            assert_eq!(
                book.explain(description).account.as_deref(),
                Some("Expenses:Food:Grocery:Dairy"),
                "{description:?} should still be dairy"
            );
        }
    }

    /// `frozen_department_item` is guarded by TWO independent mechanisms, and a
    /// regression in either is silent, so both are asserted here.
    #[test]
    fn frozen_department_item_is_last_resort_and_never_the_label() {
        let book = RuleBook::bundled();

        // It classifies the bare department-name line, which is its whole point.
        assert_eq!(
            book.explain("Frozen").account.as_deref(),
            Some("Expenses:Food:Grocery:Frozen")
        );

        // PRIORITY: it must lose the account to every other rule, including the
        // deepest last-resort in the corpus. At -80 it outranked legacy_0046
        // ("PEELED" -> shrimp, -150) and moved this line onto Frozen.
        let rules = book.item_rules();
        let frozen = rules
            .iter()
            .find(|r| r.id.as_deref() == Some("frozen_department_item"))
            .expect("rule present");
        assert!(
            rules
                .iter()
                .filter(|r| r.id.as_deref() != Some("frozen_department_item"))
                .all(|r| r.priority > frozen.priority),
            "frozen_department_item must be the lowest-priority bundled rule"
        );
        assert_eq!(
            book.explain("BQ - Frozen Raw Peeled Un").account.as_deref(),
            Some("Expenses:Food:Grocery:Seafood:Shrimp")
        );

        // POSITION: declared first in the file, so `grocery/frozen` is never the
        // last tag when another rule also matched — the app labels an item with
        // its last tag, so a later declaration would relabel every frozen shrimp.
        for description in [
            "FY - Raw Frozen Vannaamei",
            "*Frozen Raw Vannamei White",
            "Shirakiku - Frozen Imitat",
            "Fu Yang Frozen Shrimp",
        ] {
            let tags = book.explain(description).tags;
            assert!(
                tags.iter().any(|t| t == "grocery/frozen"),
                "{description:?} should carry the frozen tag"
            );
            assert_ne!(
                tags.last().map(String::as_str),
                Some("grocery/frozen"),
                "{description:?} would be labelled Frozen, got {tags:?}"
            );
        }
    }

    /// Dim sum arrives as a department-name item too. It files as prepared food
    /// on the receipt's own evidence — the line is taxed at 5%, Ontario's
    /// prepared-food-under-$4 rate, where a frozen box would be zero-rated.
    #[test]
    fn dim_sum_is_a_prepared_meal() {
        assert_eq!(
            RuleBook::bundled().explain("Dim Sum").account.as_deref(),
            Some("Expenses:Food:Grocery:PreparedMeal")
        );
    }

    /// The bundled corpus uses neither operator, so nothing it classifies changes.
    #[test]
    fn bundled_corpus_uses_no_subtraction() {
        let rules = RuleBook::bundled().item_rules();
        assert!(rules
            .iter()
            .all(|r| r.remove_tags.is_empty() && r.disables.is_empty()));
    }
}
