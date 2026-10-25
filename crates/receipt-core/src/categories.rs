//! Item classification: public rule types, match resolution and account policy.
//!
//! `layers` constructs rules, `matching` finds keyword/brand candidates, and
//! `prefix` supplies truncated-tail candidates. This module owns subtraction,
//! ranking and the printed-text -> merchant-vocabulary -> prefix stage order.
//! Public entry points stay under `categories` regardless of their implementation.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

mod layers;
mod matching;
mod prefix;

pub use layers::build_rule_layers;
pub use matching::{find_all_matches, mask_brands};
use prefix::prefix_fallback_matches;

/// One node in the item tag vocabulary.
///
/// A tag is a **path** (`grocery/dairy`), so the same leaf word can sit under two
/// parents (`household/supply`, `pet/supply`) with no ambiguity. `display` is
/// authored rather than derived — capitalizing the segment is what rendered
/// `energy_drink` as "Energy_drink" in the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagNode {
    pub path: String,
    pub display: String,
}

impl TagNode {
    /// The path split into its segment names, least specific first — exactly the
    /// flat tag list consumers receive for an item carrying this tag.
    pub fn segments(path: &str) -> Vec<String> {
        path.split('/')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// This node's parent path, or `None` for a root.
    pub fn parent(path: &str) -> Option<&str> {
        path.rsplit_once('/').map(|(head, _)| head)
    }
}

#[derive(Clone, Debug)]
pub struct CategoryRule {
    /// Provenance label from the source TOML (`legacy_0000`, `semantic_tag_0101`,
    /// …). Carried purely so a match can be traced back to the rule that caused
    /// it; the classifier never reads it. `None` for rules built in code.
    pub id: Option<String>,
    pub keywords: Vec<String>,
    /// The tag paths this rule declares, e.g. `["grocery/dairy"]`. A rule may
    /// declare several unrelated paths (`["alcohol", "gift_card"]`).
    pub tag_paths: Vec<String>,
    /// The one declared path that claims an account — the first `tag_paths`
    /// entry present in the account mapping. `None` for a rule that only adds
    /// tags. Resolution is exact: a path with no entry does **not** inherit its
    /// ancestor's account.
    pub category: Option<String>,
    /// `tag_paths` expanded to segment names, least specific first — the flat
    /// list consumers receive.
    pub tags: Vec<String>,
    /// Priority **after** the layer boost applied by [`build_rule_layers`].
    pub priority: i32,
    /// Whether the source rule demanded exact matching. The classifier reads the
    /// union set `CategoryRuleLayers::exact_only_keywords`, not this field — it is
    /// kept per-rule so the rule can be displayed accurately.
    pub exact_only: bool,
    /// Tag paths this rule *subtracts* when it matches. Applied after every
    /// rule's additions and regardless of layer order, so the effect does not
    /// depend on where the rule sits.
    pub remove_tags: Vec<String>,
    /// Rule ids whose match this rule voids entirely when it matches.
    pub disables: Vec<String>,
    /// Index of the classifier config this rule came from: 0 is the bundled
    /// defaults, 1+ are override layers in the order they were supplied.
    pub layer: usize,
    /// A `[[brands]]` entry that declares tags. Its one keyword is the brand
    /// name, matched exactly against the description *before* brands are
    /// masked — the masked text no longer contains it. See [`mask_brands`].
    pub brand: bool,
}

#[derive(Clone, Debug)]
pub struct CategoryRuleLayers {
    pub rules: Vec<CategoryRule>,
    pub exact_only_keywords: HashSet<String>,
    /// Brand names whose matched span is blanked out before any keyword is
    /// matched. See [`mask_brands`].
    pub brands: Vec<String>,
    /// Tag path -> beancount account. Ledger policy, overridable.
    pub account_mapping: HashMap<String, String>,
    /// The declared tag vocabulary these rules are validated against.
    pub tag_vocabulary: Vec<TagNode>,
}

#[derive(Clone, Debug, Default)]
pub struct BuildRuleEntry {
    /// Optional provenance label; see [`CategoryRule::id`].
    pub id: Option<String>,
    pub keywords: Vec<String>,
    /// Declared tag paths. Replaces the old `target` + `tags` pair, which the
    /// corpus wrote twice: every rule's `category` key was its tag list joined
    /// by `_` (measured: 83 of 83 agreed, zero disagreements).
    pub tag_paths: Vec<String>,
    /// Priority as declared in the source, **before** the layer boost.
    pub priority: i32,
    pub exact_only: bool,
    /// See [`CategoryRule::remove_tags`].
    pub remove_tags: Vec<String>,
    /// See [`CategoryRule::disables`].
    pub disables: Vec<String>,
    /// See [`CategoryRule::brand`].
    pub brand: bool,
}

#[derive(Clone, Debug)]
pub struct BuildClassifierConfig {
    pub exact_only_keywords: Vec<String>,
    /// Brand names to blank out before matching. See [`mask_brands`].
    pub brands: Vec<String>,
    pub rules: Vec<BuildRuleEntry>,
}

#[derive(Clone, Debug)]
pub struct RuleMatch {
    /// Provenance id of the rule that matched, when it had one — what
    /// `disables` refers to.
    pub rule_id: Option<String>,
    pub category: Option<String>,
    /// The tag paths this rule declares. Subtraction happens at path level:
    /// removing `grocery/snacks` drops that path but leaves `grocery` standing
    /// if another surviving path still implies it.
    pub tag_paths: Vec<String>,
    pub tags: Vec<String>,
    pub matched_keyword: String,
    pub priority: i32,
    pub keyword_length: usize,
    pub is_exact: bool,
    /// A last-resort match against the truncated end of a printed description.
    /// Kept internal to core; consumers display it as a non-exact (fuzzy) hit.
    pub is_prefix: bool,
    pub rule_index: usize,
}

fn compare_match_rank(left: &RuleMatch, right: &RuleMatch) -> Ordering {
    left.priority
        .cmp(&right.priority)
        .then_with(|| (left.is_exact as u8).cmp(&(right.is_exact as u8)))
        .then_with(|| left.keyword_length.cmp(&right.keyword_length))
        .then_with(|| right.rule_index.cmp(&left.rule_index))
}

/// Resolve a declared tag path to a beancount account.
///
/// **Exact lookup only** — a path with no entry does not inherit its ancestor's
/// account. That is what keeps a rule declaring `grocery/dairy/milk` (which has
/// no account of its own) from claiming Dairy.
///
/// An `Expenses:`-prefixed value passes straight through, so an override
/// document may name an account inline rather than adding an `[accounts]` entry.
pub fn resolve_account_target(
    target: Option<&str>,
    account_mapping: &HashMap<String, String>,
    default: Option<&str>,
) -> Option<String> {
    let Some(raw) = target else {
        return default.map(str::to_string);
    };
    let cleaned = raw.trim();
    if cleaned.is_empty() {
        return default.map(str::to_string);
    }
    if cleaned.starts_with("Expenses:") {
        return Some(cleaned.to_string());
    }
    account_mapping
        .get(cleaned)
        .map(String::as_str)
        .or(default)
        .map(str::to_string)
}

/// The matches that survive subtraction, plus the tag paths each still carries.
///
/// Two subtractive operators run here, both collected from *every* matching rule
/// before anything is applied — so neither depends on layer order, which is what
/// makes them explainable in a UI:
///
/// * `disables` voids a rule's match outright, by id. **Every** matching rule's
///   `disables` apply simultaneously, including those of a rule that is itself
///   disabled — so the disabled set is a pure function of which rules matched,
///   computed in one pass. The alternative (only surviving rules may disable) is
///   circular: what survives depends on what disables, and what disables depends
///   on what survives. Resolving that needs either a fixpoint or an evaluation
///   order, and both make the outcome harder to explain in a UI than "every rule
///   that matched had its say".
/// * `remove_tags` subtracts tag **paths**. Removing `grocery/snacks` drops that
///   path but leaves `grocery` standing when another surviving path still
///   implies it, which a flat tag-name subtraction could not express.
///
/// A match whose account-claiming path is removed stops claiming that account:
/// filing an item under Snacks while refusing to tag it `snacks` would be
/// incoherent.
pub fn resolve_matches(description: &str, rule_layers: &CategoryRuleLayers) -> Vec<RuleMatch> {
    resolve_match_set(find_all_matches(description, rule_layers), rule_layers)
}

fn resolve_match_set(matches: Vec<RuleMatch>, rule_layers: &CategoryRuleLayers) -> Vec<RuleMatch> {
    let disabled: HashSet<&str> = matches
        .iter()
        .flat_map(|matched| {
            rule_layers.rules[matched.rule_index]
                .disables
                .iter()
                .map(String::as_str)
        })
        .collect();
    let mut surviving: Vec<RuleMatch> = if disabled.is_empty() {
        matches
    } else {
        matches
            .into_iter()
            .filter(|matched| {
                // `map_or(true, ..)` rather than `is_none_or`: the latter is
                // stable only since 1.82 and this workspace's MSRV is 1.80.
                matched
                    .rule_id
                    .as_deref()
                    .map_or(true, |id| !disabled.contains(id))
            })
            .collect()
    };

    let removed: HashSet<&str> = surviving
        .iter()
        .flat_map(|matched| {
            rule_layers.rules[matched.rule_index]
                .remove_tags
                .iter()
                .map(String::as_str)
        })
        .collect();
    if !removed.is_empty() {
        for matched in &mut surviving {
            matched
                .tag_paths
                .retain(|path| !removed.contains(path.as_str()));
            matched.tags = expand_tag_paths(&matched.tag_paths);
            if matched
                .category
                .as_deref()
                .is_some_and(|path| removed.contains(path))
            {
                matched.category = None;
            }
        }
        surviving.retain(|matched| !matched.tag_paths.is_empty());
    }

    surviving
}

/// Expand tag paths to the full chain of node paths they imply, least specific
/// first, deduped in first-seen order.
///
/// `["grocery/dairy/milk"]` yields `["grocery", "grocery/dairy",
/// "grocery/dairy/milk"]` — every ancestor as a *path*, not a bare segment name.
///
/// Bare segments would be lossy: a rule declaring two unrelated paths
/// (`["alcohol", "gift_card"]`) flattens to segments that cannot be told apart
/// from one nested path, so nothing downstream could reconstruct the tree. Paths
/// also let the account map be consulted directly, since it is keyed by path.
pub fn expand_tag_paths(paths: &[String]) -> Vec<String> {
    let mut expanded = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        let mut prefix = String::new();
        for segment in path.split('/').map(str::trim).filter(|s| !s.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(segment);
            if seen.insert(prefix.clone()) {
                expanded.push(prefix.clone());
            }
        }
    }
    expanded
}

