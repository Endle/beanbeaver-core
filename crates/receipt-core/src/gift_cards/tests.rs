use crate::money::Money;
use crate::ocr_document::OcrDocument;
use crate::parser::ParsedReceiptItem;

use super::*;
fn redemption(text: &str) -> Vec<GiftCardRedemption> {
    let doc = OcrDocument::from_text(text);
    let lines = text.lines().map(String::from).collect::<Vec<_>>();
    extract_redemptions(&doc, "LCBO", &crate::fields::extract_tenders(&lines))
        .into_iter()
        .flatten()
        .collect()
}
#[test]
fn two_lcbo_cards_keep_zero_balance_masks_and_auth_separate() {
    let cards = redemption("Gift Card 50.00\n123456xxxxx9876543x EXP:NONE\nAUTHOR.#:123456 BAL: 0.00\nGift Card 9.70\n123456×xxx×1112223× EXP:NONE\nAUTHOR.#:789012 BAL: 90.30\nUnits Purchased: 2\nPOINTS BAL: 999.00");
    assert_eq!(cards.len(), 2);
    assert_eq!(cards[0].remaining_balance_cents, Some(0));
    assert_eq!(cards[1].remaining_balance_cents, Some(9030));
    assert_eq!(
        cards[1].normalized_identifier.as_deref(),
        Some("123456*****1112223*")
    );
    assert_eq!(
        cards[1].printed_identifier.as_deref(),
        Some("123456×xxx×1112223×")
    );
    assert_eq!(cards[0].authorization_reference.as_deref(), Some("123456"));
    assert_eq!(cards[0].expiry, GiftCardExpiry::NoExpiry);
    assert_eq!(
        cards[1]
            .evidence
            .iter()
            .find(|e| e.field == "remaining_balance_cents")
            .unwrap()
            .line_index,
        5
    );
}
#[test]
fn split_balance_and_missing_balance_are_different() {
    let cards = redemption("Gift Card 10.00\n0.00\nBAL:\nGift Card 12.00\n123456xxxxx1112223x");
    assert_eq!(cards[0].remaining_balance_cents, Some(0));
    assert_eq!(cards[1].remaining_balance_cents, None);
    assert_eq!(cards[1].expiry, GiftCardExpiry::Unknown);
}
#[test]
fn balance_labels_survive_missing_colons_without_matching_longer_words() {
    for label in ["BAL", "REMAINING BALANCE", "Gift Card Balance"] {
        let cards = redemption(&format!(
            "Gift Card 12.00\nAUTHOR.#:123456 {label} 18.00\nGift Card 3.00\n{label}: 0.00"
        ));
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].remaining_balance_cents, Some(1800), "{label}");
        assert_eq!(cards[1].remaining_balance_cents, Some(0), "{label}");
        assert!(cards[0].unresolved_fields.is_empty());
    }
    for label in ["BALLOON", "BALANCE", "GLOBAL", "REMAINING BALANCES"] {
        let cards = redemption(&format!("Gift Card 12.00\n{label} 18.00"));
        assert_eq!(cards[0].remaining_balance_cents, None, "{label}");
    }
}

#[test]
fn colonless_balance_keeps_invalid_amounts_and_conflicts_unresolved() {
    for detail in [
        "BAL -18.00",
        "BAL 1,80.00",
        "BAL 18.00\nBAL 19.00",
        "BAL unreadable",
    ] {
        let cards = redemption(&format!("Gift Card 12.00\n{detail}"));
        assert_eq!(cards[0].remaining_balance_cents, None, "{detail}");
        assert!(cards[0]
            .unresolved_fields
            .contains(&"remaining_balance_cents".into()));
    }
    let cards = redemption("Gift Card 12.00\nBAL\nGift Card 3.00\nBAL 18.00\nPOINTS BAL 99.00");
    assert_eq!(cards[0].remaining_balance_cents, None);
    assert_eq!(cards[1].remaining_balance_cents, Some(1800));
}

