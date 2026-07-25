//! Merchant-scoped abbreviation vocabulary.
//!
//! Chains print item names into a fixed-width column and each has its own house
//! style for the squeeze — Costco drops vowels to a consonant skeleton
//! (`KS LIQ LNDRY`), FreshCo does it to brand names (`Mnstr`), Foody Mart
//! truncates. The classifier's keywords are spelled out in full, so those lines
//! either miss entirely or, worse, get captured by whatever fuzzy match happens
//! to survive the mangling.
//!
//! This module expands the abbreviations back before classification, using
//! **exact whole-token substitution** scoped to the resolved merchant. It is
//! explicitly *not* a per-merchant fuzzy tolerance: see
//! `rules/default_merchant_vocab.toml` for the measurements showing why a
//! looser threshold cannot work here (the required operating point sits *below*
//! the score of known false positives).
//!
//! Two properties make this safe where leniency isn't:
//!
//! - **Whole-token equality.** `KS` expands in `KS LIQ LNDRY` but never inside
//!   `KSOMETHING`, so the substring hijacking that produces `PEAR` in "Pearl"
//!   has no analogue here.
//! - **Fails closed.** An unknown abbreviation doesn't expand, so the item
//!   degrades to "no category" rather than a confident wrong one. A receipt
//!   whose merchant doesn't resolve gets no expansions at all.

use std::collections::HashMap;

/// One expansion of one abbreviation.
#[derive(Clone, Debug)]
pub struct Expansion {
    /// The spelled-out form, in the casing it should display with.
    pub full: String,
    /// Whether this expansion may feed the item classifier.
    ///
    /// `false` for brand names and flavour words, which carry no category
    /// signal but plenty of collision risk. Both failures here were measured
    /// against the corpus, not imagined:
    ///
    /// - `KS -> Kirkland Signature` makes the text score 0.70 against the
    ///   *Pharmacy* keyword `KIRKLAND IBU` — exactly the long-keyword bar — so
    ///   Kirkland milk filed as pharmacy.
    /// - `ORNGE -> Orange` turns Grand Marnier liqueur into `Grocery:Fruit`.
    ///
    /// These still expand for **display**, so the recovered name stays complete;
    /// they are simply withheld from the string the classifier sees.
    pub classify: bool,
}

/// One merchant's abbreviation table.
#[derive(Clone, Debug)]
pub struct MerchantVocab {
    /// Canonical merchant family name, as resolved by
    /// [`crate::merchant_match`] — not the raw OCR text.
    pub canonical: String,
    /// Abbreviation -> expansion, keyed by the uppercased abbreviation.
    pub expansions: HashMap<String, Expansion>,
}

/// Look up the vocabulary for a resolved canonical merchant.
///
/// Matching is case-insensitive on the canonical name. Returns `None` when the
/// merchant has no table, which is the common case and a no-op downstream.
pub fn for_merchant<'a>(
    canonical: &str,
    vocabularies: &'a [MerchantVocab],
) -> Option<&'a MerchantVocab> {
    let needle = canonical.trim().to_ascii_uppercase();
    vocabularies
        .iter()
        .find(|vocab| vocab.canonical.to_ascii_uppercase() == needle)
}

/// Whether a character participates in a word token. Item numbers (`1845613`)
/// and sizes (`4L`) are alphanumeric, so digits count; everything else —
/// spaces, slashes, punctuation — is a separator.
fn is_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
}

/// Expand `description`'s abbreviations using `vocab`, returning `None` when
/// nothing matched.
///
/// Substitution is whole-token and case-insensitive; the expansion's own casing
/// is taken from the table, so `KS LIQ LNDRY` becomes
/// `Kirkland Signature Liquid Laundry`. Separators between tokens are preserved
/// verbatim, so item numbers and sizes survive untouched.
pub fn expand(description: &str, vocab: &MerchantVocab) -> Option<String> {
    expand_inner(description, vocab, false)
}

/// As [`expand`], but applying only the entries marked `classify = true`.
///
/// Brand and flavour expansions are withheld: they add no category signal and
/// measurably create false positives. See [`Expansion::classify`].
pub fn expand_for_classification(description: &str, vocab: &MerchantVocab) -> Option<String> {
    expand_inner(description, vocab, true)
}

fn expand_inner(description: &str, vocab: &MerchantVocab, classifying: bool) -> Option<String> {
    let mut out = String::with_capacity(description.len());
    let mut expanded_any = false;
    let mut token = String::new();

    // Flush the pending token, substituting it when the table has an entry.
    fn flush(
        token: &mut String,
        out: &mut String,
        vocab: &MerchantVocab,
        hit: &mut bool,
        classifying: bool,
    ) {
        if token.is_empty() {
            return;
        }
        match vocab.expansions.get(&token.to_ascii_uppercase()) {
            Some(expansion) if !classifying || expansion.classify => {
                out.push_str(&expansion.full);
                *hit = true;
            }
            _ => out.push_str(token),
        }
        token.clear();
    }

    for ch in description.chars() {
        if is_token_char(ch) {
            token.push(ch);
        } else {
            flush(&mut token, &mut out, vocab, &mut expanded_any, classifying);
            out.push(ch);
        }
    }
    flush(&mut token, &mut out, vocab, &mut expanded_any, classifying);

    expanded_any.then_some(out)
}

