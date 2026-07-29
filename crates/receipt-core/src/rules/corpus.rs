//! The bundled rule corpus: TOML text, its serde mirrors, and the loaders that
//! turn it into engine structures.
//!
//! This module owns *parsing*. It deliberately knows nothing about how the rules
//! are queried or displayed — that is [`super::book`]'s job. Everything here is
//! reachable through [`super::RuleBook`]; the free functions are kept public
//! because the E2E harnesses and the `ocr-paddle` examples call them directly.

use std::collections::HashMap;

use serde::Deserialize;

use crate::merchant_match::MerchantFamily;
use crate::merchant_vocab::{Expansion, MerchantVocab};
use crate::receipt_categories::{
    build_rule_layers, BuildClassifierConfig, BuildRuleEntry, TagNode,
};
use crate::receipt_parser::ParserRuleLayers;

pub(crate) const DEFAULT_ITEM_CLASSIFIER_TOML: &str =
    include_str!("../../../../rules/default_item_classifier.toml");
pub(crate) const DEFAULT_TAGS_TOML: &str = include_str!("../../../../rules/default_tags.toml");
pub(crate) const DEFAULT_MERCHANT_RULES_TOML: &str =
    include_str!("../../../../rules/default_merchant_rules.toml");
pub(crate) const DEFAULT_MERCHANT_FAMILIES_TOML: &str =
    include_str!("../../../../rules/default_merchant_families.toml");
pub(crate) const DEFAULT_MERCHANT_VOCAB_TOML: &str =
    include_str!("../../../../rules/default_merchant_vocab.toml");

#[derive(Debug, Deserialize)]
struct TagNodeToml {
    path: String,
    #[serde(default)]
    display: Option<String>,
}

/// One rule document. Every table is optional, so the same schema serves the
/// bundled vocabulary file, the bundled classifier file, and a user's override —
/// which may carry any mix of the three.
#[derive(Debug, Default, Deserialize)]
struct DocumentToml {
    /// Vocabulary: what an item *is*.
    #[serde(default)]
    tags: Vec<TagNodeToml>,
    /// Policy: tag path -> beancount account. The overridable half.
    #[serde(default)]
    accounts: HashMap<String, String>,
    #[serde(default)]
    exact_only_keywords: StringOrList,
    #[serde(default)]
    rules: Vec<RuleToml>,
}

/// The bundled tag vocabulary plus its default tag-path -> account mapping.
///
/// The two halves are deliberately separate concerns: the vocabulary is parse
/// output (what an item *is*) and belongs to core's charter, while the account
/// map is ledger policy (where the user *files* it) and is meant to be overridden.
pub fn default_tag_vocabulary() -> (Vec<TagNode>, HashMap<String, String>) {
    let parsed = parse_document(DEFAULT_TAGS_TOML, "bundled default_tags.toml")
        .expect("bundled default_tags.toml is valid");
    (to_tag_nodes(&parsed), parsed.accounts)
}

/// Two-stage tag-path -> beancount-account mapping.
pub fn default_category_accounts() -> HashMap<String, String> {
    default_tag_vocabulary().1
}