#[test]
fn authorization_reference_survives_spacing_between_punctuation() {
    for label in [
        "AUTHOR.#:",
        "AUTHOR. #:",
        "AUTHOR . # :",
        "AUTH :",
        "APP #:",
        "Approval Code:",
    ] {
        let cards = redemption(&format!("Gift Card 12.00\n{label}123456 BAL 18.00"));
        assert_eq!(
            cards[0].authorization_reference.as_deref(),
            Some("123456"),
            "{label}"
        );
    }
}
#[test]
fn conflicts_and_unreadable_values_stay_unresolved() {
    let cards = redemption("Gift Card 10.00\n123456xxxxx1112223x\n123456xxxxx4445556x\nBAL: 10.00\nBAL: 20.00\nEXP:???\nGift Card 2.00\nBAL: unreadable");
    assert_eq!(cards[0].printed_identifier, None);
    assert_eq!(cards[0].remaining_balance_cents, None);
    assert_eq!(cards[0].expiry, GiftCardExpiry::Unknown);
    assert!(cards[0]
        .unresolved_fields
        .contains(&"printed_identifier".into()));
    assert!(cards[1]
        .unresolved_fields
        .contains(&"remaining_balance_cents".into()));
}
#[test]
fn costco_balance_does_not_adopt_charge_echo_or_next_card() {
    let cards = redemption("TOTAL 250.00\nxxxxxxxxxxxx1234 SCANNED\nAPP#: 1111\nShop Card Resp: Approved\nREMAINING BALANCE: $0.00 AMOUNT: $50.00\nShop Card 50.00\nxxxxxxxxxxxx5678 SCANNED\nAPP#: 2222\nShop Card Resp: Approved\nAMOUNT: $200.00\nREMAINING BALANCE: $0.00\nShop Card 200.00\nxxxxxxxxxxxx9999\nACCT: MASTERCARD");
    assert_eq!(cards.len(), 2);
    assert_eq!(cards[0].remaining_balance_cents, Some(0));
    assert_eq!(cards[1].remaining_balance_cents, Some(0));
    assert_eq!(
        cards[0].printed_identifier.as_deref(),
        Some("xxxxxxxxxxxx1234")
    );
    assert_eq!(
        cards[1].printed_identifier.as_deref(),
        Some("xxxxxxxxxxxx5678")
    );
    assert_eq!(cards[1].authorization_reference.as_deref(), Some("2222"));
}
fn item(description: &str) -> ParsedReceiptItem {
    crate::parser::classified_item(
        description.into(),
        Money::from_cents(7999),
        1,
        &crate::rules::default_parser_rule_layers(),
    )
}
#[test]
fn packs_and_identical_products_keep_independent_activation_references() {
    let doc = OcrDocument::from_text("399 DOORDASH2X50 79.99\nPC 111111 ACTIVATED\n399 DOORDASH2X50 79.99\nPC 222222 ACTIVATED\n810 LCBO CARD 400.00\nPC 333333 ACTIVATED\nSUBTOTAL 559.98");
    let mut items = vec![
        item("399 DOORDASH2X50"),
        item("399 DOORDASH2X50"),
        item("810 LCBO CARD"),
    ];
    attach_purchases(&doc, "COSTCO", &mut items);
    let a = items[0].gift_card.as_ref().unwrap();
    let b = items[1].gift_card.as_ref().unwrap();
    let c = items[2].gift_card.as_ref().unwrap();
    assert_eq!(a.reference.as_deref(), Some("111111"));
    assert_eq!(b.reference.as_deref(), Some("222222"));
    assert_eq!(a.activation, GiftCardActivation::Activated);
    assert_eq!(a.card_count, Some(2));
    assert_eq!(a.denomination_cents, Some(5000));
    assert_eq!(a.total_face_value_cents, Some(10000));
    assert!(a.face_value_derived);
    assert_eq!(c.total_face_value_cents, None);
    assert_eq!(c.card_count, None);
    let mut missing = vec![item("399 DOORDASH2X50")];
    attach_purchases(&doc, "COSTCO", &mut missing);
    let g = missing[0].gift_card.as_ref().unwrap();
    assert_eq!(g.reference, None);
    assert!(g.unresolved_fields.contains(&"association".into()));
}
/// What `parse_receipt` actually emits for two identical packs — the item
/// number populated on both, and stripped from one description but not the
/// other (`costco_biz_20260125`); or the same number on both with one OCR
/// spelling missing its `X` (`2026-08-30_costco_235_05`). Keying on the raw
/// description, or falling back to the shared item number, left both real
/// receipts `unresolved: ["association"]` on clean OCR.
#[test]
fn identical_packs_resolve_with_item_numbers_as_the_parser_emits_them() {
    let doc = OcrDocument::from_text("399 DOORDASH2X50 79.99\nPC 111111 ACTIVATED\n399 DOORDASH2X50 79.99\nPC 222222 ACTIVATED\nSUBTOTAL 159.98");
    let mut items = vec![item("DOORDASH2X50"), item("399 DOORDASH2X50")];
    for it in &mut items {
        it.item_number = Some("399".into());
    }
    attach_purchases(&doc, "COSTCO", &mut items);
    let a = items[0].gift_card.as_ref().unwrap();
    let b = items[1].gift_card.as_ref().unwrap();
    assert_eq!(a.reference.as_deref(), Some("111111"));
    assert_eq!(b.reference.as_deref(), Some("222222"));
    assert!(a.unresolved_fields.is_empty() && b.unresolved_fields.is_empty());

    let doc = OcrDocument::from_text("399 DOORDASH2 50 79.99\nPC 333333 ACTIVATED\n399 DOORDASH2X50 79.99\nPC 444444 ACTIVATED\nSUBTOTAL 159.98");
    let mut items = vec![item("399 DOORDASH2 50"), item("399 DOORDASH2X50")];
    for it in &mut items {
        it.item_number = Some("399".into());
    }
    attach_purchases(&doc, "COSTCO", &mut items);
    assert_eq!(
        items[0].gift_card.as_ref().unwrap().reference.as_deref(),
        Some("333333")
    );
    assert_eq!(
        items[1].gift_card.as_ref().unwrap().reference.as_deref(),
        Some("444444")
    );
    // The mangled spelling has no pack notation, so no face value is derived
    // for it — the price never stands in for one.
    assert_eq!(items[0].gift_card.as_ref().unwrap().card_count, None);
    assert_eq!(items[1].gift_card.as_ref().unwrap().card_count, Some(2));
}

