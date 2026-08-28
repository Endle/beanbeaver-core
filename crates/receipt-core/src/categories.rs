use regex::Regex;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::ocr_confusion;

// Bigram-similarity bars for the fuzzy keyword stage, by keyword length.
//
// These are hand-tuned values, and they are tuned against the *specific* input
// normalization `legacy_bigram_collapse` applies — not against raw text. Read that
// function's docs before touching either: the two are coupled, and we intend to
// retune these gradually so the collapse can be deleted. Nudging a threshold to
// rescue one fixture, without a corpus run, is how this knot was tied.
const FUZZY_THRESHOLD_SHORT: f64 = 0.75;
const FUZZY_THRESHOLD_MEDIUM: f64 = 0.80;
const FUZZY_THRESHOLD_LONG: f64 = 0.70;

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
    pub rule_index: usize,
}

fn re_word_token() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[A-Z0-9]+").unwrap())
}

fn re_whitespace() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[^A-Z0-9]+").unwrap())
}

fn bigram_similarity(s1: &str, s2: &str) -> f64 {
    if s1.len() < 2 {
        return if s2.contains(s1) { 1.0 } else { 0.0 };
    }

    let bigrams1: HashSet<String> = s1
        .as_bytes()
        .windows(2)
        .map(|window| String::from_utf8_lossy(window).into_owned())
        .collect();
    let bigrams2: HashSet<String> = s2
        .as_bytes()
        .windows(2)
        .map(|window| String::from_utf8_lossy(window).into_owned())
        .collect();

    if bigrams1.is_empty() {
        return 0.0;
    }
    bigrams1.intersection(&bigrams2).count() as f64 / bigrams1.len() as f64
}

fn get_threshold(keyword_len: usize) -> f64 {
    if keyword_len <= 4 {
        FUZZY_THRESHOLD_SHORT
    } else if keyword_len <= 6 {
        FUZZY_THRESHOLD_MEDIUM
    } else {
        FUZZY_THRESHOLD_LONG
    }
}

/// True when `keyword` occurs in `description` allowing only OCR glyph noise —
/// the approximate counterpart of `str::find`, returning the hit offset.
///
/// Replaces what used to be an ad-hoc `'0' | 'D' => 'O'` collapse. That table
/// made `D` and `O` globally interchangeable (so `DOG` equalled `OOG`) while
/// knowing nothing about the other glyph pairs OCR actually confuses. Pricing the
/// alignment with the shared cost model is both narrower — `D`/`O` costs a real
/// 0.3 instead of being free — and broader, since every pair in the table now
/// counts, not just two.
fn confusable_find(keyword: &str, description: &str) -> Option<usize> {
    let needle: Vec<char> = keyword.chars().collect();
    let haystack: Vec<char> = description.chars().collect();
    let (cost, position) = ocr_confusion::min_substring_cost(&needle, &haystack);
    (cost <= ocr_confusion::NOISE_TOLERANCE).then_some(position)
}

fn contains_with_single_char_noise(keyword: &str, description: &str) -> Option<usize> {
    let kw_tokens: Vec<&str> = keyword
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .collect();
    if kw_tokens.len() < 2 {
        return None;
    }

    let normalized_desc = re_whitespace()
        .replace_all(&description.to_ascii_uppercase(), " ")
        .trim()
        .to_string();
    if normalized_desc.is_empty() {
        return None;
    }

    let mut pattern = format!(r"\b{}\b", regex::escape(kw_tokens[0]));
    for token in kw_tokens.iter().skip(1) {
        pattern.push_str(r"(?:\s+[A-Z0-9]\b)?\s+\b");
        pattern.push_str(&regex::escape(token));
        pattern.push_str(r"\b");
    }

    Regex::new(&pattern)
        .ok()
        .and_then(|regex| regex.find(&normalized_desc).map(|matched| matched.start()))
}

fn compact_without_spaces(value: &str) -> String {
    value.chars().filter(|ch| !ch.is_whitespace()).collect()
}

