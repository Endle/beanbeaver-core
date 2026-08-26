//! Unit tests for spatial item extraction.
//!
//! Word bounding boxes below are real OCR-derived, normalized coordinates; some
//! (e.g. 0.318) land near a math constant (1/π) purely by coincidence.
#![allow(clippy::approx_constant)]

use super::engine::{extract_spatial_items, footer_address_like, is_price_word};
use crate::money::Money;
use crate::ocr_document::{Bbox, OcrDocument, OcrLine, OcrWord};

#[test]
fn address_veto_spares_item_rows_that_own_a_price() {
    // The pattern alternates over bare tokens, so ordinary items trip it:
    // No Frills' "ON THE GO BOTTLE" (ON) and "TNS RD HT BF BRR" (RD), plus
    // T&T's "YIN ON SWEETENED SOYA DRINK". A priced row is an item by
    // construction, so the veto must not apply to it.
    for priced in [
        "03760401787 ON THE GO BOTTLE HMRJ 5.00",
        "07960651131 TNS RD HT BF BRR MRJ 1.69",
        "YIN ON SWEETENED SOYA DRINK 4.47",
        "DR PEPPER 2.99",
    ] {
        assert!(
            !footer_address_like(priced),
            "priced item row must survive the address veto: {priced:?}"
        );
    }

    // Unpriced header/footer address lines are still vetoed — that is the
    // whole reason the check exists.
    for address in [
        "7075 MARKHAM RD, MARKHAM, ON, L3S 3J9",
        "5762 HWY 7 E, MARKHAM",
        "Scarborough, ON",
        "1 Yorktech Dr",
    ] {
        assert!(
            footer_address_like(address),
            "unpriced address line must still be vetoed: {address:?}"
        );
    }
}

#[test]
fn parses_tt_price_with_gst_pst_tax_flags() {
    // T&T prints GST/PST flags after the price, sometimes several
    // space-separated (e.g. "W $6.87 G P"). Costco's H/T/J must still work.
    assert_eq!(is_price_word("W $6.87 G P"), Some(68_700));
    assert_eq!(is_price_word("W $13.97"), Some(139_700));
    assert_eq!(is_price_word("6.87 G"), Some(68_700));
    assert_eq!(is_price_word("5.00- H"), Some(-50_000));
    // F (food, zero-rated) is a flag too, and T&T pairs it with G on its
    // food-court rows: 2026-08-25_t_t_supermarket_14_48 prints the whole
    // token "W $12.81 G F" as ONE OCR box. Without F in the class the row
    // carries no price at all and the receipt's only item disappears.
    assert_eq!(is_price_word("W $12.81 G F"), Some(128_100));
    assert_eq!(is_price_word("$12.81 F"), Some(128_100));
}

fn word(text: &str, left: f64, top: f64, right: f64, bottom: f64) -> OcrWord {
    OcrWord {
        text: text.to_string(),
        bbox: Bbox {
            left,
            top,
            right,
            bottom,
        },
        confidence: 0.99,
    }
}

#[test]
fn keeps_short_produce_name_alignment() {
    let page = OcrDocument {
        lines: vec![
            OcrLine::new(
                "&& 02-Vegetable".to_string(),
                vec![word("&& 02-Vegetable", 0.15, 0.355, 0.30, 0.364)],
            ),
            OcrLine::new(
                "Napa".to_string(),
                vec![word("Napa", 0.06, 0.365, 0.09, 0.372)],
            ),
            OcrLine::new(
                "2.46 1b @ $1.29/1b 3.17".to_string(),
                vec![
                    word("2.46 1b @ $1.29/1b", 0.20, 0.378, 0.27, 0.386),
                    word("3.17", 0.89, 0.377, 0.92, 0.384),
                ],
            ),
            OcrLine::new(
                "Soybean Sprout".to_string(),
                vec![word("Soybean Sprout", 0.12, 0.388, 0.24, 0.395)],
            ),
            OcrLine::new(
                "0.65 1b @ $1.58/1b 1.03".to_string(),
                vec![
                    word("0.65 1b @ $1.58/1b", 0.21, 0.401, 0.28, 0.409),
                    word("1.03", 0.89, 0.400, 0.92, 0.407),
                ],
            ),
        ],
    };

    let outcome = extract_spatial_items(&page);
    let observed = outcome
        .items
        .into_iter()
        .map(|item| (item.description, item.price))
        .collect::<Vec<_>>();

    assert!(observed.contains(&("Napa".to_string(), Money::from_cents(317))));
    assert!(observed.contains(&("Soybean Sprout".to_string(), Money::from_cents(103))));
}