/// The item-number fallback is for a description the parser rewrote past
/// recognition; it never widens a key that already found its row.
#[test]
fn item_number_fallback_only_runs_when_the_description_finds_no_row() {
    let doc = OcrDocument::from_text(
        "399 DOORDASH2X50 79.99\nPC 111111 ACTIVATED\n810 LCBO CARD 400.00\nPC 222222 ACTIVATED\nSUBTOTAL 479.99",
    );
    let mut items = vec![item("DOORDASH GIFT"), item("LCBO CARD")];
    items[0].item_number = Some("399".into());
    attach_purchases(&doc, "COSTCO", &mut items);
    assert_eq!(
        items[0].gift_card.as_ref().unwrap().reference.as_deref(),
        Some("111111")
    );
    assert_eq!(
        items[1].gift_card.as_ref().unwrap().reference.as_deref(),
        Some("222222")
    );
    // Two rewritten items sharing a number stay ambiguous rather than guessed.
    let mut items = vec![item("DOORDASH GIFT"), item("DOORDASH GIFT")];
    for it in &mut items {
        it.item_number = Some("399".into());
    }
    attach_purchases(&doc, "COSTCO", &mut items);
    for it in &items {
        let g = it.gift_card.as_ref().unwrap();
        assert_eq!(g.reference, None);
        assert!(g.unresolved_fields.contains(&"association".into()));
    }
}

#[test]
fn absent_activation_is_unknown_and_non_costco_purchase_is_not_invented() {
    let doc = OcrDocument::from_text("810 LCBO CARD 400.00\nSUBTOTAL 400.00");
    let mut items = vec![item("810 LCBO CARD")];
    attach_purchases(&doc, "COSTCO", &mut items);
    assert_eq!(
        items[0].gift_card.as_ref().unwrap().activation,
        GiftCardActivation::Unknown
    );
    let mut items = vec![item("810 LCBO CARD")];
    attach_purchases(&doc, "LCBO", &mut items);
    assert!(items[0].gift_card.is_none());
}
#[test]
fn schema_roundtrip_keeps_zero_and_old_fields_default_to_unknown() {
    let old: GiftCardRedemption = serde_json::from_str("{}").unwrap();
    assert_eq!(old.expiry, GiftCardExpiry::Unknown);
    assert_eq!(old.remaining_balance_cents, None);
    let g = redemption("Gift Card 10.00\nBAL: 0.00").remove(0);
    assert_eq!(
        serde_json::from_str::<GiftCardRedemption>(&serde_json::to_string(&g).unwrap()).unwrap(),
        g
    );
}
#[test]
fn star_masks_and_thousands_separators_are_preserved_without_guessing() {
    let cards = redemption("Gift Card 10.00\n************1234 EXP:2028/12/31\nBAL: $1,234.56");
    assert_eq!(
        cards[0].printed_identifier.as_deref(),
        Some("************1234")
    );
    assert_eq!(cards[0].remaining_balance_cents, Some(123456));
    assert_eq!(cards[0].expiry, GiftCardExpiry::PrintedDate);
    assert_eq!(cards[0].expiry_date.as_deref(), Some("2028/12/31"));
}

