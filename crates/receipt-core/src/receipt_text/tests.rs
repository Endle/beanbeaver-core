//! Unit tests for text-line item extraction.

use super::extract_text_items;
use crate::money::Money;
use std::collections::HashSet;

#[test]
fn recovers_asterisk_tax_flag_and_ocr_merged_orphan_price() {
    // FreshCo 2026-05-02_freshcc_117_85: "$25.96*HC" must parse despite
    // the '*' separator, and the OCR-merged "$11.19" on the qty row must
    // pair with the price-less "Natrel Milk 2% 4L" line below it.
    let lines: Vec<String> = [
        "CocaCola Zero Can $25.96*HC",
        "2 @ 1/ $12.98",
        "CocaCola Zero Can $25.96xHC",
        "2 @ 1/ $12.98 $11.19 C",
        "Natrel Milk 2% 4L",
        "Eggs Large $9.79 C",
        "SUBTOTAL $73.70",
        "TOTAL $73.70",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let summary_amounts = HashSet::from([Money::from_cents(7370)]);
    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);
    let prices: Vec<Money> = items.iter().map(|it| it.price).collect();
    // Both colas recovered (asterisk tolerated) plus the orphaned Natrel.
    assert_eq!(
        prices
            .iter()
            .filter(|&&p| p == Money::from_cents(2596))
            .count(),
        2
    );
    assert!(
        items
            .iter()
            .any(|it| it.description.contains("Natrel") && it.price == Money::from_cents(1119)),
        "expected Natrel paired at 11.19, got {items:?}"
    );
}

#[test]
fn skips_compact_slash_deal_unit_rows_and_price_match_promo() {
    // FreshCo 2026-06-17_freshco_135_46: the unit-price detail rows
    // "1/ $1.99" (under Tomatos, 6 @ $1.99) and "2  1/$6.44" (under Milk
    // 2%, 2 @ $6.44) lack the "@" that the existing offer patterns key on,
    // so they leaked through as phantom items. Likewise the savings note
    // "YOU PRICE MATCHED & SAVED $6.02" is not an item. All three together
    // inflated the total by exactly $14.45 over the $132.09 subtotal.
    let lines: Vec<String> = [
        "Tomatos Diced No Slt $11.94 C",
        "1/ $1.99",
        "Milk 2% IXAL $12.88 C",
        "2  1/$6.44",
        "CocaCola Zero Can $25.96*HC",
        "2 @ $12.98",
        "YOU PRICE MATCHED & SAVED $6.02",
        "SUBTOTAL $50.78",
        "TOTAL $50.78",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let summary_amounts = HashSet::from([Money::from_cents(5078)]);
    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);
    let total: Money = items.iter().map(|it| it.price).sum();
    assert_eq!(
        total,
        Money::from_cents(5078),
        "real items must sum to the $50.78 subtotal with no phantoms, got {items:?}"
    );
    assert!(
        !items.iter().any(|it| it.price == Money::from_cents(199)),
        "compact \"1/ $1.99\" unit row must not become an item, got {items:?}"
    );
    assert!(
        !items.iter().any(|it| it.price == Money::from_cents(644)),
        "compact \"2  1/$6.44\" unit row must not become an item, got {items:?}"
    );
    assert!(
        !items.iter().any(|it| it.price == Money::from_cents(602)),
        "\"PRICE MATCHED & SAVED\" promo must not become an item, got {items:?}"
    );
}

#[test]
fn keeps_item_whose_description_ends_in_percent_fat() {
    // Costco 2026-05-24_costco_56_42: "458 MILK 2% 6.09" must parse. The
    // price-stripped description "458 MILK 2%" ends in "2%" (fat content),
    // which must NOT trip the standalone-percentage skip pattern. A bare
    // "2%" line (a tax/rate row) must still be skipped.
    let lines: Vec<String> = [
        "458 MILK 2% 6.09",
        "458 MILK 2% 6.09",
        "1346909 KS ORG 2% 4L 10.29",
        "2%",
        "SUBTOTAL 22.47",
        "TOTAL 22.47",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let summary_amounts = HashSet::from([Money::from_cents(2247)]);
    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);
    assert_eq!(
        items
            .iter()
            .filter(|it| it.price == Money::from_cents(609))
            .count(),
        2,
        "both MILK 2% lines should parse, got {items:?}"
    );
    assert!(
        !items.iter().any(|it| it.description.trim() == "2%"),
        "bare percentage line must still be skipped, got {items:?}"
    );
}

