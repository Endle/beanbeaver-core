//! Public, synthetic end-to-end contracts. No private receipt identifiers.
use receipt_core::date::Date;
use receipt_core::gift_cards::{GiftCardActivation, GiftCardRedemption};
use receipt_core::ocr_transform::{RawDetection, RawDetectionPage};
use receipt_core::parser::ParsedReceiptData;
use receipt_core::process::{
    process_receipt, reformat_parsed_receipt, ItemCorrection, ReceiptCorrections,
};

fn scan(text: &str) -> ParsedReceiptData {
    let detections = text
        .lines()
        .enumerate()
        .map(|(i, text)| {
            let y = 80.0 + i as f64 * 35.0;
            RawDetection {
                text: text.into(),
                confidence: 0.99,
                points: vec![(80.0, y), (600.0, y), (600.0, y + 20.0), (80.0, y + 20.0)],
            }
        })
        .collect();
    let page = RawDetectionPage::try_new(detections, 800, 1600, 50).unwrap();
    process_receipt(
        page,
        "synthetic.jpg",
        None,
        Date::new(2026, 9, 9).unwrap(),
        "Liabilities:Card",
        "CAD",
        "Expenses:Tax",
        None,
    )
    .parsed
}
fn edit(
    parsed: &ParsedReceiptData,
    changes: ReceiptCorrections,
) -> Result<receipt_core::process::ProcessedReceipt, String> {
    reformat_parsed_receipt(
        parsed,
        Date::new(2026, 9, 9).unwrap(),
        "Liabilities:Card",
        "USD",
        "Expenses:Tax",
        None,
        &changes,
        None,
    )
}
fn items(parsed: &ParsedReceiptData) -> Vec<ItemCorrection> {
    parsed
        .items
        .iter()
        .map(|i| ItemCorrection {
            gift_card: i.gift_card.clone(),
            description: i.description.clone(),
            item_number: i.item_number.clone(),
            price: i.price.to_string(),
            quantity: i.quantity,
            tag_path: String::new(),
        })
        .collect()
}

#[test]
fn scan_context_and_metadata_survive_serialization_and_reformatting() {
    let parsed=scan("LCBO\nBOTTLE 59.70\nTOTAL 59.70\nGift Card 50.00\n123456xxxxx9876543x EXP:NONE\nAUTHOR.#:123456 BAL:0.00\nGift Card 9.70\n123456xxxxx1112223x EXP:NONE\nAUTHOR.#:789012 BAL:90.30");
    assert_eq!(parsed.tenders.len(), 2);
    let g = parsed.tenders[1].gift_card.as_ref().unwrap();
    assert_eq!(g.currency.as_deref(), Some("CAD"));
    assert_eq!(g.remaining_balance_cents, Some(9030));
    let decoded: GiftCardRedemption =
        serde_json::from_str(&serde_json::to_string(g).unwrap()).unwrap();
    assert_eq!(decoded, *g);
    let reformatted = edit(&parsed, ReceiptCorrections::default()).unwrap();
    assert_eq!(reformatted.parsed.tenders[1].gift_card.as_ref(), Some(g));
    // Reformat's export currency must not reinterpret a historical observation.
    assert_eq!(
        reformatted.parsed.tenders[1]
            .gift_card
            .as_ref()
            .unwrap()
            .currency
            .as_deref(),
        Some("CAD")
    );
    let mut without_metadata = parsed.clone();
    for t in &mut without_metadata.tenders {
        t.gift_card = None;
    }
    assert_eq!(
        reformatted.beancount,
        edit(&without_metadata, ReceiptCorrections::default())
            .unwrap()
            .beancount
    );
}

