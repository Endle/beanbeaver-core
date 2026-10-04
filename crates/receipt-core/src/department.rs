//! Loblaw-family department banners as a last-resort category.
//!
//! No Frills, Loblaws and Real Canadian Superstore group the item block under
//! numbered department banners — `21-GROCERY`, `23-FROZEN`, `31-MEATS` — with
//! one shared numbering across the chain. Both extractors already recognise
//! these rows as section headers and skip them; this module keeps what they
//! say. An item whose own text no rule recognises (`PCBL BLACK LABE`, a
//! private-label ice cream) still sits under `23-FROZEN`, and that is evidence
//! about what it is.
//!
//! Only departments whose contents share one account are mapped. Measured on
//! the private corpus (2026-10-04), the excluded ones would misfile:
//!
//! | Dept | Why it is not mapped |
//! |---|---|
//! | `21-GROCERY` | drinks, oil, snacks, canned vegetables — no single account |
//! | `22-DAIRY` | the refrigerated case: its uncategorized lines were two juices and eggs |
//! | `25-NATURAL FOODS` | any category, organic |
//! | `27-PRODUCE` | fruit vs vegetable, roughly 3:1 |
//! | `42-ENTERTAINMENT` | `entertainment` has no account, so the line stays FIXME anyway |
//!
//! A banner's number and name must **both** agree with the table. The number is
//! what scopes this to the Loblaw numbering — another chain's `03-Meat` is not
//! department 31 — and it is also what lets the name tolerate one OCR edit
//! (`31-NEATS`, `41-HONE`) without a bare four-letter word doing the matching.

use regex::Regex;
use std::sync::OnceLock;

/// `(number, printed name, tag path)`. The tag path must carry an account in
/// `default_tags.toml`, or the fallback would tag a line and still post it to
/// FIXME.
const DEPARTMENTS: &[(u8, &str, &str)] = &[
    (23, "FROZEN", "grocery/frozen"),
    (26, "LIQUOR", "alcohol/beverage"),
    (31, "MEATS", "grocery/meat"),
    (32, "SEAFOOD", "grocery/seafood"),
    (33, "BAKERY", "grocery/bakery"),
    (41, "HOME", "household/supply"),
];

/// `NN-NAME`, then nothing numeric: a banner never carries a price, and OCR
/// sometimes glues background noise after the name (`22-DAIRY pilggo`).
fn re_banner() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*(\d{2})\s*-\s*([A-Za-z]+)\b[^0-9]*$").unwrap())
}

/// What a department banner row says, if `text` is one.
///
/// `Some(None)` is a banner for an unmapped department: it still ends the
/// department above it, so the items under `22-DAIRY` do not inherit the
/// `23-FROZEN` that may precede it in print order.
pub(crate) fn banner(text: &str) -> Option<Option<&'static str>> {
    let caps = re_banner().captures(text.trim())?;
    let number: u8 = caps[1].parse().ok()?;
    let name: Vec<char> = caps[2].to_ascii_uppercase().chars().collect();
    Some(
        DEPARTMENTS
            .iter()
            .find(|(n, printed, _)| {
                *n == number && {
                    let printed: Vec<char> = printed.chars().collect();
                    crate::ocr_confusion::weighted_distance(&name, &printed) <= 1.0
                }
            })
            .map(|(_, _, tag_path)| *tag_path),
    )
}

/// The mapped department in force at each row, in print order.
///
/// A banner sets it for every row until the next banner. Rows above the first
/// banner, and rows under an unmapped one, get `None`.
pub(crate) fn in_effect<'a>(rows: impl IntoIterator<Item = &'a str>) -> Vec<Option<&'static str>> {
    let mut current = None;
    rows.into_iter()
        .map(|row| {
            if let Some(department) = banner(row) {
                current = department;
            }
            current
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{banner, in_effect};

    #[test]
    fn mapped_banners_name_their_tag() {
        assert_eq!(banner("23-FROZEN"), Some(Some("grocery/frozen")));
        assert_eq!(banner("33-BAKERY INSTORE"), Some(Some("grocery/bakery")));
        assert_eq!(banner(" 32 - SEAFOOD"), Some(Some("grocery/seafood")));
    }

    #[test]
    fn one_ocr_edit_is_tolerated_because_the_number_agrees() {
        assert_eq!(banner("31-NEATS"), Some(Some("grocery/meat")));
        assert_eq!(banner("41-HONE"), Some(Some("household/supply")));
    }

    #[test]
    fn unmapped_banners_are_banners_with_no_tag() {
        assert_eq!(banner("21-GROCERY"), Some(None));
        // Background noise glued after the name is still a banner.
        assert_eq!(banner("22-DAIRY pilggo"), Some(None));
        assert_eq!(banner("27-PRODUCE"), Some(None));
    }

    #[test]
    fn the_number_scopes_the_table_to_the_loblaw_numbering() {
        // Right name, another chain's number.
        assert_eq!(banner("03-FROZEN"), Some(None));
        // Right number, a different department.
        assert_eq!(banner("23-PRODUCE"), Some(None));
    }

    #[test]
    fn priced_or_numbered_rows_are_not_banners() {
        assert_eq!(banner("12-PACK COKE 7.99"), None);
        assert_eq!(banner("06038306830 PCBL BLACK LABE MRJ 4.50"), None);
        assert_eq!(banner("FROZEN"), None);
    }

    #[test]
    fn a_banner_holds_until_the_next_one() {
        let rows = [
            "NOFRILLS",
            "23-FROZEN",
            "06038306830 PCBL BLACK LABE MRJ 4.50",
            "22-DAIRY",
            "06038360413 NN EXT LRG, EA MRJ 4.71",
        ];
        assert_eq!(
            in_effect(rows),
            vec![
                None,
                Some("grocery/frozen"),
                Some("grocery/frozen"),
                None,
                None
            ]
        );
    }
}