#[test]
fn emits_priced_frozen_generic_item_label() {
    // Foody Mart 2026-04-27_foody_mart_67_71: a generic "Frozen 6.99"
    // line is a real item, not a section header.
    let lines: Vec<String> = [
        "Meat 7.24",
        "Frozen 6.99",
        "Item Count: 2",
        "Sub Total 14.23",
        "Total 14.23",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let summary_amounts = HashSet::from([Money::from_cents(1423)]);
    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);
    let prices: Vec<Money> = items.iter().map(|it| it.price).collect();
    assert!(
        prices.contains(&Money::from_cents(699)),
        "Frozen 6.99 should be an item: {items:?}"
    );
}

#[test]
fn recovers_unique_malformed_three_decimal_prices_via_summary_reconciliation() {
    let lines = vec![
        "COSTCO".to_string(),
        "435259 2% FINE-FILT 6.691".to_string(),
        "430 XL EGGS 9.651".to_string(),
        "SUBTOTAL 16.38".to_string(),
        "TOTAL 16.38".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(1638)]);

    let (items, warnings) = extract_text_items(&lines, &summary_amounts);

    assert_eq!(items.len(), 2);
    assert_eq!(items[0].description, "2% FINE-FILT");
    assert_eq!(items[0].price, Money::from_cents(669));
    assert_eq!(items[1].description, "430 XL EGGS");
    assert_eq!(items[1].price, Money::from_cents(969));
    assert_eq!(warnings.len(), 2);
    assert!(warnings[0]
        .message
        .contains("auto-corrected malformed OCR price \"6.691\" -> \"6.69\""));
    assert!(warnings[1]
        .message
        .contains("auto-corrected malformed OCR price \"9.651\" -> \"9.69\""));
}

#[test]
fn skips_reg_marker_lines_with_ocr_noise_prefix() {
    let lines = vec![
        "BESTCO FRESH".to_string(),
        "*Frosh Sunkist Orange".to_string(),
        "(W)@REG$1.69".to_string(),
        "2.96 1b @ $0.99/16 2.93".to_string(),
        "TOTAL 2.93".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(293)]);

    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);

    // Should produce only one item at $2.93, not a ghost at $1.69
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].price, Money::from_cents(293));
    assert!(items[0].description.contains("Sunkist Orange"));
}

#[test]
fn skips_reg_marker_lines_with_garbled_ocr_prefix() {
    let lines = vec![
        "BESTCO FRESH".to_string(),
        "4KSf Big Instant Noodles ( 6.99".to_string(),
        "(K4AM0REG$7.99".to_string(),
        "Fish Spape Cracker (Tomat 1.99".to_string(),
        "TOTAL 8.98".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(898)]);

    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);

    // Should NOT create a ghost item at $7.99 from the REG marker line
    let prices: Vec<Money> = items.iter().map(|i| i.price).collect();
    assert!(
        !prices.contains(&Money::from_cents(799)),
        "REG marker line should not produce a ghost item at $7.99, got items: {:?}",
        items
            .iter()
            .map(|i| (&i.description, i.price))
            .collect::<Vec<_>>()
    );
}

#[test]
fn leaves_malformed_three_decimal_prices_as_warnings_without_corroborating_summary_amount() {
    let lines = vec![
        "TEST SHOP".to_string(),
        "MILK 2.991".to_string(),
        "TOTAL 2.99".to_string(),
    ];
    let summary_amounts = HashSet::new();

    let (items, warnings) = extract_text_items(&lines, &summary_amounts);

    assert!(items.is_empty());
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0]
        .message
        .contains("maybe missed item with malformed OCR price \"2.991\""));
}