/// The original ad-hoc collapse, retained **only** as the bigram stage's input
/// normalization. Frozen — do not "fix", widen, or unify it without recalibrating
/// `FUZZY_THRESHOLD_*` against the corpus.
///
/// It is not a defensible OCR model (it makes `D` and `O` globally
/// interchangeable, so `DOG` equals `OOG`, while ignoring every other confusable
/// pair). Everywhere a *confusable-equality* decision is made, the shared
/// [`ocr_confusion`] cost model has replaced it. But this stage is different: the
/// fuzzy thresholds were hand-tuned against precisely this normalization, and it
/// turns out to be load-bearing by accident.
///
/// Measured on `LUCKY HENAN NOODLES` against the snacks keyword `NOODLE SNACK`:
///
/// | input normalization | bigram score | vs 0.70 bar |
/// |---|---|---|
/// | this collapse | 0.6667 | no match — correct |
/// | raw text | 0.7000 | matches — misfiles as snacks |
/// | same-glyph collapse | 0.7000 | matches — misfiles as snacks |
///
/// Collapsing `D` merges the keyword's `OD`/`DL` bigrams into `OO`/`OL`, shrinking
/// its bigram *set* and so lowering the intersection ratio. That accident is the
/// only thing holding a genuine false positive below the bar, and the rule table
/// makes it bite: the snacks rule is priority 80 against staple's 0, and
/// `compare_match_rank` weighs priority above exactness, so a fuzzy snacks hit
/// outranks a literal `NOODLES` staple hit.
///
/// # Planned direction — this function is meant to die
///
/// Keeping it is a staging decision, not an endorsement. The intended path, to be
/// walked **gradually**, a step per PR, each measured against the cached corpus:
///
/// 1. Retune `FUZZY_THRESHOLD_SHORT` / `_MEDIUM` / `_LONG` so the fuzzy stage no
///    longer depends on this collapse's accidental bigram-set shrinkage. The
///    `NOODLE SNACK` case above is the canary: it must stay below the bar on
///    **raw** input before the collapse can go.
/// 2. Stop compacting multi-word keywords across word boundaries, which is what
///    lets an 11-char keyword drift across `HENAN|NOODLES` and score at all.
/// 3. Delete this function and route the fuzzy stage through
///    [`ocr_confusion`] like every other stage.
///
/// Perfection is explicitly *not* the bar for those steps — core is expected to
/// carry some divergence, and a step that trades a stale false positive for a
/// smaller new one is still progress. Do not let "the corpus must stay at zero
/// divergences" block the sequence; do record what moved, in both directions.
///
/// Until step 1 lands, treat every constant this feeds as coupled: changing the
/// thresholds and this normalization independently is how the two silently
/// drifted apart in the first place.
fn legacy_bigram_collapse(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '0' | 'D' => 'O',
            _ => ch,
        })
        .collect()
}

fn fuzzy_contains(keyword: &str, description: &str, threshold: Option<f64>) -> (bool, isize, bool) {
    let desc_raw = description.to_ascii_uppercase();
    let kw_raw = keyword.trim().to_ascii_uppercase();
    let desc_conf_raw = ocr_confusion::canonicalize_same_glyph(&desc_raw);
    let kw_conf_raw = ocr_confusion::canonicalize_same_glyph(&kw_raw);
    let exact_only = threshold.is_some_and(|value| value >= 1.0);

    let kw_len_raw = kw_raw.chars().filter(|ch| !ch.is_whitespace()).count();
    if kw_len_raw <= 3 {
        let pattern = format!(r"\b{}\b", regex::escape(&kw_raw));
        if let Ok(regex) = Regex::new(&pattern) {
            if let Some(found) = regex.find(&desc_raw) {
                return (true, found.start() as isize, true);
            }
        }
        if !exact_only {
            for token_match in re_word_token().find_iter(&desc_raw) {
                if ocr_confusion::canonicalize_same_glyph(token_match.as_str()) == kw_conf_raw {
                    return (true, token_match.start() as isize, true);
                }
            }
        }
        return (false, -1, false);
    }

    let desc = compact_without_spaces(&desc_raw);
    let kw = compact_without_spaces(&kw_raw);

    if let Some(position) = desc.find(&kw) {
        return (true, position as isize, true);
    }
    if !exact_only {
        // Priced against the raw text, not the canonicalized copy: the cost model
        // already treats interchangeable glyphs as near-free, and keeps the pairs
        // it merely considers *plausible* (D/O and friends) at a real cost.
        if let Some(position) = confusable_find(&kw, &desc) {
            return (true, position as isize, true);
        }
    }

    if let Some(position) = contains_with_single_char_noise(&kw_raw, &desc_raw) {
        return (true, position as isize, true);
    }
    if !exact_only {
        if let Some(position) = contains_with_single_char_noise(&kw_conf_raw, &desc_conf_raw) {
            return (true, position as isize, true);
        }
    }

    let keyword_len = kw.chars().count();
    let threshold = threshold.unwrap_or_else(|| get_threshold(keyword_len));
    if threshold >= 1.0 {
        return (false, -1, false);
    }

    let desc_chars: Vec<char> = legacy_bigram_collapse(&desc).chars().collect();
    let kw_chars: Vec<char> = legacy_bigram_collapse(&kw).chars().collect();
    let window_size = keyword_len + 1;
    let mut best_similarity = 0.0;
    let mut best_position = -1;

    for start in 0..=(desc_chars.len().saturating_sub(keyword_len)) {
        let end = (start + window_size).min(desc_chars.len());
        let window: String = desc_chars[start..end].iter().collect();
        let keyword_string: String = kw_chars.iter().collect();
        let similarity = bigram_similarity(&keyword_string, &window);
        if similarity > best_similarity {
            best_similarity = similarity;
            best_position = start as isize;
        }
    }

    if best_similarity >= threshold {
        (true, best_position, false)
    } else {
        (false, -1, false)
    }
}