/// The part of `expanded` worth showing beside `printed`, with the leading
/// tokens the two already share removed.
///
/// Costco prefixes every line with an item number, so a naive append reads
/// `1845613 KS LIQ LNDRY (1845613 Kirkland Signature Liquid Laundry)`. Dropping
/// the shared prefix leaves `(Kirkland Signature Liquid Laundry)` — the part the
/// reader could not already see.
///
/// Returns `None` when nothing distinct remains.
pub fn recovered_tail(printed: &str, expanded: &str) -> Option<String> {
    let printed_tokens: Vec<&str> = printed.split_whitespace().collect();
    let expanded_tokens: Vec<&str> = expanded.split_whitespace().collect();
    let shared = printed_tokens
        .iter()
        .zip(expanded_tokens.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let tail = expanded_tokens[shared..].join(" ");
    (!tail.is_empty()).then_some(tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn costco() -> MerchantVocab {
        MerchantVocab {
            canonical: "COSTCO".to_string(),
            expansions: [
                ("KS", "Kirkland Signature", false),
                ("LIQ", "Liquid", true),
                ("LNDRY", "Laundry", true),
            ]
            .into_iter()
            .map(|(k, v, classify)| {
                (
                    k.to_string(),
                    Expansion {
                        full: v.to_string(),
                        classify,
                    },
                )
            })
            .collect(),
        }
    }

    #[test]
    fn expands_whole_tokens_and_preserves_separators() {
        let vocab = costco();
        assert_eq!(
            expand("1845613 KS LIQ LNDRY", &vocab).as_deref(),
            Some("1845613 Kirkland Signature Liquid Laundry")
        );
    }

    #[test]
    fn expansion_is_case_insensitive_on_the_abbreviation() {
        let vocab = costco();
        assert_eq!(
            expand("ks liq lndry", &vocab).as_deref(),
            Some("Kirkland Signature Liquid Laundry")
        );
    }

    /// The whole point: an abbreviation must never expand as a substring of a
    /// longer token. This is the failure mode that gives `PEAR` inside "Pearl"
    /// and `RADISH` inside "Paradise"; token equality forecloses it.
    #[test]
    fn does_not_expand_inside_a_longer_token() {
        let vocab = costco();
        assert_eq!(expand("KSOMETHING KSX", &vocab), None);
        assert_eq!(
            expand("SKS KS", &vocab).as_deref(),
            Some("SKS Kirkland Signature")
        );
    }

    #[test]
    fn returns_none_when_nothing_matches() {
        assert_eq!(expand("232952 COKE ZERO", &costco()), None);
    }

    /// Separators that are not spaces (Costco prints `KS CRG/ 2% 4L`) must not
    /// be swallowed, or the price/size tail would be corrupted.
    #[test]
    fn preserves_non_space_separators() {
        let vocab = costco();
        assert_eq!(
            expand("KS/LIQ 4L", &vocab).as_deref(),
            Some("Kirkland Signature/Liquid 4L")
        );
    }

    #[test]
    fn recovered_tail_drops_the_shared_item_number_prefix() {
        assert_eq!(
            recovered_tail(
                "1845613 KS LIQ LNDRY",
                "1845613 Kirkland Signature Liquid Laundry"
            )
            .as_deref(),
            Some("Kirkland Signature Liquid Laundry")
        );
    }

    #[test]
    fn recovered_tail_keeps_a_trailing_size_that_follows_an_expansion() {
        // Only the *leading* shared run is dropped; "4L" sits after an expanded
        // token, so it stays and the reading remains complete.
        assert_eq!(
            recovered_tail("458 KS 2% 4L", "458 Kirkland Signature 2% 4L").as_deref(),
            Some("Kirkland Signature 2% 4L")
        );
    }

    #[test]
    fn recovered_tail_is_none_when_nothing_distinct_remains() {
        assert_eq!(recovered_tail("458 MILK 2", "458 MILK 2"), None);
    }

    #[test]
    fn merchant_lookup_is_case_insensitive_and_fails_closed() {
        let all = vec![costco()];
        assert!(for_merchant("costco", &all).is_some());
        assert!(for_merchant("COSTCO", &all).is_some());
        assert!(for_merchant("FRESHCO", &all).is_none());
        assert!(for_merchant("", &all).is_none());
    }
}