/// Dollarama prints a gift card as a zero-priced SKU, a load row that names its
/// own amount, the card serial, and the terminal's activation slip — all
/// *before* TOTAL. The slip's `Amount` row is a priced line to a text parser,
/// so the one card used to come out as three items summing to twice the total.
/// Identifiers here are synthetic.
#[test]
fn dollarama_gift_card_is_one_item_charged_on_its_load_row() {
    let parsed = scan(
        "DOLLARAMA\nAMAZON.CA $25 01234567890 0.00\nBH 25 25.00\n6300000000000000\nTRANSACTION RECORD\nAccount GIFT CARD\nTrans Type ACTIVATE\nAmount $25.00\nReference # 000000000000\nApproved\n*** CUSTOMER COPY ***\nTOTAL $25.00\nMASTERCARD $25.00",
    );
    let items: Vec<_> = parsed
        .items
        .iter()
        .map(|i| (i.description.as_str(), i.price.to_string()))
        .collect();
    assert_eq!(
        items,
        vec![("AMAZON.CA $25 01234567890", "25.00".to_string())]
    );
    assert!(
        parsed.items[0].tags.iter().any(|t| t == "gift_card"),
        "tags: {:?}",
        parsed.items[0].tags
    );
    // The terminal's activation slip describes the card it activated.
    let gift = parsed.items[0]
        .gift_card
        .as_ref()
        .expect("purchase metadata");
    assert_eq!(gift.activation, GiftCardActivation::Activated);
    assert_eq!(gift.reference.as_deref(), Some("000000000000"));
    assert_eq!(gift.denomination_cents, Some(2500));
    let kinds: Vec<_> = parsed.warnings.iter().map(|w| w.kind).collect();
    assert!(
        kinds.contains(&receipt_core::common::ReceiptWarningKind::PriceAutoCorrected),
        "the fold must be auditable: {kinds:?}"
    );
    assert!(
        !kinds.contains(&receipt_core::common::ReceiptWarningKind::TotalMismatch),
        "{kinds:?}"
    );
}

/// The variable-denomination twin: the load row can only name the range the
/// card accepts, which the card itself prints. Identifiers here are synthetic.
#[test]
fn dollarama_variable_gift_card_is_one_item_charged_on_its_load_row() {
    let parsed = scan(
        "DOLLARAMA\nPNGO 25-500CAD 01234567890 0.00\nVariable 25-500 25.00\n6000000000000000000\nTRANSACTION RECORD\nAccount : GIFT CARD\nTrans Type ACTIVATE\nAmount $25.00\nReference # 000000000000\nApproved\n*** CUSTOMER COPY ***\nTOTAL $25.00\nMASTERCARD $25.00",
    );
    let items: Vec<_> = parsed
        .items
        .iter()
        .map(|i| (i.description.as_str(), i.price.to_string()))
        .collect();
    assert_eq!(
        items,
        vec![("PNGO 25-500CAD 01234567890", "25.00".to_string())]
    );
    assert!(
        parsed.items[0].tags.iter().any(|t| t == "gift_card"),
        "tags: {:?}",
        parsed.items[0].tags
    );
    // The terminal's activation slip describes the card it activated.
    let gift = parsed.items[0]
        .gift_card
        .as_ref()
        .expect("purchase metadata");
    assert_eq!(gift.activation, GiftCardActivation::Activated);
    assert_eq!(gift.reference.as_deref(), Some("000000000000"));
    assert_eq!(gift.denomination_cents, Some(2500));
    let kinds: Vec<_> = parsed.warnings.iter().map(|w| w.kind).collect();
    assert!(
        kinds.contains(&receipt_core::common::ReceiptWarningKind::PriceAutoCorrected),
        "the fold must be auditable: {kinds:?}"
    );
    assert!(
        !kinds.contains(&receipt_core::common::ReceiptWarningKind::UncategorizedItem),
        "{kinds:?}"
    );
}