#[test]
fn recovers_item_with_comma_decimal_price() {
    // OCR read this Bestco Fresh line's decimal point as a comma ("0,99").
    let lines = vec![
        "BESTCO FRESH".to_string(),
        "*Kang Shi Fu Plum Juice 50 0,99".to_string(),
        "TOTAL 0.99".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(99)]);

    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].price, Money::from_cents(99));
    assert!(items[0].description.contains("Plum Juice"));
}

#[test]
fn orphan_deal_line_price_skips_qty_row_to_reach_description_below() {
    // Foody Mart 2026-07-03_foody_mart_83_54 frozen section: the deal
    // subtext line carries the NEXT block's price, with that block's own
    // qty row in between. The coriander deal line ends inside its
    // parenthetical, so its 3.50 is subtext — not an orphan price.
    let lines = vec![
        "Fresh Corianders 1.99".to_string(),
        "(FE)@1.99(2/$3.50)".to_string(),
        "1 @ $1.99".to_string(),
        "Searay - Tofu Fish 5.96".to_string(),
        "(# 250g)@4.99(1/$2.98) 2.99".to_string(),
        "2 @ $2.98".to_string(),
        "Ten Ten - Shangdong Style".to_string(),
        "(R#5 LL 600g) @4.99 (2/$7.98) 4.99".to_string(),
        "1 @ $2.99".to_string(),
        "Ten Ten - Pork Bun".to_string(),
        "(RR#50# 360g)@4.99(2/$7.98)".to_string(),
        "1 @ $4.99".to_string(),
        "Sub Total 15.44".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(1544)]);

    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);
    let observed: Vec<(String, Money)> = items
        .into_iter()
        .map(|item| (item.description, item.price))
        .collect();

    assert!(
        observed.contains(&("Fresh Corianders".to_string(), Money::from_cents(199))),
        "{observed:?}"
    );
    assert!(
        observed.contains(&("Searay - Tofu Fish".to_string(), Money::from_cents(596))),
        "{observed:?}"
    );
    assert!(
        observed.contains(&(
            "Ten Ten - Shangdong Style".to_string(),
            Money::from_cents(299)
        )),
        "{observed:?}"
    );
    assert!(
        observed.contains(&("Ten Ten - Pork Bun".to_string(), Money::from_cents(499))),
        "{observed:?}"
    );
    assert_eq!(observed.len(), 4, "{observed:?}");
}

#[test]
fn weight_row_total_validates_against_rate_and_frees_drifted_price() {
    // Foody Mart 2026-07-09_foody_mart_137_73 meat cluster: the wings'
    // weight row carries the NEXT item's 7.45 (3.37 × $2.98 = 10.04, so
    // the trailing price can't be the row's own total), and the bare
    // "Meat" counter label below is the item it prices. Works without
    // receipt-level drift — the weight math alone frees the orphan.
    let lines = vec![
        "Fresh Chicken Wings".to_string(),
        "(WRER) 10.04".to_string(),
        "3.37 1b @ $2.98/1b 7.45".to_string(),
        "Meat".to_string(),
        "Meat 4.19".to_string(),
        "Sub Total 21.68".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(2168)]);

    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);
    let observed: Vec<(String, Money)> = items
        .into_iter()
        .map(|item| (item.description, item.price))
        .collect();

    assert!(
        observed
            .iter()
            .any(|(d, p)| d.starts_with("Fresh Chicken Wings") && *p == Money::from_cents(1004)),
        "{observed:?}"
    );
    assert!(
        observed.contains(&("Meat".to_string(), Money::from_cents(745))),
        "{observed:?}"
    );
    assert!(
        observed.contains(&("Meat".to_string(), Money::from_cents(419))),
        "{observed:?}"
    );
}