/// The pre-vocabulary account table, kept only as the source the migration was
/// generated from. Unused at runtime.
#[cfg(test)]
fn legacy_category_accounts() -> HashMap<String, String> {
    [
        ("grocery_dairy", "Expenses:Food:Grocery:Dairy"),
        ("grocery_meat", "Expenses:Food:Grocery:Meat"),
        ("grocery_seafood_fish", "Expenses:Food:Grocery:Seafood:Fish"),
        (
            "grocery_seafood_shrimp",
            "Expenses:Food:Grocery:Seafood:Shrimp",
        ),
        ("grocery_seafood", "Expenses:Food:Grocery:Seafood"),
        ("grocery_fruit", "Expenses:Food:Grocery:Fruit"),
        ("grocery_vegetable", "Expenses:Food:Grocery:Vegetable"),
        (
            "grocery_vegetable_canned",
            "Expenses:Food:Grocery:Vegetable:Canned",
        ),
        (
            "grocery_frozen_dumpling",
            "Expenses:Food:Grocery:Frozen:Dumpling",
        ),
        (
            "grocery_frozen_icecream",
            "Expenses:Food:Grocery:Frozen:IceCream",
        ),
        ("grocery_frozen", "Expenses:Food:Grocery:Frozen"),
        (
            "grocery_prepared_meal",
            "Expenses:Food:Grocery:PreparedMeal",
        ),
        ("grocery_bakery", "Expenses:Food:Grocery:Bakery"),
        ("grocery_staple", "Expenses:Food:Grocery:Staple"),
        ("grocery_seasoning", "Expenses:Food:Grocery:Seasoning"),
        ("grocery_snacks", "Expenses:Food:Grocery:Snacks"),
        ("grocery_snacks_mint", "Expenses:Food:Grocery:Snacks:Mint"),
        (
            "grocery_drink_cocacola",
            "Expenses:Food:Grocery:Drink:CocaCola",
        ),
        ("grocery_drink_juice", "Expenses:Food:Grocery:Drink:Juice"),
        ("grocery_drink_coffee", "Expenses:Food:Grocery:Drink:Coffee"),
        ("grocery_drink", "Expenses:Food:Grocery:Drink"),
        ("alcoholic_beverage", "Expenses:Food:AlcoholicBeverage"),
        ("home_household_supply", "Expenses:Home:HouseholdSupply"),
        ("personal_care", "Expenses:PersonalCare"),
        ("personal_care_tooth", "Expenses:PersonalCare:Tooth"),
        ("pet", "Expenses:Pet"),
        ("pet_supply", "Expenses:Pet:Supply"),
        ("restaurant_gift_card", "Expenses:Food:Restaurant:GiftCard"),
        ("health_pharmacy", "Expenses:Health:Pharmacy"),
        ("shopping_clothing", "Expenses:Shopping:Clothing"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// Accepts a TOML value that is either a single string or a list of strings,
/// mirroring `python_receipt_categories.rs::string_or_list` (trim, drop empties).
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum StringOrList {
    One(String),
    Many(Vec<String>),
}

impl StringOrList {
    fn into_trimmed(self) -> Vec<String> {
        match self {
            StringOrList::One(text) => {
                let cleaned = text.trim();
                if cleaned.is_empty() {
                    Vec::new()
                } else {
                    vec![cleaned.to_string()]
                }
            }
            StringOrList::Many(values) => values
                .into_iter()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .collect(),
        }
    }
}

impl Default for StringOrList {
    fn default() -> Self {
        StringOrList::Many(Vec::new())
    }
}

/// Lowercase + dedupe tags, preserving first-seen order. Mirrors
/// `python_receipt_categories.rs::normalize_tags`.
fn normalize_tags(values: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut normalized = Vec::new();
    for value in values {
        let cleaned = value.trim().to_ascii_lowercase();
        if cleaned.is_empty() || !seen.insert(cleaned.clone()) {
            continue;
        }
        normalized.push(cleaned);
    }
    normalized
}

#[derive(Debug, Deserialize)]
struct RuleToml {
    /// Human/provenance label. Every bundled rule block writes one; it used to be
    /// dropped on the floor here, which is why nothing could trace a match back to
    /// its rule. Read now, but still not consulted by the classifier.
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    keywords: StringOrList,
    /// Tag paths (`grocery/dairy`). Replaces the old `category`/`key` +
    /// flat-`tags` pair; the account a rule claims now comes from looking its
    /// path up in `[accounts]`.
    #[serde(default)]
    tags: StringOrList,
    /// Tag paths to subtract when this rule matches. The override format was
    /// purely additive before this, which is what made "stop tagging MILK as
    /// dairy" inexpressible.
    #[serde(default)]
    remove_tags: StringOrList,
    /// Rule ids whose match this rule voids.
    #[serde(default)]
    disables: StringOrList,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    exact_only: bool,
}

fn to_build_config(parsed: &DocumentToml) -> BuildClassifierConfig {
    BuildClassifierConfig {
        exact_only_keywords: parsed.exact_only_keywords.clone().into_trimmed(),
        rules: parsed
            .rules
            .iter()
            .map(|rule| BuildRuleEntry {
                id: rule
                    .id
                    .as_ref()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty()),
                keywords: rule.keywords.clone().into_trimmed(),
                tag_paths: normalize_tags(rule.tags.clone().into_trimmed()),
                priority: rule.priority,
                exact_only: rule.exact_only,
                remove_tags: normalize_tags(rule.remove_tags.clone().into_trimmed()),
                disables: rule.disables.clone().into_trimmed(),
            })
            .collect(),
    }
}

/// Reject any rule naming a tag path the vocabulary does not declare.
///
/// This is the check that makes a typo loud. Before the vocabulary existed a
/// misspelled tag silently invented a new one, because a tag was whatever some
/// rule happened to write.
fn validate_tag_paths(
    configs: &[BuildClassifierConfig],
    vocabulary: &[TagNode],
) -> Result<(), String> {
    let known: std::collections::HashSet<&str> =
        vocabulary.iter().map(|node| node.path.as_str()).collect();
    let mut unknown: Vec<String> = Vec::new();
    for config in configs {
        for rule in &config.rules {
            for path in rule.tag_paths.iter().chain(rule.remove_tags.iter()) {
                if !known.contains(path.as_str()) && !unknown.contains(path) {
                    unknown.push(path.clone());
                }
            }
        }
    }
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort();
    Err(format!(
        "unknown tag path(s) not declared in the vocabulary: {}",
        unknown.join(", ")
    ))
}

/// Reject a `disables` naming a rule id that does not exist.
///
/// Same reasoning as the tag-path check: a typo that silently does nothing is
/// the failure mode this format is trying to leave behind. It does make bundled
/// rule ids a compatibility surface — they are frozen and additive, which the
/// bundled corpus states in its header.
fn validate_disable_ids(configs: &[BuildClassifierConfig]) -> Result<(), String> {
    let known: std::collections::HashSet<&str> = configs
        .iter()
        .flat_map(|c| c.rules.iter())
        .filter_map(|r| r.id.as_deref())
        .collect();
    let mut unknown: Vec<String> = Vec::new();
    for config in configs {
        for rule in &config.rules {
            for id in &rule.disables {
                if !known.contains(id.as_str()) && !unknown.contains(id) {
                    unknown.push(id.clone());
                }
            }
        }
    }
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort();
    Err(format!(
        "`disables` names unknown rule id(s): {}",
        unknown.join(", ")
    ))
}

fn parse_document(toml_text: &str, what: &str) -> Result<DocumentToml, String> {
    toml::from_str(toml_text).map_err(|e| format!("invalid {what}: {e}"))
}

/// Collapse a document's `[[tags]]` into vocabulary nodes, filling in a
/// last-resort `display` for any node that omitted one.
fn to_tag_nodes(parsed: &DocumentToml) -> Vec<TagNode> {
    parsed
        .tags
        .iter()
        .map(|node| {
            let path = node.path.trim().to_string();
            let display = node
                .display
                .as_ref()
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| TagNode::segments(&path).pop().unwrap_or_default());
            TagNode { path, display }
        })
        .filter(|node| !node.path.is_empty())
        .collect()
}

/// Build the default rule layers from the bundled documents — no overrides.
pub fn default_parser_rule_layers() -> ParserRuleLayers {
    parser_rule_layers_with_overrides(&[]).expect("bundled rule documents are valid")
}

/// Build rule layers from the bundled documents plus zero or more override
/// documents, later layers winning.
///
/// An override may carry any mix of the three tables. `[[tags]]` and
/// `[accounts]` **merge** over the defaults — a later document replaces matching
/// entries and leaves the rest alone — while `[[rules]]` append as a new layer
/// with the usual priority boost. That is what lets a user add a rule without
/// restating the vocabulary, or re-point one account without touching any rule.
///
/// Returns `Err` when an override fails to parse **or** names a tag path the
/// vocabulary does not declare. Never panics on user input, so the FFI path can
/// surface either as a typed error.
pub fn parser_rule_layers_with_overrides(
    override_documents: &[&str],
) -> Result<ParserRuleLayers, String> {
    let tags_doc = parse_document(DEFAULT_TAGS_TOML, "bundled default_tags.toml")?;
    let classifier_doc = parse_document(
        DEFAULT_ITEM_CLASSIFIER_TOML,
        "bundled default_item_classifier.toml",
    )?;

    let mut vocabulary = to_tag_nodes(&tags_doc);
    let mut accounts = tags_doc.accounts.clone();
    let mut configs = vec![to_build_config(&classifier_doc)];

    for (i, text) in override_documents.iter().enumerate() {
        let doc = parse_document(text, &format!("override rule document (layer {i})"))?;
        for node in to_tag_nodes(&doc) {
            match vocabulary.iter_mut().find(|known| known.path == node.path) {
                Some(existing) => *existing = node,
                None => vocabulary.push(node),
            }
        }
        for (path, account) in &doc.accounts {
            let (path, account) = (path.trim(), account.trim());
            if !path.is_empty() && !account.is_empty() {
                accounts.insert(path.to_string(), account.to_string());
            }
        }
        configs.push(to_build_config(&doc));
    }

    validate_tag_paths(&configs, &vocabulary)?;
    validate_disable_ids(&configs)?;
    Ok(finish_layers(build_rule_layers(
        accounts,
        configs,
        vec![],
        vocabulary,
    )))
}

/// Wrap built category rules with the flattened account mapping and the bundled
/// merchant vocabulary. Shared by both loaders above so they cannot drift.
fn finish_layers(
    category_rules: crate::receipt_categories::CategoryRuleLayers,
) -> ParserRuleLayers {
    let account_mapping = category_rules
        .account_mapping
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    ParserRuleLayers {
        category_rules,
        account_mapping,
        merchant_vocab: default_merchant_vocab(),
    }
}

#[derive(Debug, Deserialize)]
struct MerchantRuleToml {
    #[serde(default)]
    keywords: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct MerchantRulesToml {
    #[serde(default)]
    rules: Vec<MerchantRuleToml>,
}

#[derive(Debug, Deserialize)]
struct MerchantFamilyToml {
    canonical: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    corroborators: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct MerchantFamiliesToml {
    #[serde(default)]
    families: Vec<MerchantFamilyToml>,
}

/// Load the bundled canonical/alias/corroborator merchant families that drive
/// the fuzzy merchant matcher (`crate::merchant_match`).
pub fn default_merchant_families() -> Vec<MerchantFamily> {
    let parsed: MerchantFamiliesToml = toml::from_str(DEFAULT_MERCHANT_FAMILIES_TOML)
        .expect("bundled default_merchant_families.toml is valid");
    parsed
        .families
        .into_iter()
        .map(|family| MerchantFamily {
            canonical: family.canonical,
            aliases: family.aliases,
            corroborators: family.corroborators,
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct MerchantVocabExpansionToml {
    short: String,
    full: String,
    /// Whether this expansion may feed the classifier. Defaults to `true`;
    /// brand and flavour entries set it `false` (see `Expansion::classify`).
    #[serde(default = "default_true")]
    classify: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct MerchantVocabEntryToml {
    canonical: String,
    #[serde(default)]
    expansions: Vec<MerchantVocabExpansionToml>,
}

#[derive(Debug, Deserialize)]
struct MerchantVocabToml {
    #[serde(default)]
    merchants: Vec<MerchantVocabEntryToml>,
}

/// Load the bundled merchant-scoped abbreviation vocabulary that
/// [`crate::merchant_vocab`] applies before classification.
///
/// Abbreviations are keyed uppercase; the expansion keeps the casing written in
/// the TOML, since it is what surfaces in the recovered item name.
pub fn default_merchant_vocab() -> Vec<MerchantVocab> {
    let parsed: MerchantVocabToml = toml::from_str(DEFAULT_MERCHANT_VOCAB_TOML)
        .expect("bundled default_merchant_vocab.toml is valid");
    parsed
        .merchants
        .into_iter()
        .map(|entry| MerchantVocab {
            canonical: entry.canonical,
            expansions: entry
                .expansions
                .into_iter()
                .map(|e| {
                    (
                        e.short.trim().to_ascii_uppercase(),
                        Expansion {
                            full: e.full,
                            classify: e.classify,
                        },
                    )
                })
                .collect(),
        })
        .collect()
}

/// Flatten merchant keywords from the bundled default merchant rules, preserving
/// file order. Mirrors `runtime/merchant_rules.py::load_known_merchant_keywords`
/// for the default-only (no project override) case.
pub fn default_known_merchants() -> Vec<String> {
    let parsed: MerchantRulesToml = toml::from_str(DEFAULT_MERCHANT_RULES_TOML)
        .expect("bundled default_merchant_rules.toml is valid");
    parsed
        .rules
        .into_iter()
        .flat_map(|rule| rule.keywords)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The migration renamed keys into paths but must not have lost, gained, or
    /// altered a single beancount account — that is what keeps the corpus and the
    /// E2E fixtures meaningful across the format change.
    #[test]
    fn migrated_accounts_match_the_pre_vocabulary_table() {
        let legacy = legacy_category_accounts();
        let (_, migrated) = default_tag_vocabulary();
        assert_eq!(
            legacy.len(),
            migrated.len(),
            "account count changed during migration"
        );
        let mut legacy_accounts: Vec<&String> = legacy.values().collect();
        let mut migrated_accounts: Vec<&String> = migrated.values().collect();
        legacy_accounts.sort();
        migrated_accounts.sort();
        assert_eq!(
            legacy_accounts, migrated_accounts,
            "the set of beancount accounts changed during migration"
        );
    }

    /// Every account key must be a declared tag path, and every path's parent
    /// must exist — otherwise the vocabulary has holes the UI would have to guess
    /// its way around.
    #[test]
    fn bundled_vocabulary_is_closed() {
        let (vocabulary, accounts) = default_tag_vocabulary();
        let paths: std::collections::HashSet<&str> =
            vocabulary.iter().map(|n| n.path.as_str()).collect();
        for key in accounts.keys() {
            assert!(
                paths.contains(key.as_str()),
                "account key {key} is not a declared tag"
            );
        }
        for node in &vocabulary {
            if let Some(parent) = TagNode::parent(&node.path) {
                assert!(
                    paths.contains(parent),
                    "{} has no declared parent",
                    node.path
                );
            }
            assert!(
                !node.display.is_empty(),
                "{} has no display name",
                node.path
            );
        }
    }

    /// A misspelled tag used to silently invent a new tag. It is now an error
    /// naming the offending path — which is what makes import validation useful.
    #[test]
    fn unknown_tag_path_is_rejected() {
        let err = parser_rule_layers_with_overrides(&[r#"
[[rules]]
id = "typo"
keywords = ["ZZZ"]
tags = ["grocery/diary"]
"#])
        .expect_err("an undeclared tag path must not load");
        assert!(
            err.contains("grocery/diary"),
            "error should name the path: {err}"
        );
    }

    /// An override document may carry any mix of the three tables: declare a tag,
    /// map it to an account, and use it from a rule, all in one file.
    #[test]
    fn override_document_may_extend_vocabulary_and_accounts() {
        let layers = parser_rule_layers_with_overrides(&[r#"
[[tags]]
path = "grocery/dairy/kefir"
display = "Kefir"

[accounts]
"grocery/dairy/kefir" = "Expenses:Food:Grocery:Dairy:Kefir"

[[rules]]
id = "user_kefir"
keywords = ["ZZZ KEFIR BRAND"]
tags = ["grocery/dairy/kefir"]
priority = 5
"#])
        .expect("override loads");
        let rule = layers
            .category_rules
            .rules
            .iter()
            .find(|r| r.id.as_deref() == Some("user_kefir"))
            .expect("rule present");
        // Path expands to its segments, so the item still carries the ancestors.
        assert_eq!(rule.tags, vec!["grocery", "dairy", "kefir"]);
        assert_eq!(rule.category.as_deref(), Some("grocery/dairy/kefir"));
        assert!(layers
            .category_rules
            .tag_vocabulary
            .iter()
            .any(|n| n.path == "grocery/dairy/kefir"));
    }

    /// A rule naming a path with no `[accounts]` entry adds tags and claims
    /// nothing — no walking up to an ancestor that does have one. This is what
    /// keeps the former tag-only rules behaving as they always have.
    #[test]
    fn account_resolution_does_not_walk_up_to_ancestors() {
        let layers = default_parser_rule_layers();
        let milk = layers
            .category_rules
            .rules
            .iter()
            .find(|r| r.id.as_deref() == Some("semantic_tag_0101"))
            .expect("milk rule present");
        assert_eq!(milk.tag_paths, vec!["grocery/dairy/milk"]);
        assert_eq!(
            milk.category, None,
            "milk must not inherit the Dairy account from its parent"
        );
    }
}