/// Classification from one matching pass. The winning path and resolved account
/// are separate from the union of semantic tags contributed by all rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemClassification {
    pub tag_path: Option<String>,
    pub account: Option<String>,
    pub tags: Vec<String>,
}

fn classification_from_matches(
    matches: &[RuleMatch],
    layers: &CategoryRuleLayers,
) -> ItemClassification {
    let tag_path = matches
        .iter()
        .filter(|m| m.category.is_some())
        .max_by(|a, b| compare_match_rank(a, b))
        .and_then(|m| m.category.clone());
    let account = resolve_account_target(tag_path.as_deref(), &layers.account_mapping, None);
    let mut seen = HashSet::new();
    let tags = matches
        .iter()
        .flat_map(|m| m.tags.iter())
        .filter(|tag| seen.insert((*tag).clone()))
        .cloned()
        .collect();
    ItemClassification {
        tag_path,
        account,
        tags,
    }
}

pub fn classify_item(description: &str, layers: &CategoryRuleLayers) -> ItemClassification {
    classification_from_matches(&resolve_matches(description, layers), layers)
}

pub fn classify_item_key(
    description: &str,
    layers: &CategoryRuleLayers,
    default: Option<String>,
) -> Option<String> {
    classify_item(description, layers).tag_path.or(default)
}