#[test]
fn an_unreadable_balance_cannot_borrow_the_tender_amount() {
    let cards = redemption("Gift Card\n10.00\nBAL:\nGift Card 5.00\nBAL: ???");
    for card in cards {
        assert_eq!(card.remaining_balance_cents, None);
        assert!(card
            .unresolved_fields
            .contains(&"remaining_balance_cents".into()));
    }
}

#[test]
fn geometry_orders_reordered_annotations_before_association() {
    let mut doc=OcrDocument::from_text("399 DOORDASH2X50 79.99\nPC 111111 ACTIVATED\n399 DOORDASH2X50 79.99\nPC 222222 ACTIVATED\nSUBTOTAL 159.98");
    for (i, row) in doc.lines.iter_mut().enumerate() {
        row.center_y = (i + 1) as f64 / 10.0;
    }
    doc.lines.swap(1, 3);
    let mut items = vec![item("399 DOORDASH2X50"), item("399 DOORDASH2X50")];
    attach_purchases(&doc, "COSTCO", &mut items);
    assert_eq!(
        items[0].gift_card.as_ref().unwrap().reference.as_deref(),
        Some("111111")
    );
    assert_eq!(
        items[1].gift_card.as_ref().unwrap().reference.as_deref(),
        Some("222222")
    );
    assert_eq!(
        items[0]
            .gift_card
            .as_ref()
            .unwrap()
            .evidence
            .iter()
            .find(|e| e.field == "reference")
            .unwrap()
            .line_index,
        3
    );
}

#[test]
fn unreadable_references_and_negative_activation_are_unknown() {
    let doc = OcrDocument::from_text(
        "810 LCBO CARD 400.00\nPC unreadable NOT ACTIVATED\nSUBTOTAL 400.00",
    );
    let mut items = vec![item("810 LCBO CARD")];
    attach_purchases(&doc, "COSTCO", &mut items);
    let gift = items[0].gift_card.as_ref().unwrap();
    assert_eq!(gift.reference, None);
    assert_eq!(gift.reference_label.as_deref(), Some("PC"));
    assert_eq!(gift.activation, GiftCardActivation::Unknown);
    assert!(gift.unresolved_fields.contains(&"activation".into()));
    assert!(gift.unresolved_fields.contains(&"reference".into()));
}

#[test]
fn a_missing_product_does_not_donate_its_activation_to_the_previous_item() {
    let doc = OcrDocument::from_text(
        "810 LCBO CARD 400.00\n999 ANOTHER CARD 50.00\nPC 123456 ACTIVATED\nSUBTOTAL 450.00",
    );
    let mut items = vec![item("810 LCBO CARD")];
    attach_purchases(&doc, "COSTCO", &mut items);
    let gift = items[0].gift_card.as_ref().unwrap();
    assert_eq!(gift.reference, None);
    assert_eq!(gift.activation, GiftCardActivation::Unknown);
}

#[test]
fn ambiguity_survives_an_unchanged_correction() {
    let mut prior = GiftCardPurchase {
        source_id: "item:1".into(),
        unresolved_fields: vec!["association".into()],
        evidence: vec![GiftCardEvidence {
            field: "association".into(),
            line_index: 4,
            text: "399 DOORDASH2X50".into(),
        }],
        ..Default::default()
    };
    let mut corrected = prior.clone();
    corrected.correct_from(&prior).unwrap();
    assert_eq!(corrected, prior);
    prior.card_count = Some(2);
    prior.denomination_cents = Some(5000);
    prior.face_value_derived = true;
    prior.total_face_value_cents = Some(10000);
    let mut corrected = prior.clone();
    corrected.card_count = Some(3);
    corrected.correct_from(&prior).unwrap();
    assert_eq!(corrected.total_face_value_cents, Some(15000));
    assert!(corrected.corrected_fields.contains(&"card_count".into()));
    assert!(corrected
        .corrected_fields
        .contains(&"total_face_value_cents".into()));
}
#[test]
fn advertisements_loyalty_purchases_and_ordinary_card_references_are_not_redemptions() {
    let text="LCBO\n810 LCBO CARD 50.00\nTotal 50.00\nVISA 50.00\n************1111\nAUTH #:22222\nWIN A GIFT CARD 50.00\n1 OF 2 GIFT CARDS 100.00\nPOINTS BALANCE: 90.30";
    let doc = OcrDocument::from_text(text);
    let lines = text.lines().map(String::from).collect::<Vec<_>>();
    let tenders = crate::fields::extract_tenders(&lines);
    assert_eq!(tenders.len(), 1);
    assert_eq!(tenders[0].kind, "card");
    assert_eq!(extract_redemptions(&doc, "LCBO", &tenders), vec![None]);
}
#[test]
fn malformed_negative_and_partially_read_balances_are_not_repaired() {
    for amount in [
        "1,50.00",
        "-10.00",
        "- 10.00",
        "- $10.00",
        "$ - 10.00",
        "10.00-",
        "10.00 -",
        "1.50.00",
        "9999999999999999999999.00",
    ] {
        for following_row in ["", "\n99.00"] {
            let cards = redemption(&format!("Gift Card 5.00\nBAL: {amount}{following_row}"));
            assert_eq!(cards[0].remaining_balance_cents, None, "{amount}");
            assert!(cards[0]
                .unresolved_fields
                .contains(&"remaining_balance_cents".into()));
        }
    }
    let cards = redemption("Gift Card 5.00\n************1234 EXP:2028/12/31 BAL: 90.30");
    assert_eq!(cards[0].expiry_date.as_deref(), Some("2028/12/31"));
    assert_eq!(cards[0].remaining_balance_cents, Some(9030));
}