#[test]
fn keeps_sale_marker_short_produce_name_as_qty_row_target() {
    // T&T 2026-06-29_t_t_supermarket_49_59: "(SALE) NAPA" is 11 chars, so
    // the short-parenthetical stub check rejected it as an item line. Its
    // weight-row price $4.75 then skipped to "(SALE) STRAWBERRY" below,
    // and strawberry's own $5.00 cascaded onto the garbled "TMERE" line.
    let page = OcrDocument {
        lines: vec![
            OcrLine::new(
                "PRODUCE".to_string(),
                vec![word("PRODUCE", 0.05, 0.326, 0.16, 0.337)],
            ),
            OcrLine::new(
                "(SALE) NAPA".to_string(),
                vec![word("(SALE) NAPA", 0.05, 0.336, 0.22, 0.347)],
            ),
            OcrLine::new(
                "2.200 kg @ $2.16/kg W $4.75".to_string(),
                vec![
                    word("2.200 kg @ $2.16/kg", 0.06, 0.345, 0.36, 0.359),
                    word("W $4.75", 0.72, 0.345, 0.83, 0.359),
                ],
            ),
            OcrLine::new(
                "(SALE) STRAWBERRY".to_string(),
                vec![word("(SALE) STRAWBERRY", 0.06, 0.376, 0.31, 0.387)],
            ),
            OcrLine::new(
                "594143 2 @2/$5.00 W $5.00".to_string(),
                vec![
                    word("594143 2 @2/$5.00", 0.06, 0.385, 0.36, 0.398),
                    word("W $5.00", 0.72, 0.385, 0.83, 0.398),
                ],
            ),
            OcrLine::new(
                "DELI".to_string(),
                vec![word("DELI", 0.05, 0.414, 0.13, 0.426)],
            ),
            OcrLine::new(
                "T&T PRESERVED DUCK EGGS W $5.99".to_string(),
                vec![
                    word("T&T PRESERVED DUCK EGGS", 0.06, 0.424, 0.41, 0.436),
                    word("W $5.99", 0.72, 0.424, 0.83, 0.436),
                ],
            ),
            OcrLine::new(
                "TMERE".to_string(),
                vec![word("TMERE", 0.06, 0.434, 0.27, 0.448)],
            ),
        ],
    };

    let outcome = extract_spatial_items(&page);
    let observed = outcome
        .items
        .into_iter()
        .map(|item| (item.description, item.price))
        .collect::<Vec<_>>();

    assert!(
        observed.contains(&("NAPA".to_string(), Money::from_cents(475))),
        "{observed:?}"
    );
    assert!(
        observed.contains(&("STRAWBERRY".to_string(), Money::from_cents(500))),
        "{observed:?}"
    );
    assert!(
        observed.contains(&(
            "T&T PRESERVED DUCK EGGS".to_string(),
            Money::from_cents(599)
        )),
        "{observed:?}"
    );
    assert!(
        !observed.iter().any(|(desc, _)| desc.contains("TMERE")),
        "{observed:?}"
    );
}