#[test]
fn coincidental_qty_echo_pairs_downward_under_receipt_drift() {
    // Foody Mart 2026-07-09_foody_mart_137_73: on a receipt whose price
    // column systematically leans up (three non-reconciling qty rows
    // establish it), "1 @ $1.99  1.99" coincidentally equals its own
    // total but is really TCMC's price — the Margina item above is
    // already priced, so the row has nothing left to donate upward.
    let lines = vec![
        "HLY - Fish Cracker Seawee 2.59".to_string(),
        "(33g)@2.59(2/$3.50)".to_string(),
        "1 @ $2.59 0.91".to_string(),
        "HLY - Fish Cracker Seawee".to_string(),
        "(33g)@2.59(2/$3.50)".to_string(),
        "1 @ $0.91 2.99".to_string(),
        "NX - Dried Sweet Potato V".to_string(),
        "(500g)@2.99(2/$5.00)".to_string(),
        "1 @ $2.99 1.99".to_string(),
        "Margina Strawberry Flavor".to_string(),
        "(95g)@1.99(2/$2.50)".to_string(),
        "1 @ $1.99 1.99".to_string(),
        "TCMC - Strawberry Flavore".to_string(),
        "Sub Total 9.47".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(947)]);

    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);
    let observed: Vec<(String, Money)> = items
        .into_iter()
        .map(|item| (item.description, item.price))
        .collect();

    assert!(
        observed.contains(&(
            "TCMC - Strawberry Flavore".to_string(),
            Money::from_cents(199)
        )),
        "{observed:?}"
    );
    assert!(
        observed.contains(&(
            "Margina Strawberry Flavor".to_string(),
            Money::from_cents(199)
        )),
        "{observed:?}"
    );
}

#[test]
fn paren_subtext_price_pairs_forward_when_description_above_consumed() {
    // Foody Mart 2026-07-09_foody_mart_137_73 frozen section: "Pork Lard"
    // is priced by the qty row above it, so the trailing 2.98 on its size
    // subtext "(3 380g)" belongs to Pak Fok below — the old backward walk
    // glued it onto Pork Lard as a duplicate. The unclaimed-description
    // case must keep walking backward ("Fresh Chicken Wings"-style).
    let lines = vec![
        "Genuine - Fried Soya Cake 1.98".to_string(),
        "1 @ $1.98 3.98".to_string(),
        "Sanquan - Yellow Millet C".to_string(),
        "(360g)@5.99(1/$3.98)".to_string(),
        "1 @ $3.98 1.28".to_string(),
        "LBT - Frozen Sandwich".to_string(),
        "(13)@2.99(1/$1.28)".to_string(),
        "1 @ $1.28 3.99".to_string(),
        "Pork Lard".to_string(),
        "(3 380g) 2.98".to_string(),
        "Pak Fok - Fried Tofu".to_string(),
        "(150g)@3.59(1/$2.98)".to_string(),
        "1 @ $2.98".to_string(),
        "Sub Total 14.21".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(1421)]);

    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);
    let observed: Vec<(String, Money)> = items
        .into_iter()
        .map(|item| (item.description, item.price))
        .collect();

    assert!(
        observed.contains(&("Pork Lard".to_string(), Money::from_cents(399))),
        "{observed:?}"
    );
    assert!(
        observed
            .iter()
            .any(|(d, p)| d.starts_with("Pak Fok - Fried Tofu") && *p == Money::from_cents(298)),
        "{observed:?}"
    );
    assert!(
        observed
            .iter()
            .any(|(d, p)| d.starts_with("LBT - Frozen Sandwich") && *p == Money::from_cents(128)),
        "{observed:?}"
    );
    assert!(
        observed.iter().any(
            |(d, p)| d.starts_with("Sanquan - Yellow Millet C") && *p == Money::from_cents(398)
        ),
        "{observed:?}"
    );
    // Pork Lard must not also appear at Pak Fok's price (the old
    // backward-walk duplicate).
    assert!(
        !observed
            .iter()
            .any(|(d, p)| d.starts_with("Pork Lard") && *p == Money::from_cents(298)),
        "{observed:?}"
    );
}

