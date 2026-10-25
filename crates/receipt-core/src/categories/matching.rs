//! Candidate matching: OCR-tolerant keywords and exact brand spans.
//!
//! This stage reports each rule's strongest keyword hit. Account ranking,
//! subtraction and fallback ordering belong to the parent module.

use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use super::{CategoryRuleLayers, RuleMatch};
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

pub(super) fn re_word_token() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[A-Z0-9]+").unwrap())
}

fn re_whitespace() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[^A-Z0-9]+").unwrap())
}

/// A keyword-derived pattern, compiled once per process and then reused.
///
/// `find_all_matches` calls `fuzzy_contains` once per keyword per rule per
/// item, and two of its stages build a pattern from the keyword alone. They used
/// to compile it on every call. A pattern is a pure function of its text, so a
/// memo keyed by that text cannot go stale however the rule layers are edited or
/// overlaid; it grows only with the distinct keywords a process has seen.
///
/// Handed out as an `Arc` rather than a `Regex` clone on purpose: cloning a
/// `Regex` gives the clone a fresh match cache, so every lookup would rebuild
/// the lazy DFA it had just built for the previous one.
fn keyword_regex(pattern: &str) -> Option<Arc<Regex>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<Regex>>>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(compiled) = cache.get(pattern) {
        return compiled.clone();
    }
    let compiled = Regex::new(pattern).ok().map(Arc::new);
    cache.insert(pattern.to_string(), compiled.clone());
    compiled
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

    keyword_regex(&pattern)
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
        if let Some(regex) = keyword_regex(&pattern) {
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
    let original: Vec<char> = description.chars().collect();
    let masked = mask_brands(description, &rule_layers.brands);
    let description = masked.as_str();
    let mut matches = Vec::new();

    for (rule_index, rule) in rule_layers.rules.iter().enumerate() {
        if rule.brand {
            // Exactly the spans `mask_brands` blanked, so a brand's tags apply
            // precisely when its name was taken away from every other rule.
            for keyword in &rule.keywords {
                let needle: Vec<char> = keyword
                    .chars()
                    .filter(|ch| !ch.is_whitespace())
                    .map(|ch| ch.to_ascii_uppercase())
                    .collect();
                if brand_span(&original, &needle).is_some() {
                    matches.push(RuleMatch {
                        rule_id: rule.id.clone(),
                        category: rule.category.clone(),
                        tag_paths: rule.tag_paths.clone(),
                        tags: rule.tags.clone(),
                        matched_keyword: keyword.clone(),
                        priority: rule.priority,
                        keyword_length: needle.len(),
                        is_exact: true,
                        is_prefix: false,
                        rule_index,
                    });
                    break;
                }
            }
            continue;
        }
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
                is_prefix: false,
                rule_index,
            });
        }
    }

    matches
}