#[test]
fn weight_block_prices_reuse_produce_label_and_drifted_qty_price_pairs_nearest() {
    // No Frills 2026-07-02_no_frills_52_16: one "CHERRIES RED" label with
    // four gross/tare/net weighings (each priced), and a banana qty row
    // that absorbed the melon's 5.99 during line grouping (its own math
    // says 1.775 x 1.52 = 2.70).
    fn row(text: &str, y: f64, price: Option<&str>) -> OcrLine {
        let mut words = vec![word(text, 0.06, y, 0.45, y + 0.012)];
        let mut full = text.to_string();
        if let Some(p) = price {
            words.push(word(p, 0.80, y, 0.90, y + 0.012));
            full = format!("{text} {p}");
        }
        OcrLine::new(full, words)
    }
    let page = OcrDocument {
        lines: vec![
            row("4011 BANANA MRJ", 0.300, Some("2.70")),
            row("1.775 kg @ $1.52/kg", 0.317, Some("5.99")),
            row("4032 WMELON RED SOLS MRJ", 0.334, None),
            row("4045 CHERRIES RED MRJ", 0.351, None),
            row("0.985 kg Grosks", 0.368, None),
            row("-0.010 kg Tare =", 0.385, Some("4.28")),
            row("0.975 kg Net @ $4.39/kg", 0.402, None),
            row("1.035 kg Gross", 0.419, None),
            row("-0.010 kg Tare =", 0.436, Some("4.50")),
            row("1.025 kg Net @ $4.39/k9", 0.453, None),
            row("1.105 kg Gross", 0.470, None),
            row("-0.010 kg Tare =", 0.487, Some("4.81")),
            row("1.095 kg Net @ $4.39/k9", 0.504, None),
            row("1.020 kg Gros ed", 0.521, None),
            row("-0.010 kg Tare =", 0.538, None),
            row("1.010 kg Net @ $4.39/kg", 0.555, Some("4.43")),
            row("4403 PEACH YELLOW MRJ", 0.572, None),
            row("0.900 kg @ $2.18/kg", 0.589, Some("1.96")),
        ],
    };

    let outcome = extract_spatial_items(&page);
    let observed = outcome
        .items
        .into_iter()
        .map(|item| (item.description, item.price))
        .collect::<Vec<_>>();

    let has = |needle: &str, price: Money| {
        observed
            .iter()
            .any(|(desc, p)| desc.contains(needle) && *p == price)
    };
    assert!(has("BANANA", Money::from_cents(270)), "{observed:?}");
    assert!(
        has("WMELON RED SOLS", Money::from_cents(599)),
        "{observed:?}"
    );
    for price in [428, 450, 481, 443].map(Money::from_cents) {
        assert!(
            has("CHERRIES RED", price),
            "missing CHERRIES RED {price}: {observed:?}"
        );
    }
    assert!(has("PEACH YELLOW", Money::from_cents(196)), "{observed:?}");
    assert!(
        !observed
            .iter()
            .any(|(desc, _)| desc.contains("Tare") || desc.contains("Gro")),
        "{observed:?}"
    );
}

#[test]
fn keeps_costco_short_code_item_with_percent_suffix() {
    // Costco 2026-05-24_costco_56_42: "458 MILK 2% 6.09". The 3-digit item
    // code "458" plus the "2%" fat suffix dragged the alpha-ratio below 0.5
    // so both MILK lines were dropped, while "1346909 KS ORG 2% 4L" (6-digit
    // code, stripped) survived. Stripping the short code restores them.
    let page = OcrDocument {
        lines: vec![
            OcrLine::new(
                "458 MILK 2% 6.09".to_string(),
                vec![
                    word("458 MILK 2%", 0.30, 0.200, 0.54, 0.212),
                    word("6.09", 0.82, 0.200, 0.90, 0.212),
                ],
            ),
            OcrLine::new(
                "458 MILK 2% 6.09".to_string(),
                vec![
                    word("458 MILK 2%", 0.30, 0.220, 0.54, 0.232),
                    word("6.09", 0.82, 0.220, 0.90, 0.232),
                ],
            ),
            OcrLine::new(
                "1346909 KS ORG 2% 4L 10.29".to_string(),
                vec![
                    word("1346909 KS ORG 2% 4L", 0.30, 0.240, 0.58, 0.252),
                    word("10.29", 0.82, 0.240, 0.90, 0.252),
                ],
            ),
            OcrLine::new(
                "TOTAL 26.47".to_string(),
                vec![
                    word("TOTAL", 0.09, 0.500, 0.18, 0.512),
                    word("26.47", 0.82, 0.500, 0.90, 0.512),
                ],
            ),
        ],
    };

    let outcome = extract_spatial_items(&page);
    let milk_count = outcome
        .items
        .iter()
        .filter(|item| item.price == Money::from_cents(609))
        .count();
    assert_eq!(
        milk_count, 2,
        "both MILK 2% lines expected, got {:?}",
        outcome.items
    );
}