#[test]
fn identical_purchases_reorder_without_merging_or_losing_unresolved_evidence() {
    let parsed=scan("COSTCO\n399 DOORDASH2X50 79.99\nPC 111111 ACTIVATED\n399 DOORDASH2X50 79.99\nPC unreadable ACTIVATED\nSUBTOTAL 159.98\nTOTAL 159.98\nMASTERCARD 159.98");
    assert_eq!(parsed.items.len(), 2);
    let first = parsed.items[0].gift_card.as_ref().unwrap();
    let second = parsed.items[1].gift_card.as_ref().unwrap();
    assert_eq!(first.activation, GiftCardActivation::Activated);
    assert_eq!(first.reference.as_deref(), Some("111111"));
    assert_eq!(second.reference, None);
    assert!(second.unresolved_fields.contains(&"reference".into()));
    let mut edits = items(&parsed);
    edits.reverse();
    edits[1].description = "Renamed first pack".into();
    let changed = edit(
        &parsed,
        ReceiptCorrections {
            items: Some(edits),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(changed.parsed.items[0].gift_card.as_ref(), Some(second));
    assert_eq!(changed.parsed.items[1].gift_card.as_ref(), Some(first));
}

#[test]
fn stale_and_duplicated_purchase_sources_are_rejected() {
    let parsed =
        scan("COSTCO\n810 LCBO CARD 400.00\nPC 123456 ACTIVATED\nTOTAL 400.00\nMASTERCARD 400.00");
    assert_eq!(parsed.items.len(), 1);
    let mut stale = items(&parsed);
    stale[0].gift_card.as_mut().unwrap().source_id = "item:99".into();
    assert!(edit(
        &parsed,
        ReceiptCorrections {
            items: Some(stale),
            ..Default::default()
        }
    )
    .is_err());
    // Even the same ordinal in a new OCR document is a different source.
    let rescanned =
        scan("COSTCO\n810 LCBO CARD 400.00\nPC 999999 ACTIVATED\nTOTAL 400.00\nMASTERCARD 400.00");
    assert!(edit(
        &rescanned,
        ReceiptCorrections {
            items: Some(items(&parsed)),
            ..Default::default()
        }
    )
    .is_err());
    let original = items(&parsed);
    let duplicated = vec![original[0].clone(), original[0].clone()];
    assert!(edit(
        &parsed,
        ReceiptCorrections {
            items: Some(duplicated),
            ..Default::default()
        }
    )
    .is_err());
}

#[test]
fn correcting_total_recomputes_tender_mismatch_without_changing_payment() {
    use receipt_core::common::ReceiptWarningKind;
    let parsed = scan(
        "LCBO\nBOTTLE 10.00\nTOTAL 10.00\nGift Card 9.00\n123456xxxxx9876543x EXP:NONE\nBAL: 91.00",
    );
    assert!(parsed
        .warnings
        .iter()
        .any(|w| w.kind == ReceiptWarningKind::TenderMismatch));
    let fixed = edit(
        &parsed,
        ReceiptCorrections {
            total: Some("9.00".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!fixed
        .parsed
        .warnings
        .iter()
        .any(|w| w.kind == ReceiptWarningKind::TenderMismatch));
    assert_eq!(fixed.parsed.tenders[0].amount.to_string(), "9.00");
    assert_eq!(
        fixed.parsed.tenders[0].gift_card,
        parsed.tenders[0].gift_card
    );
}

#[test]
fn centered_tax_registration_does_not_rename_a_zero_priced_gift_card() {
    // Synthetic geometry: a centered registration overlaps the first product.
    // Keep the zero-price SKU valid so the existing variable-load fold can use it.
    let rows = [
        ("DOLLARAMA", 170.0, 140.0, 1000.0, 285.0),
        ("123 Example Road", 270.0, 295.0, 850.0, 370.0),
        ("Example ON A1A 1A1", 345.0, 345.0, 800.0, 415.0),
        ("HST 123456789", 395.0, 395.0, 725.0, 460.0),
        ("APPLE GC VAR.", 50.0, 425.0, 380.0, 490.0),
        ("00000000000", 570.0, 450.0, 850.0, 510.0),
        ("0.00", 905.0, 460.0, 1020.0, 510.0),
        ("Variable 10-500", 50.0, 475.0, 430.0, 540.0),
        ("30.00", 885.0, 505.0, 1020.0, 560.0),
        ("TOTAL $30.00", 50.0, 700.0, 1020.0, 750.0),
    ];
    let detections = rows
        .into_iter()
        .map(|(text, x0, y0, x1, y1)| RawDetection {
            text: text.into(),
            confidence: 0.99,
            points: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)],
        })
        .collect();
    let page = RawDetectionPage::try_new(detections, 1150, 1000, 0).unwrap();
    let parsed = process_receipt(
        page,
        "synthetic.jpg",
        None,
        Date::new(2026, 10, 1).unwrap(),
        "Liabilities:Card",
        "CAD",
        "Expenses:Tax",
        None,
    )
    .parsed;
    assert_eq!(parsed.items.len(), 1, "{:?}", parsed.items);
    assert!(parsed.items[0].description.starts_with("APPLE GC VAR."));
    assert!(!parsed.items[0].description.contains("HST"));
    assert_eq!(parsed.items[0].price.to_string(), "30.00");
    assert_eq!(parsed.total.to_string(), "30.00");
}