/// Locate `brand` in `description`, ignoring ASCII case and any whitespace on
/// either side of the comparison, and return the matched span as char indices.
///
/// Whitespace-insensitive because OCR splits and joins words freely
/// ("WHOLESALE" -> "WHOL ESALE" is a corpus regular), and anchored on word
/// boundaries because the short entries are the dangerous ones: an unanchored
/// two-letter brand would eat the middle of an unrelated product word.
fn brand_span(description: &[char], brand: &[char]) -> Option<(usize, usize)> {
    if brand.is_empty() {
        return None;
    }
    let alnum_at = |index: usize| description.get(index).is_some_and(|c| c.is_alphanumeric());
    for start in 0..description.len() {
        if description[start].is_whitespace() {
            continue;
        }
        if start > 0 && alnum_at(start - 1) && description[start].is_alphanumeric() {
            continue;
        }
        let mut cursor = start;
        let mut matched = 0;
        while matched < brand.len() && cursor < description.len() {
            let ch = description[cursor];
            if ch.is_whitespace() {
                cursor += 1;
                continue;
            }
            if ch.to_ascii_uppercase() != brand[matched] {
                break;
            }
            cursor += 1;
            matched += 1;
        }
        if matched == brand.len() && !alnum_at(cursor) {
            return Some((start, cursor));
        }
    }
    None
}

/// Blank out every declared brand name in `description` before the item
/// keywords are matched against it.
///
/// A brand is text that names the *maker*, not the product, so letting keywords
/// read it is a pure false-positive source. Two corpus lines paid for this:
/// FOODY MART's "Fish Well - Preserved Veg" matched `FISH` inside the brand and
/// filed pickles under Seafood:Fish, and "Meat Corner - AA Beef Pla" matched
/// `CORN` inside "Meat **Corn**er" and filed beef under Vegetable.
///
/// Deliberately NOT a split on " - ". Only 19% of corpus item lines carry that
/// separator at all — Costco and Walmart never do — and where it appears it is
/// not reliably a brand/product boundary ("GROUND PORK - REGULAR" is a product
/// on the left). Matching the brand wherever it occurs and removing exactly
/// that span is the only form that works on every merchant.
///
/// The span becomes spaces rather than being deleted so word boundaries survive:
/// deleting would fuse the two sides into a new token that matches nothing real.
///
/// Matching is exact (case- and whitespace-insensitive), never fuzzy. Brand
/// names are short and brand-ish, which is the same neighbourhood
/// `exact_only_keywords` exists to keep out of the fuzzy stage.
pub fn mask_brands(description: &str, brands: &[String]) -> String {
    if brands.is_empty() {
        return description.to_string();
    }
    let mut chars: Vec<char> = description.chars().collect();
    for brand in brands {
        let needle: Vec<char> = brand
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .map(|ch| ch.to_ascii_uppercase())
            .collect();
        // A brand can legitimately appear more than once on one line; each pass
        // blanks the first remaining occurrence, so this terminates.
        while let Some((start, end)) = brand_span(&chars, &needle) {
            for slot in &mut chars[start..end] {
                *slot = ' ';
            }
        }
    }
    chars.into_iter().collect()
}