#[test]
fn prefers_item_above_onsale_price() {
    let page = OcrDocument {
        lines: vec![
            OcrLine::new(
                "*S & B Wasabi".to_string(),
                vec![word("*S & B Wasabi", 0.08, 0.100, 0.260, 0.112)],
            ),
            OcrLine::new(
                "(E)ON SALE 1.98".to_string(),
                vec![
                    word("(E)ON SALE", 0.09, 0.120, 0.210, 0.132),
                    word("1.98", 0.88, 0.120, 0.93, 0.132),
                ],
            ),
            OcrLine::new(
                "2 @ $0.99 4.59".to_string(),
                vec![
                    word("2 @ $0.99", 0.22, 0.140, 0.320, 0.152),
                    word("4.59", 0.88, 0.140, 0.93, 0.152),
                ],
            ),
            OcrLine::new(
                "Hot Kid Honey Flavour Bal".to_string(),
                vec![word("Hot Kid Honey Flavour Bal", 0.08, 0.160, 0.360, 0.172)],
            ),
            OcrLine::new(
                "TOTAL 6.57".to_string(),
                vec![
                    word("TOTAL", 0.09, 0.500, 0.180, 0.512),
                    word("6.57", 0.88, 0.500, 0.93, 0.512),
                ],
            ),
        ],
    };

    let outcome = extract_spatial_items(&page);
    let observed = outcome
        .items
        .into_iter()
        .map(|item| (item.description, item.price))
        .collect::<Vec<_>>();

    assert_eq!(
        observed,
        vec![
            ("S & B Wasabi".to_string(), Money::from_cents(198)),
            (
                "Hot Kid Honey Flavour Bal".to_string(),
                Money::from_cents(459)
            ),
        ]
    );
}

#[test]
fn quantity_price_row_with_ea_suffix_uses_item_above() {
    let page = OcrDocument {
        lines: vec![
            OcrLine::new(
                "FF SHEPHERDS PURSE FILLING".to_string(),
                vec![word("FF SHEPHERDS PURSE FILLING", 0.05, 0.700, 0.40, 0.712)],
            ),
            OcrLine::new(
                "2 @ $3.49ea. W $6.98".to_string(),
                vec![
                    word("2 @ $3.49ea.", 0.07, 0.723, 0.23, 0.735),
                    word("W $6.98", 0.88, 0.723, 0.95, 0.735),
                ],
            ),
            OcrLine::new(
                "TOTAL 6.98".to_string(),
                vec![
                    word("TOTAL", 0.10, 0.900, 0.18, 0.912),
                    word("6.98", 0.88, 0.900, 0.93, 0.912),
                ],
            ),
        ],
    };

    let outcome = extract_spatial_items(&page);
    let observed = outcome
        .items
        .into_iter()
        .map(|item| (item.description, item.price))
        .collect::<Vec<_>>();

    assert_eq!(
        observed,
        vec![(
            "FF SHEPHERDS PURSE FILLING".to_string(),
            Money::from_cents(698)
        )]
    );
}

#[test]
fn skips_receipt_metadata_when_quantity_row_needs_item_context() {
    let page = OcrDocument {
        lines: vec![
            OcrLine::new(
                "WS# P6 Cashier6".to_string(),
                vec![word("WS# P6 Cashier6", 0.05, 0.100, 0.22, 0.112)],
            ),
            OcrLine::new(
                "*S & B Wasabi".to_string(),
                vec![word("*S & B Wasabi", 0.08, 0.140, 0.260, 0.152)],
            ),
            OcrLine::new(
                "(E)ON SALE 1.98".to_string(),
                vec![
                    word("(E)ON SALE", 0.09, 0.160, 0.210, 0.172),
                    word("1.98", 0.88, 0.160, 0.93, 0.172),
                ],
            ),
            OcrLine::new(
                "2 @ $0.99 4.59".to_string(),
                vec![
                    word("2 @ $0.99", 0.22, 0.180, 0.320, 0.192),
                    word("4.59", 0.88, 0.180, 0.93, 0.192),
                ],
            ),
            OcrLine::new(
                "Hot Kid Honey Flavour Bal".to_string(),
                vec![word("Hot Kid Honey Flavour Bal", 0.08, 0.200, 0.360, 0.212)],
            ),
            OcrLine::new(
                "TOTAL 6.57".to_string(),
                vec![
                    word("TOTAL", 0.09, 0.500, 0.180, 0.512),
                    word("6.57", 0.88, 0.500, 0.93, 0.512),
                ],
            ),
        ],
    };

    let outcome = extract_spatial_items(&page);
    let observed = outcome
        .items
        .into_iter()
        .map(|item| (item.description, item.price))
        .collect::<Vec<_>>();

    assert_eq!(
        observed,
        vec![
            ("S & B Wasabi".to_string(), Money::from_cents(198)),
            (
                "Hot Kid Honey Flavour Bal".to_string(),
                Money::from_cents(459)
            ),
        ]
    );
}