#[test]
fn priced_header_chain_resolves_first_two_items_under_drift() {
    // Foody Mart 2026-06-28_foody_mart_115_56: the header carries the
    // first item's price AND the first item's name row carries the
    // second item's — skipping priced rows in the header's forward search
    // crossed the pairing (Nissin got 5.59, Wasabi got 2.68). The two
    // priced headers plus the non-reconciling deal row establish drift.
    let lines = vec![
        "&& 01-Grocery # 5.59".to_string(),
        "S & B - Wasabi 2.68".to_string(),
        "( 90g)".to_string(),
        "Nissin - Chicken Flavour".to_string(),
        "(0T 5x100g)@4.99 (1/$2.68) 4.99".to_string(),
        "1 @ $2.68".to_string(),
        "Shodoshima - Asian Style".to_string(),
        "(EMI620g)".to_string(),
        "&& 19-Dim Sum 2.98".to_string(),
        "Dim Sum".to_string(),
        "Sub Total 16.24".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(1624)]);

    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);
    let observed: Vec<(String, Money)> = items
        .into_iter()
        .map(|item| (item.description, item.price))
        .collect();

    assert!(
        observed.contains(&("S & B - Wasabi".to_string(), Money::from_cents(559))),
        "{observed:?}"
    );
    assert!(
        observed.contains(&(
            "Nissin - Chicken Flavour".to_string(),
            Money::from_cents(268)
        )),
        "{observed:?}"
    );
    assert!(
        observed.contains(&(
            "Shodoshima - Asian Style".to_string(),
            Money::from_cents(499)
        )),
        "{observed:?}"
    );
    assert!(
        observed.contains(&("Dim Sum".to_string(), Money::from_cents(298))),
        "{observed:?}"
    );
    assert_eq!(observed.len(), 4, "{observed:?}");
}

#[test]
fn mangled_reg_row_forwards_drifted_price_under_drift() {
    // Bestco 2026-06-25_fresh_183_77: "(-EG4.99  2.99" is a mangled
    // @REG$4.99 marker whose TRAILING price is the next item's (LKS).
    // Suppressing the row (the straight-receipt behavior) dropped LKS;
    // pairing backward glued 2.99 onto Yang Guo Fu. Three priced section
    // headers establish the drift.
    let lines = vec![
        "&& Grocery 6.99".to_string(),
        "Hot Bean Sauce 450g".to_string(),
        "&& Taxed Grocery 2.59".to_string(),
        "Heytea Kale Plant Beverag".to_string(),
        "&& Vegetable 4.79".to_string(),
        "Green Long Hot Pepper".to_string(),
        "*Yang Guo Fu Spicy Hot Pot 3.99".to_string(),
        "(-EG4.99 2.99".to_string(),
        "LKS Dried Cod Fish Slice".to_string(),
        "*DongBei Sticky Spicy Hot 4.99".to_string(),
        "Sub Total 26.34".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(2634)]);

    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);
    let observed: Vec<(String, Money)> = items
        .into_iter()
        .map(|item| (item.description, item.price))
        .collect();

    assert!(
        observed.contains(&(
            "LKS Dried Cod Fish Slice".to_string(),
            Money::from_cents(299)
        )),
        "{observed:?}"
    );
    assert!(
        observed.contains(&(
            "*Yang Guo Fu Spicy Hot Pot".to_string(),
            Money::from_cents(399)
        )),
        "{observed:?}"
    );
    assert!(
        observed.contains(&("Hot Bean Sauce 450g".to_string(), Money::from_cents(699))),
        "{observed:?}"
    );
}

#[test]
fn weight_rate_row_with_dropped_at_still_prices_item_above() {
    // Bestco 2026-06-25_fresh_183_77: OCR dropped the '@' from Fresh
    // Ginger's weight row ("1.86 lb  $2.49/lb  4.63"), so it wasn't a
    // quantity expression and became a phantom item with the weight text
    // as its description. The '/unit' tail licenses the no-@ rate form.
    let lines = vec![
        "Fresh Ginger".to_string(),
        "1.86 1b  $2.49/1b 4.63".to_string(),
        "Sub Total 4.63".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(463)]);

    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);
    let observed: Vec<(String, Money)> = items
        .into_iter()
        .map(|item| (item.description, item.price))
        .collect();

    assert!(
        observed
            .iter()
            .any(|(d, p)| d.starts_with("Fresh Ginger") && *p == Money::from_cents(463)),
        "{observed:?}"
    );
    assert_eq!(observed.len(), 1, "{observed:?}");
}