pub fn find_all_matches(description: &str, rule_layers: &CategoryRuleLayers) -> Vec<RuleMatch> {
    let masked = mask_brands(description, &rule_layers.brands);
    let description = masked.as_str();
    let mut matches = Vec::new();

    for (rule_index, rule) in rule_layers.rules.iter().enumerate() {
        // Scan all keywords in this rule and keep the strongest match: an
        // exact (substring) hit beats a fuzzy hit, and among equally strong
        // hits the longer keyword wins. Without this preference, a fuzzy
        // match on an early keyword would mask an exact match on a later
        // keyword in the same rule — e.g. dairy's "MILK CHOCOLATE" fuzzy-
        // matching "Chocolate Milk" before its own exact "CHOCOLATE MILK"
        // keyword gets a chance.
        let mut best_keyword: Option<&String> = None;
        let mut best_is_exact = false;
        let mut best_keyword_length: usize = 0;
        for keyword in &rule.keywords {
            let threshold = if rule_layers.exact_only_keywords.contains(keyword) {
                Some(1.0)
            } else {
                None
            };
            let (matched, _, is_exact) = fuzzy_contains(keyword, description, threshold);
            if !matched {
                continue;
            }
            let kw_len = keyword.chars().filter(|ch| !ch.is_whitespace()).count();
            let strictly_better = match (best_is_exact, is_exact) {
                (false, true) => true,
                (true, false) => false,
                _ => kw_len > best_keyword_length,
            };
            if best_keyword.is_none() || strictly_better {
                best_keyword = Some(keyword);
                best_is_exact = is_exact;
                best_keyword_length = kw_len;
                if is_exact && kw_len >= rule.keywords.iter().map(|k| k.len()).max().unwrap_or(0) {
                    // Already the longest exact match possible for this rule.
                    break;
                }
            }
        }
        if let Some(keyword) = best_keyword {
            matches.push(RuleMatch {
                rule_id: rule.id.clone(),
                category: rule.category.clone(),
                tag_paths: rule.tag_paths.clone(),
                tags: rule.tags.clone(),
                matched_keyword: keyword.clone(),
                priority: rule.priority,
                keyword_length: best_keyword_length,
                is_exact: best_is_exact,
                rule_index,
            });
        }
    }

    matches
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

pub fn build_rule_layers(
    default_account_mapping: HashMap<String, String>,
    classifier_configs: Vec<BuildClassifierConfig>,
    account_configs: Vec<HashMap<String, String>>,
    tag_vocabulary: Vec<TagNode>,
) -> CategoryRuleLayers {
    let mut account_mapping = default_account_mapping;
    for config in account_configs {
        for (key, value) in config {
            let key = key.trim();
            let value = value.trim();
            if !key.is_empty() && !value.is_empty() {
                account_mapping.insert(key.to_string(), value.to_string());
            }
        }
    }

    let mut exact_only_keywords = HashSet::new();
    let mut brands: Vec<String> = Vec::new();
    let mut rules = Vec::new();

    for (idx, config) in classifier_configs.into_iter().enumerate() {
        let layer_priority = ((idx + 1) as i32) * 100;
        for brand in config.brands {
            let cleaned = brand.trim().to_ascii_uppercase();
            if !cleaned.is_empty() && !brands.contains(&cleaned) {
                brands.push(cleaned);
            }
        }
        for keyword in config.exact_only_keywords {
            let cleaned = keyword.trim();
            if !cleaned.is_empty() {
                exact_only_keywords.insert(cleaned.to_string());
            }
        }

        for rule in config.rules {
            if rule.keywords.is_empty() || rule.tag_paths.is_empty() {
                continue;
            }
            // Exact lookup, no walk-up: `grocery/dairy/milk` has no account
            // entry, so the milk rule adds a tag and claims nothing — which is
            // how the former tag-only rules keep behaving as they always have.
            let category = rule
                .tag_paths
                .iter()
                .find(|path| account_mapping.contains_key(*path))
                .cloned();
            let tags = expand_tag_paths(&rule.tag_paths);
            if rule.exact_only {
                for keyword in &rule.keywords {
                    exact_only_keywords.insert(keyword.clone());
                }
            }
            rules.push(CategoryRule {
                id: rule.id,
                remove_tags: rule.remove_tags,
                disables: rule.disables,
                keywords: rule.keywords,
                tag_paths: rule.tag_paths,
                category,
                tags,
                priority: rule.priority + layer_priority,
                exact_only: rule.exact_only,
                layer: idx,
            });
        }
    }

    CategoryRuleLayers {
        rules,
        exact_only_keywords,
        brands,
        account_mapping,
        tag_vocabulary,
    }
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
    let matches = find_all_matches(description, rule_layers);

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

pub fn classify_item_key(
    description: &str,
    rule_layers: &CategoryRuleLayers,
    default: Option<String>,
) -> Option<String> {
    let matches = resolve_matches(description, rule_layers);
    let best = matches
        .into_iter()
        .filter(|matched| matched.category.is_some())
        .max_by(compare_match_rank);
    best.and_then(|matched| matched.category).or(default)
}

pub fn classify_item_tags(description: &str, rule_layers: &CategoryRuleLayers) -> Vec<String> {
    let matches = resolve_matches(description, rule_layers);
    let mut tags = Vec::new();
    let mut seen = HashSet::new();

    for matched in matches {
        for tag in matched.tags {
            if seen.insert(tag.clone()) {
                tags.push(tag);
            }
        }
    }

    tags
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
mod tests {
    use super::{
        build_rule_layers, classify_item_key, classify_item_tags, list_item_categories,
        mask_brands, resolve_account_target, BuildClassifierConfig, BuildRuleEntry, TagNode,
    };
    use crate::rules::{default_category_accounts, default_parser_rule_layers};
    use std::collections::HashMap;

    /// Ported from desktop `tests/test_item_categories.py`: a large corpus of
    /// `description -> account` expectations the public classifier must satisfy.
    /// These are the real-receipt regressions (priority ordering, longer-keyword
    /// tiebreaks, brand-prefix wins, OCR D/O/0 confusable noise) that the desktop
    /// suite guards; here they run natively against `receipt-core`, no PyO3.
    #[test]
    fn public_classifier_categorizes_real_receipt_items() {
        let cases: &[(&str, &str)] = &[
            // staple — corn starch is sold beside the flour, but the produce
            // rule's bare "CORN" claimed it until "CORN STARCH" outgrew it on
            // the keyword-length tiebreak.
            ("WN CORN STARCH", "Expenses:Food:Grocery:Staple"),
            ("Cornstarch 454g", "Expenses:Food:Grocery:Staple"),
            // fruit — receipts print the vowel-dropped abbreviation, which the
            // full "WATERMELON" keyword never matches, so the drinks rule's
            // bare "WATER" claimed it.
            ("WATERMLN SGRBABY MRJ", "Expenses:Food:Grocery:Fruit"),
            ("WTRMLN SEEDLESS", "Expenses:Food:Grocery:Fruit"),
            // ...without dragging actual water out of the drinks rule.
            ("SPRING WATER 24PK", "Expenses:Food:Grocery:Drink"),
            // dairy — Neilson TruTaste milk matched no rule at all and fell
            // through to FIXME. TRUTASTE carries the match because the brand
            // token is what OCR mangles ("NLSN" -> "WLSN"/"HLSN"), not this.
            ("NLSN TRUTASTE 2%", "Expenses:Food:Grocery:Dairy"),
            ("WLSN TRUTASTE 28", "Expenses:Food:Grocery:Dairy"),
            // dairy — Loblaw's house-brand eggs print as "NN MED WHT OVWRP":
            // no name, Medium, WHiTe, OVerWRaP. The word "egg" is nowhere on
            // the line, so the phrase itself has to carry the match. OCR reads
            // the V as a U about as often as not, and truncates the tail.
            ("NN MED WHT OVWRP MRJ", "Expenses:Food:Grocery:Dairy"),
            ("NN MED WHT OUWRP MRJ", "Expenses:Food:Grocery:Dairy"),
            ("IN MED WHT O OUWRP", "Expenses:Food:Grocery:Dairy"),
            // seasoning
            (
                "SAPORITO FOODS CORN OIL 2.84L",
                "Expenses:Food:Grocery:Seasoning",
            ),
            (
                "FLOWER PERICARPIURN ZANTHOXYLI",
                "Expenses:Food:Grocery:Seasoning",
            ),
            (
                "T&T SLICED RED CHILI PEPPER",
                "Expenses:Food:Grocery:Seasoning",
            ),
            ("Pork Lard", "Expenses:Food:Grocery:Seasoning"),
            (
                "D.M.D White Pepper Powder",
                "Expenses:Food:Grocery:Seasoning",
            ),
            ("YYH Chillies 80g", "Expenses:Food:Grocery:Seasoning"),
            // alcoholic
            ("COORS LIGHT 6 PK HQ", "Expenses:Food:AlcoholicBeverage"),
            // pet
            ("2130150 HUGG WIPE", "Expenses:Pet:Supply"),
            // tooth care
            ("SONICARE TOOTHBRUSH HEADS", "Expenses:PersonalCare:Tooth"),
            ("1474938 COLGATE PR", "Expenses:PersonalCare:Tooth"),
            ("1457015 GLIDE ADV", "Expenses:PersonalCare:Tooth"),
            // Sensodyne brand name, no generic "TOOTHPASTE" token on the line
            // (Costco 2026-07-29).
            ("5592654 PRONAMEL", "Expenses:PersonalCare:Tooth"),
            // gift card: denomination glued to the brand, matched mid-token
            ("399 DOORDASH2X50", "Expenses:Food:Restaurant:GiftCard"),
            // snacks — a boxed Panda cookie is packaged, not fresh bakery. The
            // generic "COOKIE" keyword outranked every brand on the
            // keyword-length tiebreak until this rule was given a priority
            // (Costco 2026-08-05).
            ("969786 PANDA COOKIE", "Expenses:Food:Grocery:Snacks"),
            // dairy (incl. milk/chocolate tiebreak + single-char noise)
            ("Natrel - 2% Partly Skimme", "Expenses:Food:Grocery:Dairy"),
            ("Milk Chocolate 1%", "Expenses:Food:Grocery:Dairy"),
            (
                "NEILSON JOYYA CHOCOLATE E MILK",
                "Expenses:Food:Grocery:Dairy",
            ),
            ("1355285 TOGO VAN 2KG", "Expenses:Food:Grocery:Dairy"),
            // juice vs fruit-flavour keyword
            (
                "Tropicana - Blackberry Bl",
                "Expenses:Food:Grocery:Drink:Juice",
            ),
            // drink (brand-prefix / truncation / soft-drink-over-fruit)
            ("Soft Drink Orange", "Expenses:Food:Grocery:Drink"),
            ("YQSL - Grapefruit Tea Dri", "Expenses:Food:Grocery:Drink"),
            ("LZY - Original Flavor Dri", "Expenses:Food:Grocery:Drink"),
            ("Wing Hing Sweet Soy Bever", "Expenses:Food:Grocery:Drink"),
            ("*Yuan Qi Sen Lin Iced Tea", "Expenses:Food:Grocery:Drink"),
            (
                "'Tropicana Daily C Tea Dr ×1",
                "Expenses:Food:Grocery:Drink",
            ),
            ("TY - Lemon Tea", "Expenses:Food:Grocery:Drink"),
            // coffee
            ("108934 RAINFOREST", "Expenses:Food:Grocery:Drink:Coffee"),
            ("599010 LAVAZZA 1KG", "Expenses:Food:Grocery:Drink:Coffee"),
            // snacks
            ("HLY - Fish Cracker Seawee", "Expenses:Food:Grocery:Snacks"),
            ("1968518 WHITE RABBIT", "Expenses:Food:Grocery:Snacks"),
            ("*Sandwich Biscuits(Matcha)", "Expenses:Food:Grocery:Snacks"),
            (
                "*Or:ion Double Choco Pie 12",
                "Expenses:Food:Grocery:Snacks",
            ),
            ("La Pian (Spicy Gluten Sli", "Expenses:Food:Grocery:Snacks"),
            // staple
            ("LHL - Malatang Slightly S", "Expenses:Food:Grocery:Staple"),
            // seafood
            (
                "BQ - Frozen Raw Peeled Un",
                "Expenses:Food:Grocery:Seafood:Shrimp",
            ),
            // bakery
            ("BAKERY", "Expenses:Food:Grocery:Bakery"),
            ("Red Bean Pinapple Bun", "Expenses:Food:Grocery:Bakery"),
            // prepared meal
            ("Hot Food", "Expenses:Food:Grocery:PreparedMeal"),
            // meat (prefer over lard false positive)
            ("Pork Large Intestine", "Expenses:Food:Grocery:Meat"),
            // fruit header
            ("&& Fruit (FT)", "Expenses:Food:Grocery:Fruit"),
            // clothing
            ("1944033 CHAMP SHORT", "Expenses:Shopping:Clothing"),
            ("2946010 SKECHERSGLID", "Expenses:Shopping:Clothing"),
            ("3966510 FO TANK S", "Expenses:Shopping:Clothing"),
            // household supply (incl. LYSOL with D/O/0 OCR confusables)
            ("295619 KS BAGS 60", "Expenses:Home:HouseholdSupply"),
            ("1218587 SWIFFER DUST", "Expenses:Home:HouseholdSupply"),
            ("3458556 TIDE CQLDWTR", "Expenses:Home:HouseholdSupply"),
            ("1727590 CASCADE PLUS", "Expenses:Home:HouseholdSupply"),
            ("1185 BAKING SODA", "Expenses:Home:HouseholdSupply"),
            ("1796144 TOLIET 2PK", "Expenses:Home:HouseholdSupply"),
            // "Tissue" abbreviated on the slip, so the TISSUE keyword misses it
            // (FreshCo unknown-date_freshco_157_38).
            ("Bath Tiss Jmbo 202s", "Expenses:Home:HouseholdSupply"),
            ("LYSOL BATH P 059631882930", "Expenses:Home:HouseholdSupply"),
            ("LYS0L BATH P 059631882930", "Expenses:Home:HouseholdSupply"),
            ("LYSDL BATH P 059631882930", "Expenses:Home:HouseholdSupply"),
            // personal care
            ("443404 MARC ANTHONY", "Expenses:PersonalCare"),
        ];

        // Build the bundled layers once; classify to a key then resolve to an
        // account (the desktop `categorize_item` chain).
        let layers = default_parser_rule_layers();
        let mapping: HashMap<String, String> = layers.account_mapping.iter().cloned().collect();
        let categorize = |description: &str| -> Option<String> {
            let key = classify_item_key(description, &layers.category_rules, None)?;
            resolve_account_target(Some(&key), &mapping, None)
        };

        let mut failures = Vec::new();
        for (desc, expected) in cases {
            let got = categorize(desc);
            if got.as_deref() != Some(*expected) {
                failures.push(format!("{desc:?} => {got:?}, expected {expected:?}"));
            }
        }
        assert!(
            failures.is_empty(),
            "categorization drift:\n{}",
            failures.join("\n")
        );
    }

    /// Semantic classification (key + tags) from the bundled rules. Ported from
    /// the `classify_item_semantic` cases in `test_item_categories.py`; tags are
    /// checked as a superset to stay robust to match ordering.
    #[test]
    fn semantic_classification_yields_expected_key_and_tags() {
        let layers = default_parser_rule_layers();
        let key = |d: &str| classify_item_key(d, &layers.category_rules, None);
        let tags = |d: &str| classify_item_tags(d, &layers.category_rules);

        assert_eq!(key("347937 CHICKEN").as_deref(), Some("grocery/meat"));
        for t in ["grocery", "grocery/meat", "grocery/meat/chicken"] {
            assert!(
                tags("347937 CHICKEN").iter().any(|x| x == t),
                "CHICKEN missing tag {t}"
            );
        }

        // Two rules claim a boxed Panda cookie, and only one of them is wrong
        // in a way a keyword can fix. `panda_cookie_snack` outranks the bakery
        // rule's generic "COOKIE" for the ACCOUNT; the bakery *tag* stays,
        // because tags accumulate from every match by design and the bundled
        // corpus deliberately never subtracts (see
        // `rules::book::tests::bundled_corpus_uses_no_subtraction`).
        //
        // The staple tag is the one that was simply false: the staple rule's
        // "DANDAN" keyword fuzzy-matched "PANDA". It is `exact_only` now, so
        // dan dan noodles still classify and cookies no longer do.
        assert_eq!(
            key("969786 PANDA COOKIE").as_deref(),
            Some("grocery/snacks")
        );
        assert_eq!(
            tags("969786 PANDA COOKIE"),
            vec!["grocery", "grocery/bakery", "grocery/snacks"]
        );
        assert_eq!(key("DANDAN NOODLE").as_deref(), Some("grocery/staple"));

        // The other `exact_only` keyword, and the clearer case for the
        // mechanism: "GINGER" fuzzy-matched "FINGER", so a beef rib came back
        // tagged as seasoning — and the app shows the most specific tag, which
        // made a 60-dollar cut of beef read as a spice. Nothing was misread
        // here; GINGER and FINGER are simply one letter apart, which is a
        // neighbor the fuzzy stage should never have entertained.
        assert_eq!(
            key("Beef Rib Boneless Finger").as_deref(),
            Some("grocery/meat")
        );
        assert_eq!(
            tags("Beef Rib Boneless Finger"),
            vec!["grocery", "grocery/meat"]
        );
        // ...while ginger itself still classifies.
        assert_eq!(key("Ginger Root").as_deref(), Some("grocery/seasoning"));

        // Salt is a seasoning, not a staple: a FOODY MART scan filed
        // "Windsor - Table Salt" under Staple because "SALT" sat in the staple
        // rule's keyword list beside FLOUR and SUGAR.
        assert_eq!(
            key("Windsor - Table Salt").as_deref(),
            Some("grocery/seasoning")
        );
        assert_eq!(
            tags("Windsor - Table Salt"),
            vec!["grocery", "grocery/seasoning"]
        );

        assert_eq!(key("435259 FINE-FILT").as_deref(), Some("grocery/dairy"));
        for t in ["grocery", "grocery/dairy", "grocery/dairy/milk"] {
            assert!(
                tags("435259 FINE-FILT").iter().any(|x| x == t),
                "FINE-FILT missing tag {t}"
            );
        }

        // LCBO card: tags only, no spendable category.
        assert_eq!(key("810 LCBO CARD"), None);
        for t in ["alcohol", "gift_card"] {
            assert!(
                tags("810 LCBO CARD").iter().any(|x| x == t),
                "LCBO missing tag {t}"
            );
        }

        assert_eq!(
            key("2773717 MONSTER VRTY").as_deref(),
            Some("grocery/drink")
        );
        assert!(tags("2773717 MONSTER VRTY")
            .iter()
            .any(|x| x == "grocery/drink/energy_drink"));

        // FreshCo drops vowels ("Mnstr", "Wtr"), so the spelled-out `MONSTER` and
        // `COCONUT WATER` keywords never fire and a fuzzy hit fills the vacuum:
        // `RADISH` scores exactly 0.80 against a window of "Pa-RADIS-e", meeting
        // the 0.80 medium-keyword bar, and "Coconut Wtr" falls through to bare
        // `COCONUT` as fruit. The abbreviations are exact hits, and exactness
        // outranks fuzz at equal priority.
        assert_eq!(
            key("Mnstr Ultra Paradise").as_deref(),
            Some("grocery/drink")
        );
        assert!(tags("Mnstr Ultra Paradise")
            .iter()
            .any(|x| x == "grocery/drink/energy_drink"));
        assert_eq!(key("Cocomax Coconut Wtr").as_deref(), Some("grocery/drink"));

        // MARUTAI is a project-only override; public rules must not classify it.
        assert_eq!(key("MARUTAI"), None);
        assert!(tags("MARUTAI").is_empty());
    }

    /// Collisions that are decided by `keyword_length` — the THIRD key in
    /// [`compare_match_rank`], below `priority` and `is_exact`.
    ///
    /// These deserve a test of their own because the mechanism is invisible in
    /// the rule file: nothing in `default_item_classifier.toml` states that
    /// "SUNVINE" must outrank "SUGAR", it merely happens to be two characters
    /// longer. Adding a keyword elsewhere in the file can flip any of these
    /// silently, and the only other thing that would notice is a private-corpus
    /// fixture the public gate cannot see.
    ///
    /// `priority` cannot express these. It is a property of the *rule*, not the
    /// keyword, so lifting the fruit rule above the staple rule to win one line
    /// would also lift all ~45 of its other keywords — making "APPLE" beat the
    /// juice rule's "APPLE JUICE", and "MELON" beat "WINTER MELON". Priority
    /// also outranks exactness, so a bump lets fuzzy hits beat literal ones
    /// (see the `NOODLES`/snacks note on `fuzzy_contains`). Length sits below
    /// exactness and can never do that, which is why it stays the default and
    /// these assertions exist instead.
    ///
    /// Cases are from Foody Mart 2026-08-15 (`2026-08-15_foody_mart_47_48`),
    /// whose ~25-character description truncation is what strands the product
    /// word and leaves these collisions to be resolved by length at all.
    #[test]
    fn length_decided_keyword_collisions_stay_decided() {
        let layers = default_parser_rule_layers();
        let key = |d: &str| classify_item_key(d, &layers.category_rules, None);

        // "Sugar Baby" is a watermelon cultivar and "Sunvine" a watermelon
        // brand. The line truncates "Watermelon" to "Wate", so every watermelon
        // keyword misses and the staple rule's "SUGAR" is the only other hit —
        // it filed a melon as a staple. Both new keywords are longer than
        // "SUGAR" (10 and 7 vs 5); either alone would carry it, which is the
        // point of having both.
        assert_eq!(
            key("Sunvine - Sugar Baby Wate").as_deref(),
            Some("grocery/fruit")
        );
        // Guard the mechanism, not just the outcome: plain sugar is still a
        // staple, so this is a genuine collision and not the fruit rule
        // swallowing the keyword.
        assert_eq!(key("Rogers Sugar 2kg").as_deref(), Some("grocery/staple"));

        // "SCALLOP" (7) must outrank legacy_0001's "MEAT" (4) — the line
        // literally reads "Bay Scallop Meat". It must also land on plain
        // seafood rather than the narrow shrimp leaf it used to sit in.
        assert_eq!(
            key("BQ - Bay Scallop Meat 60-").as_deref(),
            Some("grocery/seafood")
        );
        // The sibling shellfish were deliberately left on the shrimp rule when
        // scallop moved off it. This asserts that scope decision, so whoever
        // finishes the job sees this line fail and updates it on purpose.
        assert_eq!(key("Squid Tent").as_deref(), Some("grocery/seafood/shrimp"));

        // Department-name line vs. the adjective inside a product name.
        // "VEGETABLE OIL" (13) must keep outranking "VEGETABLE" (9), or adding
        // the department word would have dragged every bottle of oil out of
        // seasoning.
        assert_eq!(key("Vegetables").as_deref(), Some("grocery/vegetable"));
        assert_eq!(
            key("Unico - Vegetable Oil").as_deref(),
            Some("grocery/seasoning")
        );

        // Not a length collision — a brand keyword standing in for a product
        // word that the truncation removed entirely. Included because it is the
        // same defect class: 紅薯粉絲 (sweet potato vermicelli) survives only on
        // the Chinese sub-line, which the OCR does not reliably detect, so
        // "VERMICELLI" has nothing to match and the line classified as nothing.
        assert_eq!(
            key("Shodoshima - Asian Style").as_deref(),
            Some("grocery/staple")
        );
    }

    /// The mask removes exactly the brand's own span, leaving the product text
    /// — and its word boundaries — intact for the keyword stage.
    #[test]
    fn mask_brands_blanks_the_brand_span_only() {
        let brands = vec!["FISH WELL".to_string()];
        assert_eq!(
            mask_brands("Fish Well - Preserved Veg", &brands),
            "          - Preserved Veg"
        );
        // Whitespace on either side is ignored, because OCR splits and joins
        // words freely.
        assert_eq!(mask_brands("FishWell Sauce", &brands), "         Sauce");
        assert_eq!(mask_brands("Fish  Well Sauce", &brands), "           Sauce");
        // Every occurrence goes, not just the first.
        assert_eq!(
            mask_brands("Fish Well Fish Well", &brands),
            "                   "
        );
    }

    /// Short brands are the dangerous ones, so a brand only matches on word
    /// boundaries — otherwise "LA" would eat the middle of "SALAD".
    #[test]
    fn mask_brands_requires_word_boundaries() {
        let brands = vec!["LA".to_string()];
        assert_eq!(mask_brands("Fresh Salad Mix", &brands), "Fresh Salad Mix");
        assert_eq!(mask_brands("LA - Rice Stick", &brands), "   - Rice Stick");
    }

    /// The two defects the brand table was introduced for: a keyword matching
    /// inside a brand name and claiming the whole line.
    #[test]
    fn brands_stop_keywords_matching_inside_the_maker_name() {
        let layers = default_parser_rule_layers();
        let key = |d: &str| classify_item_key(d, &layers.category_rules, None);

        // FISH (legacy_0002) matches "Fish Well"; PRESERVED VEG must win the line.
        assert_eq!(
            key("Fish Well - Preserved Veg").as_deref(),
            Some("grocery/seasoning")
        );
        // CORN matches inside "Meat Corner"; BEEF must win the line.
        assert_eq!(
            key("Meat Corner - AA Beef Pla").as_deref(),
            Some("grocery/meat")
        );
    }

    /// `list_item_categories` returns path-sorted (path, account) pairs drawn
    /// from both the account map and the rules. Mirrors the desktop test.
    #[test]
    fn list_item_categories_returns_sorted_key_account_pairs() {
        let config = BuildClassifierConfig {
            exact_only_keywords: vec![],
            brands: vec![],
            rules: vec![BuildRuleEntry {
                id: None,
                keywords: vec!["CUSTOM DIRECT ACCOUNT".to_string()],
                tag_paths: vec!["project/custom".to_string()],
                priority: 0,
                exact_only: false,
                ..Default::default()
            }],
        };
        let mut accounts = default_category_accounts();
        accounts.insert(
            "project/custom".to_string(),
            "Expenses:Project:Custom".to_string(),
        );
        accounts.insert("zzz_custom".to_string(), "Expenses:Project:Zzz".to_string());
        let layers = build_rule_layers(
            accounts,
            vec![config],
            vec![],
            vec![TagNode {
                path: "project/custom".to_string(),
                display: "Custom".to_string(),
            }],
        );

        let categories = list_item_categories(&layers);
        let mut sorted = categories.clone();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(categories, sorted, "not sorted by path");
        assert!(categories.contains(&(
            "project/custom".to_string(),
            "Expenses:Project:Custom".to_string()
        )));
        assert!(
            categories.contains(&("zzz_custom".to_string(), "Expenses:Project:Zzz".to_string()))
        );
    }

    /// A project rule reaches an account by declaring a tag path that the
    /// project's own `[accounts]` table maps (mirrors
    /// `test_project_rule_key_maps_via_account_config`).
    #[test]
    fn project_rule_key_maps_via_account_config() {
        let config = BuildClassifierConfig {
            exact_only_keywords: vec![],
            brands: vec![],
            rules: vec![BuildRuleEntry {
                id: None,
                keywords: vec!["CUSTOM NOODLE BRAND".to_string()],
                tag_paths: vec!["grocery/staple".to_string()],
                priority: 20,
                exact_only: true,
                ..Default::default()
            }],
        };
        let accounts: HashMap<String, String> = [(
            "grocery/staple".to_string(),
            "Expenses:Food:Grocery:Staple".to_string(),
        )]
        .into_iter()
        .collect();
        let layers = build_rule_layers(
            accounts,
            vec![config],
            vec![],
            vec![TagNode {
                path: "grocery/staple".to_string(),
                display: "Staple".to_string(),
            }],
        );
        let key = classify_item_key("CUSTOM NOODLE BRAND", &layers, None).expect("classifies");
        let mapping: HashMap<String, String> = layers
            .account_mapping
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        assert_eq!(
            resolve_account_target(Some(&key), &mapping, None).as_deref(),
            Some("Expenses:Food:Grocery:Staple")
        );
    }
}