// --- ported regressions from desktop tests/test_receipt_spatial_parser_regressions.py ---
// Prices are ×10000 fixed-point (3.17 -> 31_700); the desktop asserts on Decimal.
// The Rust spatial extractor takes no rule layers (categorization is a later stage).

fn line(text: &str, words: Vec<OcrWord>) -> OcrLine {
    OcrLine::new(text, words)
}

fn pairs_of(lines: Vec<OcrLine>) -> Vec<(String, Money)> {
    extract_spatial_items(&OcrDocument { lines })
        .items
        .into_iter()
        .map(|item| (item.description, item.price))
        .collect()
}

/// A code-only price row (barcode + deposit price) between two items must not
/// have its price stolen by the following item.
#[test]
fn keeps_next_priced_item_from_stealing_code_only_price_row() {
    let lines = vec![
        line(
            "26-L.IQUOR COORS LIGHT 6 PK HQ 15.79",
            vec![
                word("26-L.IQUOR", 0.031, 0.289, 0.191, 0.309),
                word("COORS LIGHT 6 PK HQ", 0.318, 0.301, 0.688, 0.328),
                word("15.79", 0.760, 0.298, 0.860, 0.324),
            ],
        ),
        line(
            "05632700795 0.60",
            vec![
                word("05632700795", 0.068, 0.309, 0.257, 0.328),
                word("0.60", 0.881, 0.317, 0.963, 0.340),
            ],
        ),
        line(
            "DEPOSIT 1 COORS PINEAPPLE HQ 3.19",
            vec![
                word("DEPOSIT 1", 0.102, 0.328, 0.258, 0.348),
                word("COORS PINEAPPLE", 0.322, 0.342, 0.579, 0.365),
                word("HQ", 0.657, 0.341, 0.701, 0.362),
                word("3.19", 0.793, 0.338, 0.876, 0.361),
            ],
        ),
        line(
            "05632702339 0.10",
            vec![
                word("05632702339", 0.070, 0.347, 0.259, 0.366),
                word("0.10", 0.882, 0.356, 0.963, 0.377),
            ],
        ),
        line(
            "DEPOSIT 1",
            vec![word("DEPOSIT 1", 0.103, 0.366, 0.260, 0.385)],
        ),
        line(
            "TOTAL 19.68",
            vec![
                word("TOTAL", 0.090, 0.500, 0.180, 0.512),
                word("19.68", 0.880, 0.500, 0.950, 0.512),
            ],
        ),
    ];
    let pairs = pairs_of(lines);
    assert!(pairs.contains(&(
        "26-L.IQUOR COORS LIGHT 6 PK HQ".to_string(),
        Money::from_cents(1579)
    )));
    assert!(pairs
        .iter()
        .any(|(d, p)| d.contains("COORS PINEAPPLE") && *p == Money::from_cents(319)));
    assert!(!pairs
        .iter()
        .any(|(d, p)| d.contains("COORS PINEAPPLE") && *p == Money::from_cents(60)));
}

/// A quantity total ("3@$0.10 2.79") must attach to the multi-buy item, not
/// to the intervening "DEPOSIT 1" stub.
#[test]
fn keeps_quantity_total_off_deposit_stub() {
    let lines = vec![
        line(
            "(3)06365703339 GROWERS CIDER HQ 10.47",
            vec![
                word("(3)06365703339", 0.074, 0.385, 0.310, 0.403),
                word("GROWERS CIDER", 0.373, 0.381, 0.597, 0.403),
                word("HQ", 0.657, 0.379, 0.702, 0.400),
                word("10.47", 0.869, 0.393, 0.963, 0.415),
            ],
        ),
        line(
            "3 @ $3.49",
            vec![word("3 @ $3.49", 0.102, 0.403, 0.261, 0.422)],
        ),
        line(
            "DEPOSIT 1 0.30",
            vec![
                word("DEPOSIT 1", 0.100, 0.421, 0.258, 0.439),
                word("0.30", 0.884, 0.430, 0.961, 0.452),
            ],
        ),
        line(
            "3@$0.10 2.79",
            vec![
                word("3@$0.10", 0.098, 0.438, 0.225, 0.457),
                word("2.79", 0.812, 0.451, 0.893, 0.473),
            ],
        ),
        line(
            "06365703620 GROW CIDER HQ 0.10",
            vec![
                word("06365703620", 0.061, 0.457, 0.259, 0.475),
                word("GROW CIDER", 0.322, 0.456, 0.498, 0.476),
                word("HQ", 0.676, 0.454, 0.720, 0.475),
                word("0.10", 0.882, 0.469, 0.964, 0.492),
            ],
        ),
        line(
            "DEPOSIT 1",
            vec![word("DEPOSIT 1", 0.094, 0.475, 0.256, 0.495)],
        ),
        line(
            "TOTAL 13.66",
            vec![
                word("TOTAL", 0.090, 0.500, 0.180, 0.512),
                word("13.66", 0.880, 0.500, 0.950, 0.512),
            ],
        ),
    ];
    let pairs = pairs_of(lines);
    assert!(pairs
        .iter()
        .any(|(d, p)| d.contains("GROW CIDER") && *p == Money::from_cents(279)));
    assert!(!pairs
        .iter()
        .any(|(d, p)| d == "DEPOSIT 1" && *p == Money::from_cents(279)));
}