#[test]
fn an_unreadable_shop_payment_cannot_donate_metadata_to_the_next_card() {
    for previous_payment in ["Shop Card unreadable", "Shop Card"] {
        for response in [
            "Shop Card Resp: Approved",
            "Shop Card\nResp: Approved",
            "Resp: Approved\nShop Card",
        ] {
            let cards = redemption(&format!(
                "TOTAL 75.00\nAPP#: 111111\nREMAINING BALANCE: $90.00\n{previous_payment}\n************2222\n{response}\nShop Card 25.00"
            ));
            assert_eq!(cards.len(), 1, "{previous_payment}; {response}");
            let card = &cards[0];
            assert_eq!(
                card.normalized_identifier.as_deref(),
                Some("************2222")
            );
            assert_eq!(card.authorization_reference, None);
            assert_eq!(card.remaining_balance_cents, None);
            assert!(card.unresolved_fields.is_empty());
            assert!(card
                .evidence
                .iter()
                .all(|e| !e.text.contains("111111") && !e.text.contains("90.00")));

            // The response annotation belongs to the current authorization
            // block; treating it as a payment boundary would lose this ID/auth.
            let cards = redemption(&format!(
                "TOTAL 75.00\nAPP#: 111111\nREMAINING BALANCE: $90.00\n{previous_payment}\n************2222\nAPP#: 222222\n{response}\nREMAINING BALANCE: $15.00\nShop Card 25.00"
            ));
            let card = &cards[0];
            assert_eq!(
                card.normalized_identifier.as_deref(),
                Some("************2222")
            );
            assert_eq!(card.authorization_reference.as_deref(), Some("222222"));
            assert_eq!(card.remaining_balance_cents, Some(1500));
            assert!(card.unresolved_fields.is_empty());
        }
    }
}

#[test]
fn an_unreadable_neighboring_tender_still_bounds_the_payment_block() {
    let cards=redemption("Gift Card 10.00\n123456xxxxx1112223x\nGift Card unreadable\n123456xxxxx4445556x EXP:NONE\nBAL: 90.00");
    assert_eq!(cards.len(), 1);
    assert_eq!(
        cards[0].printed_identifier.as_deref(),
        Some("123456xxxxx1112223x")
    );
    assert_eq!(cards[0].remaining_balance_cents, None);
    assert_eq!(cards[0].expiry, GiftCardExpiry::Unknown);
}

#[test]
fn same_row_activation_is_preserved_and_conflicting_references_are_unresolved() {
    let doc = OcrDocument::from_text("399 DOORDASH2X50 PC 111111 ACTIVATED 79.99\nSUBTOTAL 79.99");
    let mut items = vec![item("399 DOORDASH2X50")];
    attach_purchases(&doc, "COSTCO", &mut items);
    assert_eq!(
        items[0].gift_card.as_ref().unwrap().reference.as_deref(),
        Some("111111")
    );
    let doc = OcrDocument::from_text(
        "399 DOORDASH2X50 79.99\nPC 111111 PC 222222 ACTIVATED\nSUBTOTAL 79.99",
    );
    attach_purchases(&doc, "COSTCO", &mut items);
    let gift = items[0].gift_card.as_ref().unwrap();
    assert_eq!(gift.reference, None);
    assert!(gift.unresolved_fields.contains(&"reference".into()));
}
