//! OCR glyph-confusion costs and the fuzzy string distance built on them.
//!
//! Receipt OCR does not fail uniformly: some substitutions are near-certain
//! artifacts of the glyph set (`0` for `O`), others are plausible only when a
//! stroke fades or blots (`M` for `H`), and most are simply wrong. Charging every
//! substitution the same price throws that information away, so this module
//! grades it.
//!
//! Costs are **discrete tiers, deliberately not tuned floats.** The tiers encode
//! a visual argument a reviewer can check by looking at the glyphs; a fitted
//! `0.37` would encode noise from whatever corpus produced it. Do not replace
//! them with per-pair regressed values without a corpus large enough to have real
//! per-pair counts.
//!
//! Everything here assumes input is already uppercased and ASCII — callers
//! normalize first (see `merchant_match::normalize`), so lowercase pairs are
//! intentionally absent from the table.

/// Same printed glyph, different codepoint. OCR choosing the wrong one is barely
/// an error at all, so it costs almost nothing.
const COST_SAME_GLYPH: f64 = 0.1;

/// Distinct glyphs sharing a stroke skeleton: a faded, blotted, or bridged
/// stroke turns one into the other. `M`/`H` is the motivating case — a thermal
/// `M` whose middle diagonal drops out reads as `H`.
///
/// Keep this tier evidence-driven. `M`/`N` was once listed here on plausibility
/// alone and had to be removed: it made the bakery keyword `NAAN` match
/// `VAN`**`NAAM`**`EI` (a frozen shrimp item) for 0.3, inside the equality budget,
/// which misfiled two corpus fixtures as `Bakery`. A pair that only *sounds*
/// reasonable is a false-positive generator — add one when a real receipt
/// demands it, not before.
const COST_SHARED_SKELETON: f64 = 0.3;

/// Confusable only under heavy smearing. Plausible, but weak evidence, so it
/// costs most of a full edit.
const COST_SMEAR_ONLY: f64 = 0.6;

/// No visual relationship — a whole edit. Also the ceiling: no substitution ever
/// costs more than an insert or a delete.
const COST_UNRELATED: f64 = 1.0;

/// Cost in `[0, 1]` of OCR reading `a` where `b` was printed (or vice versa —
/// the table is symmetric).
///
/// Real OCR confusion is mildly directional: `0`→`O` and `O`→`0` differ in
/// likelihood depending on whether the surrounding field is numeric. Modelling
/// that is not worth the table doubling, so this stays order-independent.
pub fn confusion_cost(a: char, b: char) -> f64 {
    if a == b {
        return 0.0;
    }
    // ASCII digits sort before uppercase letters, so `lo`/`hi` gives each pair
    // exactly one spelling in the match below.
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    match (lo, hi) {
        ('0', 'O')
        | ('1', 'I')
        | ('1', 'L')
        | ('I', 'L')
        | ('5', 'S')
        | ('8', 'B')
        | ('2', 'Z')
        | ('6', 'G') => COST_SAME_GLYPH,

        ('0', 'D')
        | ('0', 'Q')
        | ('D', 'O')
        | ('O', 'Q')
        | ('C', 'G')
        | ('E', 'F')
        | ('H', 'M')
        | ('H', 'N')
        | ('M', 'W')
        | ('U', 'V') => COST_SHARED_SKELETON,

        ('P', 'R') | ('K', 'X') => COST_SMEAR_ONLY,

        _ => COST_UNRELATED,
    }
}