/// A duplicate code row that repeats the previous item's price must lend that
/// price to the following unpriced item.
#[test]
fn assigns_duplicate_code_row_price_to_next_unpriced_item() {
    let lines = vec![
        line(
            "27-PRODUCE CANTALOUPE MRJ 1.99",
            vec![
                word("27-PRODUCE", 0.017, 0.493, 0.205, 0.514),
                word("CANTALOUPE", 0.318, 0.510, 0.496, 0.529),
                word("MRJ", 0.676, 0.510, 0.738, 0.529),
                word("1.99", 0.817, 0.507, 0.896, 0.528),
            ],
        ),
        line(
            "4050 1.99",
            vec![
                word("4050", 0.055, 0.513, 0.132, 0.532),
                word("1.99", 0.784, 0.525, 0.862, 0.547),
            ],
        ),
        line(
            "81363501124 BLACKBERRIES 60Z MRJ",
            vec![
                word("81363501124", 0.054, 0.531, 0.254, 0.551),
                word("BLACKBERRIES 60Z", 0.319, 0.528, 0.602, 0.549),
                word("MRJ", 0.641, 0.528, 0.704, 0.547),
            ],
        ),
        line(
            "TOTAL 3.98",
            vec![
                word("TOTAL", 0.090, 0.600, 0.180, 0.612),
                word("3.98", 0.880, 0.600, 0.950, 0.612),
            ],
        ),
    ];
    let pairs = pairs_of(lines);
    assert!(pairs.contains(&("CANTALOUPE".to_string(), Money::from_cents(199))));
    assert!(pairs.contains(&("BLACKBERRIES 60Z".to_string(), Money::from_cents(199))));
}

/// Two rows whose price is embedded in an OCR-garbled trailing word
/// ("gnigoQq bn14.99") must both surface as priced items.
#[test]
fn accepts_embedded_trailing_price_word() {
    let lines = vec![
        line(
            "2146010 SEAFOOD CNTR gnigoQq bn14.99",
            vec![
                word("2146010", 0.056, 0.568, 0.190, 0.589),
                word("SEAFOOD CNTR", 0.320, 0.565, 0.539, 0.585),
                word("gnigoQq bn14.99", 0.567, 0.564, 0.899, 0.586),
            ],
        ),
        line(
            "2146010b SEAFOOD CNTR noitqQ 14.99",
            vec![
                word("2146010b", 0.060, 0.586, 0.233, 0.606),
                word("SEAFOOD CNTR", 0.320, 0.584, 0.534, 0.606),
                word("noitqQ", 0.581, 0.586, 0.706, 0.611),
                word("14.99", 0.803, 0.584, 0.898, 0.606),
            ],
        ),
        line(
            "TOTAL 29.98",
            vec![
                word("TOTAL", 0.090, 0.650, 0.180, 0.662),
                word("29.98", 0.880, 0.650, 0.950, 0.662),
            ],
        ),
    ];
    let pairs = pairs_of(lines);
    let seafood = pairs
        .iter()
        .filter(|(d, p)| d.contains("SEAFOOD CNTR") && *p == Money::from_cents(1499))
        .count();
    assert_eq!(seafood, 2);
}
