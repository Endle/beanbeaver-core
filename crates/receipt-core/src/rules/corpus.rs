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
use crate::receipt_categories::{build_rule_layers, BuildClassifierConfig, BuildRuleEntry};
use crate::receipt_parser::ParserRuleLayers;

pub(crate) const DEFAULT_ITEM_CLASSIFIER_TOML: &str =
    include_str!("../../../../rules/default_item_classifier.toml");
pub(crate) const DEFAULT_MERCHANT_RULES_TOML: &str =
    include_str!("../../../../rules/default_merchant_rules.toml");
pub(crate) const DEFAULT_MERCHANT_FAMILIES_TOML: &str =
    include_str!("../../../../rules/default_merchant_families.toml");
pub(crate) const DEFAULT_MERCHANT_VOCAB_TOML: &str =
    include_str!("../../../../rules/default_merchant_vocab.toml");

/// Two-stage category-key -> beancount-account mapping. Ported verbatim from
/// `receipt/item_categories.py::DEFAULT_CATEGORY_ACCOUNTS`.
pub fn default_category_accounts() -> HashMap<String, String> {
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
#[derive(Debug, Deserialize)]
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
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    tags: StringOrList,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    exact_only: bool,
}

#[derive(Debug, Default, Deserialize)]
struct ClassifierToml {
    #[serde(default)]
    exact_only_keywords: StringOrList,
    #[serde(default)]
    rules: Vec<RuleToml>,
}

fn to_build_config(parsed: ClassifierToml) -> BuildClassifierConfig {
    BuildClassifierConfig {
        exact_only_keywords: parsed.exact_only_keywords.into_trimmed(),
        rules: parsed
            .rules
            .into_iter()
            .map(|rule| {
                // `key` wins over `category`, then trim and treat empty as absent
                // (matches the PyBuildRuleEntry extraction order).
                let target = rule
                    .key
                    .or(rule.category)
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty());
                BuildRuleEntry {
                    id: rule
                        .id
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty()),
                    keywords: rule.keywords.into_trimmed(),
                    target,
                    tags: normalize_tags(rule.tags.into_trimmed()),
                    priority: rule.priority,
                    exact_only: rule.exact_only,
                }
            })
            .collect(),
    }
}

fn parse_classifier(toml_text: &str) -> ClassifierToml {
    toml::from_str(toml_text).expect("bundled default_item_classifier.toml is valid")
}

/// Build the default item-category rule layers from the bundled classifier TOML
/// + the default account mapping (no project-local overrides — the iOS case).
pub fn default_parser_rule_layers() -> ParserRuleLayers {
    let config = to_build_config(parse_classifier(DEFAULT_ITEM_CLASSIFIER_TOML));
    finish_layers(build_rule_layers(
        default_category_accounts(),
        vec![config],
        vec![],
    ))
}

/// Build item-category rule layers from the bundled default classifier plus zero
/// or more override classifier TOMLs (later layers win, same as the desktop
/// `classifier_configs=(default, override)` layering). Used by the private E2E
/// harness, which asserts a few categories that live in an out-of-tree
/// `private_rules.toml` rather than the public defaults. Merchant rules are
/// unaffected (still the public defaults).
///
/// Returns `Err` when an override TOML fails to parse (never panics on user
/// input — callers on the FFI path map this to a typed error).
pub fn parser_rule_layers_with_overrides(
    override_classifier_tomls: &[&str],
) -> Result<ParserRuleLayers, String> {
    let mut configs = vec![to_build_config(parse_classifier(
        DEFAULT_ITEM_CLASSIFIER_TOML,
    ))];
    for (i, text) in override_classifier_tomls.iter().enumerate() {
        let parsed: ClassifierToml = toml::from_str(text)
            .map_err(|e| format!("invalid override classifier TOML (layer {i}): {e}"))?;
        configs.push(to_build_config(parsed));
    }
    Ok(finish_layers(build_rule_layers(
        default_category_accounts(),
        configs,
        vec![],
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
