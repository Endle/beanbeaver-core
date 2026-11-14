//! A product rung up twice, with one copy damaged.
//!
//! Asian-grocery tills print one line per unit, so the same product often
//! appears two or three times on one receipt — and OCR reads each copy
//! independently. When one copy's damage lands on the very letters a keyword
//! needs, that copy books to FIXME while its twin a few rows away is
//! recognised. Foody Mart 2026-11-08 $62.48 had a pen stroke across two such
//! pairs: `Meat Corner - AA Bedf Pla` beside `Meat Corner - AA Beef/Pla`, and
//! `Sunrise - Fresh Shooth To` beside `Sunrise - Fresh Smooth To`.
//!
//! The clean copy is evidence about the damaged one, and about exactly the
//! letters that differ: if the damaged copy disagreed only somewhere the
//! keyword does not look, the keyword would have matched it too. So the repair
//! copies the clean copy's letters into the damaged line **at the positions
//! where they disagree, and nowhere else** — the damaged line keeps its own
//! punctuation and spacing, which is why `Bedf Pla` comes back as the printed
//! `Beef Pla` and not as its twin's `Beef/Pla`.
//!
//! Comparison is over letters and digits only, uppercased, and only between
//! copies with the same number of them: a substitution is a misread glyph,
//! while an inserted or dropped letter is as likely a different product
//! (`Pla` against `Rol`) as a damaged one. Costs come from the receipt's one
//! OCR error model, [`crate::ocr_confusion`], so a faded `M` read as `H` is
//! cheap and an unrelated letter costs a full edit.

use crate::ocr_confusion::confusion_cost;

/// The most a repair may cost: one unrelated letter, or a few cheap glyph
/// confusions.
///
/// Swept over the private corpus (205 receipts, 2026-11-13) with
/// [`MAX_REPAIRED_SHARE`] in force, counting the lines whose text and category
/// the stage changes; every one was checked against what its receipt prints:
///
/// | budget | cached | live | wrong |
/// |---|---|---|---|
/// | 0.35 (`NOISE_TOLERANCE`) | 1 | 1 | 0 |
/// | **1.0** | **4** | **6** | **0** |
/// | 2.0 | 4 | 6 | 0 |
/// | 3.0 | 4 | 6 | 0 |
///
/// Each is a product printed twice with one glyph of one copy misread:
/// `Shooth`/`Smooth` (the 0.35 row — `H`/`M` is a skeleton confusion),
/// `Bedf`/`Beef`, `Meet`/`Meat`, `Origunal`/`Original`, and live only
/// `Swallop`/`Scallop` and `XL EOGS`/`XL EGGS`. Nothing sits between one edit
/// and three, even with the share cap lifted and inserted or dropped letters
/// allowed. The budget is set at the knee: a looser one buys nothing
/// measurable and admits genuinely different products sooner.
pub(crate) const REPAIR_BUDGET: f64 = 1.0;

/// The largest share of a line's letters a repair may change.
///
/// A prior, not a measurement: the corpus never comes near it except at `Meet`
/// against `Meat` (one in four, exactly the cap). What it guards against is a
/// short word one letter from a different short word — `Ham` and `Jam` — where
/// a single edit is a third of the evidence.
pub(crate) const MAX_REPAIRED_SHARE: f64 = 0.25;

/// `(char index, printed char, comparison key)` for each letter or digit.
fn letters(text: &str) -> Vec<(usize, char, char)> {
    text.chars()
        .enumerate()
        .filter(|(_, ch)| ch.is_alphanumeric())
        .map(|(index, ch)| (index, ch, ch.to_ascii_uppercase()))
        .collect()
}

/// `damaged` with `clean`'s letters copied in where the two disagree, and what
/// the disagreement cost.
///
/// `None` when they do not have the same number of letters, when they already
/// agree on every letter, or when the disagreement is more than a misread
/// glyph can explain (see [`REPAIR_BUDGET`] and [`MAX_REPAIRED_SHARE`]).
pub(crate) fn repair(damaged: &str, clean: &str) -> Option<(String, f64)> {
    let ours = letters(damaged);
    let theirs = letters(clean);
    if ours.is_empty() || ours.len() != theirs.len() {
        return None;
    }
    let mut cost = 0.0;
    let mut fixes = Vec::new();
    for (&(index, _, key), &(_, printed, clean_key)) in ours.iter().zip(&theirs) {
        if key != clean_key {
            cost += confusion_cost(key, clean_key);
            fixes.push((index, printed));
        }
    }
    if fixes.is_empty() || cost > REPAIR_BUDGET || cost > MAX_REPAIRED_SHARE * ours.len() as f64 {
        return None;
    }
    let mut chars: Vec<char> = damaged.chars().collect();
    for (index, printed) in fixes {
        chars[index] = printed;
    }
    Some((chars.into_iter().collect(), cost))
}

#[cfg(test)]
mod tests {
    use super::repair;

    fn repaired(damaged: &str, clean: &str) -> Option<String> {
        repair(damaged, clean).map(|(text, _)| text)
    }

    #[test]
    fn copies_only_the_letters_that_disagree() {
        // The twin's own damage (`/`) is not a letter, so it stays behind.
        assert_eq!(
            repaired("Meat Corner - AA Bedf Pla", "Meat Corner - AA Beef/Pla").as_deref(),
            Some("Meat Corner - AA Beef Pla")
        );
        assert_eq!(
            repaired("Sunrise - Fresh Shooth To", "Sunrise - Fresh Smooth To").as_deref(),
            Some("Sunrise - Fresh Smooth To")
        );
    }

    #[test]
    fn a_glyph_confusion_costs_less_than_an_unrelated_letter() {
        let (_, faded_m) = repair("Fresh Shooth", "Fresh Smooth").unwrap();
        let (_, blot) = repair("AA Bedf", "AA Beef").unwrap();
        assert!(faded_m < blot, "{faded_m} vs {blot}");
    }

    #[test]
    fn a_different_letter_count_is_a_different_product() {
        assert_eq!(repaired("AA Beef Pla", "AA Beef Plat"), None);
        assert_eq!(repaired("Meat Corner Beef", "Meat Corner Beef Rol"), None);
    }

    #[test]
    fn more_than_one_misread_glyph_is_not_a_repair() {
        assert_eq!(repaired("AA Bxxf Pla", "AA Beef Pla"), None);
    }

    #[test]
    fn short_words_need_proportionally_more_agreement() {
        // One letter in four is the most a repair may change...
        assert_eq!(repaired("Meet", "Meat").as_deref(), Some("Meat"));
        // ...so one in three is a different word, not a damaged one.
        assert_eq!(repaired("Jam", "Ham"), None);
    }

    #[test]
    fn identical_letters_need_no_repair() {
        assert_eq!(repaired("AA Beef/Pla", "AA Beef Pla"), None);
    }
}