#[test]
fn subtext_price_stays_with_unclaimed_item_above() {
    // Foody Mart 2026-05-21_foody_mart_53_05: each HLY block prints its
    // OWN price on its deal-subtext row ("(43H)@3.99(1/$0.98)  5.88H")
    // with the name above still unclaimed — the downward orphan pairing
    // must not steal it for the next block. Also covers the OCR `@`→`0`
    // deal-row shape ("(HEARH)03.99(…)"), which must still read as a
    // quantity expression so the price walks back to Origin.
    let lines = vec![
        "HLY - Potato Chips Honey".to_string(),
        "(43H)@3.99(1/$0.98) 5.88H".to_string(),
        "6 @ $0.98".to_string(),
        "HLY - Potato Chips Origin".to_string(),
        "(HEARH)03.99(1/$0.98) 2.94H".to_string(),
        "3 @ $0.98".to_string(),
        "Sub Total 8.82".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(882)]);

    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);
    let observed: Vec<(String, Money)> = items
        .into_iter()
        .map(|item| (item.description, item.price))
        .collect();

    assert!(
        observed
            .iter()
            .any(|(d, p)| d.starts_with("HLY - Potato Chips Honey") && *p == Money::from_cents(588)),
        "{observed:?}"
    );
    assert!(
        observed.iter().any(
            |(d, p)| d.starts_with("HLY - Potato Chips Origin") && *p == Money::from_cents(294)
        ),
        "{observed:?}"
    );
    assert_eq!(observed.len(), 2, "{observed:?}");
}

#[test]
fn multiline_name_continuation_does_not_block_orphan_pairing() {
    // Foody Mart 2026-04-24_foody_mart_70_68: Natrel's name wraps onto a
    // second, unclaimed row. The consumed-above walk must read through
    // the continuation to the claimed first row, or the qty row's
    // drifted 7.59 never reaches Gray Ridge below.
    let lines = vec![
        "Natrel 4.98".to_string(),
        "1 - 2% Partly Skimme".to_string(),
        "((015640541)@8.99(1/$4.98)".to_string(),
        "1 @$4.98 7.59".to_string(),
        "Gray Ridge - White Fegs E".to_string(),
        "Sub Total 12.57".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(1257)]);

    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);
    let observed: Vec<(String, Money)> = items
        .into_iter()
        .map(|item| (item.description, item.price))
        .collect();

    assert!(
        observed
            .iter()
            .any(|(d, p)| d.starts_with("Natrel") && *p == Money::from_cents(498)),
        "{observed:?}"
    );
    assert!(
        observed
            .iter()
            .any(|(d, p)| d.starts_with("Gray Ridge") && *p == Money::from_cents(759)),
        "{observed:?}"
    );
}

#[test]
fn strips_embedded_unit_price_and_tax_flags_from_description() {
    // Shoppers 2026-06-30_shoppers_23_72: "VICKS SINUS CO 20.99 GP 20.99"
    // (description, unit price + tax flags, extended price on one row).
    let lines = vec![
        "SCO CheckOut".to_string(),
        "VICKS SINUS CO 20.99 GP 20.99".to_string(),
        "SUBTOTAL: 20.99".to_string(),
        "TOTAL: $23.72".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(2099), Money::from_cents(2372)]);

    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].description, "VICKS SINUS CO");
    assert_eq!(items[0].price, Money::from_cents(2099));
}

#[test]
fn comma_decimal_normalization_leaves_non_price_commas_untouched() {
    use super::engine::normalize_decimal_spacing;
    // Positive: a comma between a digit and exactly two fraction digits.
    assert_eq!(normalize_decimal_spacing("0,99"), "0.99");
    assert_eq!(normalize_decimal_spacing("item 12,49 H"), "item 12.49 H");
    // Negative: thousands separators and prose stay as-is.
    assert_eq!(normalize_decimal_spacing("1,000"), "1,000");
    assert_eq!(normalize_decimal_spacing("12,345"), "12,345");
    assert_eq!(normalize_decimal_spacing("Anytown, ON"), "Anytown, ON");
    // Negative: three fraction digits are not a clean 2-decimal price.
    assert_eq!(normalize_decimal_spacing("0,999"), "0,999");
}