/// Levenshtein distance with [`confusion_cost`] pricing substitutions; inserts
/// and deletes cost a full 1.0.
///
/// Because a confusable swap is cheap, OCR's systematic glyph errors rank far
/// ahead of coincidental edits of the same count — which is the whole point of
/// grading them.
pub fn weighted_distance(a: &[char], b: &[char]) -> f64 {
    if a.is_empty() {
        return b.len() as f64;
    }
    if b.is_empty() {
        return a.len() as f64;
    }
    let mut prev: Vec<f64> = (0..=b.len()).map(|j| j as f64).collect();
    let mut curr: Vec<f64> = vec![0.0; b.len() + 1];
    for i in 1..=a.len() {
        curr[0] = i as f64;
        for j in 1..=b.len() {
            let sub = confusion_cost(a[i - 1], b[j - 1]);
            curr[j] = (prev[j] + 1.0)
                .min(curr[j - 1] + 1.0)
                .min(prev[j - 1] + sub);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Budget for treating two strings as "equal apart from OCR noise": one
/// shared-skeleton confusion, or up to three same-glyph ones.
///
/// Deliberately absolute rather than length-scaled. These are *equality*
/// relaxations feeding an exact-match verdict, so the budget must not grow with
/// the string — a long keyword should not become progressively easier to match
/// exactly. Genuinely loose matching is the fuzzy stage's job.
///
/// Set a hair above `COST_SHARED_SKELETON` rather than equal to it, because
/// `3.0 * COST_SAME_GLYPH` is `0.30000000000000004` in binary floating point: an
/// exact `0.3` would reject the three-cheap-swaps case this budget is documented
/// to allow. The slack is numerical headroom only — it admits no additional class
/// of error, which the compile-time assertions below pin.
pub const NOISE_TOLERANCE: f64 = 0.35;

// The budget's documented meaning, enforced at compile time so retuning any of
// these constants is a deliberate decision rather than a silent behavior change.
const _: () = assert!(COST_SHARED_SKELETON <= NOISE_TOLERANCE);
const _: () = assert!(3.0 * COST_SAME_GLYPH <= NOISE_TOLERANCE);
// And nothing beyond that: not four cheap swaps, not two skeleton errors, and
// never a smeared or wholly unrelated character.
const _: () = assert!(4.0 * COST_SAME_GLYPH > NOISE_TOLERANCE);
const _: () = assert!(2.0 * COST_SHARED_SKELETON > NOISE_TOLERANCE);
const _: () = assert!(COST_SMEAR_ONLY > NOISE_TOLERANCE);
const _: () = assert!(COST_UNRELATED > NOISE_TOLERANCE);

/// Class representative for glyphs that print identically (the
/// [`COST_SAME_GLYPH`] tier), or `ch` unchanged when it has no twin.
///
/// Only the same-glyph tier is collapsed, because only it is a true equivalence
/// relation — `1`/`I`/`L` are mutually interchangeable. The skeleton tier is
/// **not**: `M`/`H`, `H`/`N` and `M`/`W` are each plausible, but folding them into
/// one class would make `W` equal `N`, which no OCR engine would do. Graded
/// [`confusion_cost`] handles those instead.
pub fn same_glyph_canonical(ch: char) -> char {
    match ch {
        '0' | 'O' => 'O',
        '1' | 'I' | 'L' => 'I',
        '5' | 'S' => 'S',
        '8' | 'B' => 'B',
        '2' | 'Z' => 'Z',
        '6' | 'G' => 'G',
        other => other,
    }
}

/// Map every char through [`same_glyph_canonical`], so strings differing only by
/// interchangeable glyphs compare equal.
pub fn canonicalize_same_glyph(text: &str) -> String {
    text.chars().map(same_glyph_canonical).collect()
}

/// Cheapest confusion cost of aligning all of `needle` against *any* substring of
/// `haystack`, with the matching window's start offset.
///
/// Substring semantics: skipping haystack before and after the window is free,
/// while every edit inside it is priced by [`confusion_cost`]. This is the
/// approximate counterpart of `str::find` — `(0.0, pos)` means a literal hit.
///
/// Returns `(f64::INFINITY, 0)` when `needle` is longer than `haystack`, since no
/// window can contain it.
pub fn min_substring_cost(needle: &[char], haystack: &[char]) -> (f64, usize) {
    if needle.is_empty() {
        return (0.0, 0);
    }
    if needle.len() > haystack.len() {
        return (f64::INFINITY, 0);
    }
    // `prev[j]` = cost of matching needle[..i] ending at haystack[j], carrying the
    // window start that achieved it. Row 0 is all zeros: a window may open at any
    // offset for free.
    let mut prev: Vec<(f64, usize)> = (0..=haystack.len()).map(|j| (0.0, j)).collect();
    let mut curr: Vec<(f64, usize)> = vec![(0.0, 0); haystack.len() + 1];
    for i in 1..=needle.len() {
        // Deleting needle chars with no haystack left costs a full edit each.
        curr[0] = (i as f64, 0);
        for j in 1..=haystack.len() {
            let (sub_cost, sub_start) = prev[j - 1];
            let substitute = (
                sub_cost + confusion_cost(needle[i - 1], haystack[j - 1]),
                sub_start,
            );
            let (del_cost, del_start) = prev[j];
            let delete = (del_cost + 1.0, del_start);
            let (ins_cost, ins_start) = curr[j - 1];
            let insert = (ins_cost + 1.0, ins_start);
            curr[j] = [substitute, delete, insert]
                .into_iter()
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .expect("three candidates");
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev.iter()
        .skip(1)
        .copied()
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .unwrap_or((f64::INFINITY, 0))
}

/// Normalized similarity in `[0, 1]`: `1 - distance / longer_length`. Dividing by
/// the longer input is what keeps a given score comparable across lengths, so
/// callers can use one absolute threshold.
pub fn similarity(a: &str, b: &str) -> f64 {
    let ca: Vec<char> = a.chars().collect();
    let cb: Vec<char> = b.chars().collect();
    let longer = ca.len().max(cb.len());
    if longer == 0 {
        return 0.0;
    }
    1.0 - weighted_distance(&ca, &cb) / longer as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dist(a: &str, b: &str) -> f64 {
        weighted_distance(
            &a.chars().collect::<Vec<_>>(),
            &b.chars().collect::<Vec<_>>(),
        )
    }

    #[test]
    fn identical_chars_are_free_and_unrelated_chars_cost_a_full_edit() {
        assert_eq!(confusion_cost('A', 'A'), 0.0);
        assert_eq!(confusion_cost('A', 'Z'), COST_UNRELATED);
        assert_eq!(confusion_cost('M', 'Z'), COST_UNRELATED);
    }

    #[test]
    fn cost_is_symmetric() {
        for (a, b) in [('0', 'O'), ('H', 'M'), ('K', 'X'), ('A', 'Q')] {
            assert_eq!(confusion_cost(a, b), confusion_cost(b, a), "{a} vs {b}");
        }
    }

    #[test]
    fn no_substitution_ever_exceeds_an_insert_or_delete() {
        // The ceiling is what lets `similarity` keep one meaningful threshold:
        // the metric degrades to plain Levenshtein for unrelated pairs.
        for a in ('A'..='Z').chain('0'..='9') {
            for b in ('A'..='Z').chain('0'..='9') {
                assert!(confusion_cost(a, b) <= COST_UNRELATED, "{a} vs {b}");
            }
        }
    }

    #[test]
    fn tiers_are_strictly_ordered_by_visual_similarity() {
        let same_glyph = confusion_cost('0', 'O');
        let skeleton = confusion_cost('H', 'M');
        let smear = confusion_cost('K', 'X');
        assert!(0.0 < same_glyph);
        assert!(same_glyph < skeleton);
        assert!(skeleton < smear);
        assert!(smear < COST_UNRELATED);
    }

    #[test]
    fn digit_letter_swaps_cost_far_less_than_a_real_letter_error() {
        // "C0STC0" is what OCR does to "COSTCO"; "CQSTCA" is not.
        assert!(dist("C0STC0", "COSTCO") < dist("CASTCA", "COSTCO"));
        assert_eq!(dist("C0STC0", "COSTCO"), 2.0 * COST_SAME_GLYPH);
    }

    #[test]
    fn several_free_swaps_stay_cheaper_than_one_genuine_error() {
        // The case a binary allow-k-substitutions rule gets backwards: two
        // near-certain glyph swaps are better evidence than one wrong letter.
        // `k = 1` would reject the former and accept the latter.
        let two_trivial = dist("10B", "IOB"); // 1/I and 0/O
        let one_wrong = dist("AOB", "IOB"); // A/I, unrelated
        assert_eq!(two_trivial, 2.0 * COST_SAME_GLYPH);
        assert_eq!(one_wrong, COST_UNRELATED);
        assert!(two_trivial < one_wrong);
    }

    #[test]
    fn watermelon_ocr_error_is_cheap() {
        // The founding case: a No Frills receipt printed "WMELON RED SDLS" and
        // OCR returned "WHELON" — an M read as H, one shared-skeleton swap.
        assert_eq!(dist("WHELON", "WMELON"), COST_SHARED_SKELETON);
        assert!(similarity("WHELON", "WMELON") > 0.9);
    }

    #[test]
    fn similarity_is_bounded_and_length_normalized() {
        assert_eq!(similarity("", ""), 0.0);
        assert_eq!(similarity("COSTCO", "COSTCO"), 1.0);
        assert!(similarity("COSTCO", "ZZZZZZ") <= 0.0);
        // Same single unrelated substitution costs proportionally less on a
        // longer string, which is what makes one absolute threshold workable.
        assert!(similarity("COSTCOXX", "COSTCOXZ") > similarity("CAT", "CAZ"));
    }

    #[test]
    fn canonicalization_agrees_with_the_same_glyph_tier() {
        // The invariant that stops the collapse table and the cost table from
        // drifting apart, which is exactly how the old ad-hoc `0|D -> O` table
        // ended up disagreeing with the graded costs.
        for a in ('A'..='Z').chain('0'..='9') {
            for b in ('A'..='Z').chain('0'..='9') {
                let collapses_together = same_glyph_canonical(a) == same_glyph_canonical(b);
                let free_or_same_glyph = confusion_cost(a, b) <= COST_SAME_GLYPH;
                assert_eq!(
                    collapses_together,
                    free_or_same_glyph,
                    "{a} vs {b}: collapse={collapses_together} cost={}",
                    confusion_cost(a, b)
                );
            }
        }
    }

    #[test]
    fn canonicalization_is_idempotent() {
        let once = canonicalize_same_glyph("LYS0L 1KG 8OX");
        assert_eq!(canonicalize_same_glyph(&once), once);
    }

    #[test]
    fn skeleton_tier_is_deliberately_not_collapsed() {
        // Folding the 0.3 tier into classes would make W equal N via M.
        assert_ne!(same_glyph_canonical('W'), same_glyph_canonical('N'));
        assert_ne!(same_glyph_canonical('D'), same_glyph_canonical('O'));
    }

    fn substring_cost(needle: &str, haystack: &str) -> f64 {
        min_substring_cost(
            &needle.chars().collect::<Vec<_>>(),
            &haystack.chars().collect::<Vec<_>>(),
        )
        .0
    }

    #[test]
    fn substring_search_is_free_outside_the_window() {
        let (cost, pos) = min_substring_cost(
            &"LYSOL".chars().collect::<Vec<_>>(),
            &"BATHLYSOLCLEANER".chars().collect::<Vec<_>>(),
        );
        assert_eq!(cost, 0.0);
        assert_eq!(pos, 4);
    }

    #[test]
    fn substring_search_prices_the_documented_lysol_misreads() {
        // The two regressions the old hard-collapse existed to protect.
        assert_eq!(substring_cost("LYSOL", "LYS0L BATH P 059"), COST_SAME_GLYPH);
        assert_eq!(
            substring_cost("LYSOL", "LYSDL BATH P 059"),
            COST_SHARED_SKELETON
        );
        // Both must land inside the equality budget, or categorization regresses.
        assert!(substring_cost("LYSOL", "LYS0L BATH P 059") <= NOISE_TOLERANCE);
        assert!(substring_cost("LYSDL", "LYSOL BATH P 059") <= NOISE_TOLERANCE);
    }

    #[test]
    fn substring_search_rejects_a_genuinely_different_word() {
        assert!(substring_cost("LYSOL", "PEPSI BATH P 059") > NOISE_TOLERANCE);
        assert!(substring_cost("CHICKEN", "CHOCOLATE BAR") > NOISE_TOLERANCE);
    }

    #[test]
    fn substring_search_handles_degenerate_inputs() {
        assert_eq!(substring_cost("", "ANYTHING"), 0.0);
        assert!(substring_cost("TOOLONG", "SHORT").is_infinite());
        assert_eq!(substring_cost("EXACT", "EXACT"), 0.0);
    }

    #[test]
    fn insert_and_delete_still_cost_a_full_edit() {
        // The documented HIGH_SIMILARITY calibration in `merchant_match` rests on
        // a dropped character ("OSTCO" vs "COSTCO"), so grading substitutions
        // must not have made deletions cheaper.
        assert_eq!(dist("OSTCO", "COSTCO"), 1.0);
        assert!((similarity("OSTCO", "COSTCO") - 0.8333).abs() < 0.001);
    }
}
