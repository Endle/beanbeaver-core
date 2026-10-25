//! Last-resort candidates for truncated receipt descriptions.
//!
//! Keep the calibrated length gates beside the fallback they constrain. The
//! parent invokes this only after ordinary and merchant-vocabulary matching
//! fail to find an account, then applies the same subtraction and ranking.

use super::matching::re_word_token;
use super::{mask_brands, resolve_account_target, CategoryRuleLayers, RuleMatch};

/// Minimum compact tail length for a truncated keyword.
///
/// Swept through the full scan pipeline (cached `device_sim --dump`, diffed
/// against the pre-fallback parser) over the September 2026 private corpus:
/// 160 receipts, 1,498 item lines. Line length >= 22, account rivals on. Counts
/// are lines the prefix stage newly categorizes; the Full Fortune filling rule's
/// exact hits are excluded.
///
/// | Tail | Categorized | Wrong | What changes                                           |
/// |------|-------------|-------|--------------------------------------------------------|
/// | 3    | 15          | 1     | + `Super Slim- Hea` (sanitary pads) as a prepared meal |
/// | 4    | 14          | 0     |                                                        |
/// | 5    | 9           | 0     | loses `Hazel Mush` x4 and `… Soft` (soft drink)        |
const PREFIX_MIN_TAIL: usize = 4;

/// Minimum printed description length, in Unicode characters, trimmed and
/// measured BEFORE brand masking. Fixed-width columns are commonly 25 chars;
/// OCR can drop spaces/hyphens. Same sweep as [`PREFIX_MIN_TAIL`], with
/// tail >= 4:
///
/// | Line | Categorized | Wrong | What changes                                           |
/// |------|-------------|-------|--------------------------------------------------------|
/// | 0    | 17          | 1     | + 3 short lines; a Costco discount line as Coffee      |
/// | 20   | 14          | 0     |                                                        |
/// | 22   | 14          | 0     | the highest value that keeps all 14                    |
/// | 23   | 13          | 0     | loses `Kitamo Ryoba Imitation` (22 chars)              |
/// | 24   | 13          | 0     |                                                        |
/// | 25   | 9           | 0     | loses `… Soft` and three 24-char `… Imitation` lines   |
///
/// These are calibration results, not an independent accuracy estimate.
const PREFIX_MIN_LINE_LEN: usize = 22;

/// Candidate matches for a fixed-width description's truncated suffix.
/// Call only after printed-text and merchant-vocabulary matching found no account.
/// ASCII letter/digit tokens are joined on both sides, allowing OCR splits and
/// punctuation. Tails start at a word boundary and consume every remaining
/// token: a trailing package size is never silently stripped to invent a cut.
/// Exact-only keywords participate in account ambiguity but never emit a match.
pub(super) fn prefix_fallback_matches(
    description: &str,
    layers: &CategoryRuleLayers,
) -> Vec<RuleMatch> {
    if description.trim().chars().count() < PREFIX_MIN_LINE_LEN {
        return Vec::new();
    }
    let masked = mask_brands(description, &layers.brands).to_ascii_uppercase();
    let words: Vec<&str> = re_word_token()
        .find_iter(&masked)
        .map(|m| m.as_str())
        .collect();
    let tails: Vec<String> = (0..words.len())
        .map(|start| words[start..].concat())
        .filter(|tail| tail.len() >= PREFIX_MIN_TAIL)
        .collect();
    let mut account = None;
    let mut matches = Vec::new();
    for (rule_index, rule) in layers.rules.iter().enumerate() {
        // A brand names the maker in full; a truncated tail is not its name.
        if rule.brand {
            continue;
        }
        let mut best: Option<(&String, usize)> = None;
        for keyword in &rule.keywords {
            let upper = keyword.to_ascii_uppercase();
            let compact: String = re_word_token()
                .find_iter(&upper)
                .map(|m| m.as_str())
                .collect();
            if !tails
                .iter()
                .any(|tail| tail.len() < compact.len() && compact.starts_with(tail))
            {
                continue;
            }
            if let Some(rival) =
                resolve_account_target(rule.category.as_deref(), &layers.account_mapping, None)
            {
                if account.as_ref().is_some_and(|previous| previous != &rival) {
                    return Vec::new();
                }
                account = Some(rival);
            }
            // Same length measure as `find_all_matches`, so ranking and the
            // explained `keyword_length` mean one thing across match kinds.
            let keyword_length = keyword.chars().filter(|ch| !ch.is_whitespace()).count();
            if !layers.exact_only_keywords.contains(keyword)
                && best.map_or(true, |(_, length)| keyword_length > length)
            {
                best = Some((keyword, keyword_length));
            }
        }
        if let Some((keyword, keyword_length)) = best {
            matches.push(RuleMatch {
                rule_id: rule.id.clone(),
                category: rule.category.clone(),
                tag_paths: rule.tag_paths.clone(),
                tags: rule.tags.clone(),
                matched_keyword: keyword.clone(),
                priority: rule.priority,
                keyword_length,
                is_exact: false,
                is_prefix: true,
                rule_index,
            });
        }
    }
    // A tag-only candidate cannot rescue an otherwise uncategorized item.
    if matches.iter().any(|m| m.category.is_some()) {
        matches
    } else {
        Vec::new()
    }
}