#[test]
fn extract_trailing_price_cents_signs_discounts() {
    use super::engine::extract_trailing_price_cents;
    // Leading-minus discount convention (e.g. Jin Lian "D9 -$1.96").
    assert_eq!(
        extract_trailing_price_cents("250g D9 -$1.96").map(|t| t.0),
        Some(Money::from_cents(-196))
    );
    assert_eq!(
        extract_trailing_price_cents("JL5 -$5.00").map(|t| t.0),
        Some(Money::from_cents(-500))
    );
    // Costco trailing-minus stays negative (and isn't double-handled).
    assert_eq!(
        extract_trailing_price_cents("TPD/1796144 3.00-").map(|t| t.0),
        Some(Money::from_cents(-300))
    );
    // Plain prices stay positive.
    assert_eq!(
        extract_trailing_price_cents("Meat 20.53").map(|t| t.0),
        Some(Money::from_cents(2053))
    );
    // Guards: a mid-token hyphen and a spaced " - " separator must NOT
    // flip the sign — only a '-' glued to the price (directly or via '$').
    assert_eq!(
        extract_trailing_price_cents("ITEM-1.96").map(|t| t.0),
        Some(Money::from_cents(196))
    );
    assert_eq!(
        extract_trailing_price_cents("MILK 2% - 3.99").map(|t| t.0),
        Some(Money::from_cents(399))
    );
}

#[test]
fn parses_leading_minus_discount_lines_as_negative_items() {
    // Mirrors the Jin Lian Food receipt: discount lines print the minus
    // ahead of the price ("-$1.96") rather than Costco's trailing form.
    let lines = vec![
        "JIN LIAN FOOD".to_string(),
        "2LB $F Pacific white $13.99".to_string(),
        "250g D9 -$1.96".to_string(),
        "D7 -$5.28".to_string(),
        "SUBTOTAL $55.49".to_string(),
        "TOTAL $50.49".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(5549), Money::from_cents(5049)]);

    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);
    let prices: Vec<Money> = items.iter().map(|it| it.price).collect();
    assert!(
        prices.contains(&Money::from_cents(1399)),
        "positive item missing: {prices:?}"
    );
    assert!(
        prices.contains(&Money::from_cents(-196)),
        "D9 discount must be negative: {prices:?}"
    );
    assert!(
        prices.contains(&Money::from_cents(-528)),
        "D7 discount must be negative: {prices:?}"
    );
}

#[test]
fn parses_tx_category_tax_suffix_prices() {
    // Sunny Foodmart suffixes a category digit after the tax flag
    // ("$3.88 Tx1"); these must still extract as normal prices.
    let lines = vec![
        "SUNNY FOODMART".to_string(),
        "Coconut Water $3.88 Tx1".to_string(),
        "Sweet Potato Noodle $5.98".to_string(),
        "SUB TOTAL $9.86".to_string(),
        "TOTAL $9.86".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(986)]);

    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);
    let prices: Vec<Money> = items.iter().map(|it| it.price).collect();
    assert!(
        prices.contains(&Money::from_cents(388)),
        "Tx1-suffixed price not recovered: {prices:?}"
    );
    assert!(
        prices.contains(&Money::from_cents(598)),
        "plain price missing: {prices:?}"
    );
}

#[test]
fn recovers_letter_fraction_malformed_price_via_reconciliation() {
    // "0.91" OCR'd as "0.9I" (1 -> I); recovered through malformed-price
    // reconciliation once the subtotal corroborates it, instead of being
    // left as a warning-only missed item.
    let lines = vec![
        "FOODY MART".to_string(),
        "HLY - Fish Cracker Seawee 2.59H".to_string(),
        "HLY - Fish Cracker Seawee 0.9IH".to_string(),
        "Sub Total 3.50".to_string(),
        "Total 3.50".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(350)]);

    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);
    let prices: Vec<Money> = items.iter().map(|it| it.price).collect();
    assert!(
        prices.contains(&Money::from_cents(259)),
        "regular item missing: {prices:?}"
    );
    assert!(
        prices.contains(&Money::from_cents(91)),
        "0.9I should reconcile to 0.91: {prices:?}"
    );
}