pub fn classify_item_tags(description: &str, layers: &CategoryRuleLayers) -> Vec<String> {
    classify_item(description, layers).tags
}

pub(crate) fn explain_classification(
    description: &str,
    layers: &CategoryRuleLayers,
) -> (ItemClassification, Vec<RuleMatch>) {
    let (classification, mut matches) = classify_receipt_description(description, layers, None);
    matches.sort_by(|a, b| compare_match_rank(b, a));
    (classification, matches)
}

/// Shared scan/explanation path. User-typed renames use `classify_item` instead.
pub(crate) fn classify_receipt_description(
    description: &str,
    layers: &CategoryRuleLayers,
    vocab: Option<&crate::merchant_vocab::MerchantVocab>,
) -> (ItemClassification, Vec<RuleMatch>) {
    let mut raw = find_all_matches(description, layers);
    let mut matches = resolve_match_set(raw.clone(), layers);
    let mut classification = classification_from_matches(&matches, layers);
    if classification.account.is_none() {
        if let Some(expanded) =
            vocab.and_then(|v| crate::merchant_vocab::expand_for_classification(description, v))
        {
            raw = find_all_matches(&expanded, layers);
            matches = resolve_match_set(raw.clone(), layers);
            classification = classification_from_matches(&matches, layers);
        }
    }
    if classification.account.is_none() {
        for candidate in prefix_fallback_matches(description, layers) {
            if !raw.iter().any(|m| m.rule_index == candidate.rule_index) {
                raw.push(candidate);
            }
        }
        // Preserve rule order for tag/display ordering, and honor subtraction
        // from literal matches even when those matches claim no account.
        raw.sort_by_key(|m| m.rule_index);
        matches = resolve_match_set(raw, layers);
        classification = classification_from_matches(&matches, layers);
    }
    (classification, matches)
}

pub fn list_item_categories(rule_layers: &CategoryRuleLayers) -> Vec<(String, String)> {
    let mut categories = HashSet::new();

    categories.extend(rule_layers.account_mapping.keys().cloned());
    for rule in &rule_layers.rules {
        if let Some(category) = &rule.category {
            categories.insert(category.clone());
        }
    }

    let mut sorted = categories.into_iter().collect::<Vec<_>>();
    sorted.sort();
    sorted
        .into_iter()
        .map(|category| {
            let account = rule_layers
                .account_mapping
                .get(&category)
                .cloned()
                .or_else(|| {
                    if category.starts_with("Expenses:") {
                        Some(category.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            (category, account)
        })
        .collect()
}

pub fn sorted_matches_for_debug(
    description: &str,
    rule_layers: &CategoryRuleLayers,
) -> Vec<RuleMatch> {
    let mut matches = resolve_matches(description, rule_layers);
    matches.sort_by(|left, right| compare_match_rank(right, left));
    matches
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod prefix_tests;