#[test]
fn drops_item_price_exceeding_receipt_total() {
    // Sunny Foodmart: "$1.58" misread as "81.58". With the Tx1 suffix now
    // parsed it would otherwise surface as an 81.58 item, far above the
    // $25.44 total — the ceiling guard drops it (prefer missing over wrong).
    let lines = vec![
        "SUNNY FOODMART".to_string(),
        "Alienercy Vitamin B Drink 81.58 Tx1".to_string(),
        "Coconut Water $3.88 Tx1".to_string(),
        "SUB TOTAL $24.32".to_string(),
        "TOTAL $25.44".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(2432), Money::from_cents(2544)]);

    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);
    let prices: Vec<Money> = items.iter().map(|it| it.price).collect();
    assert!(
        !prices.contains(&Money::from_cents(8158)),
        "81.58 outlier should be dropped: {prices:?}"
    );
    assert!(
        prices.contains(&Money::from_cents(388)),
        "valid Tx1 item should remain: {prices:?}"
    );
}

#[test]
fn column_header_total_is_not_the_grand_total_and_tender_backstops_the_cap() {
    // Pharmasave 2026-07-07_pharmasave_12_19: the "DESCRIPTION QTY UNIT
    // TOTAL" column header must not terminate the item region, and with
    // the TOTAL row mis-grouped to the HST amount ($1.40) the VISA tender
    // has to backstop the implausible-price ceiling so the $10.79 item
    // survives.
    let lines: Vec<String> = vec![
        "GRAND GENESIS",
        "PHARMASAVE",
        "HAVE GREAT DAY!",
        "DESCRIPTION QTY UNIT TOTAL",
        "TOOTHPASTE 1 $10.79 PRICE PRICE",
        "06081503923 $10.79 G",
        "SUBTOTAL",
        "HST $10.79",
        "TOTAL $1.40",
        "VISA $12.19",
        "CHANGE DUE $12.19 $0.00",
        "Items = 1",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let summary = HashSet::from([
        Money::from_cents(1079),
        Money::from_cents(140),
        Money::from_cents(1219),
    ]);
    let (items, _warnings) = extract_text_items(&lines, &summary);
    let observed: Vec<(String, Money)> = items
        .into_iter()
        .map(|item| (item.description, item.price))
        .collect();
    assert!(
        observed
            .iter()
            .any(|(d, p)| d.contains("TOOTHPASTE") && *p == Money::from_cents(1079)),
        "{observed:?}"
    );
}

#[test]
fn ocr_noise_between_weight_unit_and_at_still_prices_item_above() {
    // Foody Mart 2026-07-29: the produce rows' "lb @" came through as
    // "1b'@" (speckle before the @) and "16-@" (b read as 6, plus a dash).
    // Neither matched the weight-rate pattern, so each row stopped being a
    // quantity expression and became its own item — stealing the price and
    // dropping the real description printed on the line above.
    let lines = vec![
        "Napa Round".to_string(),
        "3.74 1b'@ $0.98/1b 3.67".to_string(),
        "Winter Melon".to_string(),
        "2.32 16-@ $1.59/16 3.69".to_string(),
        "Sub Total 7.36".to_string(),
    ];
    let summary_amounts = HashSet::from([Money::from_cents(736)]);

    let (items, _warnings) = extract_text_items(&lines, &summary_amounts);
    let observed: Vec<(String, Money)> = items
        .into_iter()
        .map(|item| (item.description, item.price))
        .collect();

    assert!(
        observed
            .iter()
            .any(|(d, p)| d == "Napa Round" && *p == Money::from_cents(367)),
        "{observed:?}"
    );
    assert!(
        observed
            .iter()
            .any(|(d, p)| d == "Winter Melon" && *p == Money::from_cents(369)),
        "{observed:?}"
    );
    assert!(
        !observed.iter().any(|(d, _)| d.starts_with("3.74")),
        "weight row leaked as an item: {observed:?}"
    );
}
