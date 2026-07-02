use crate::receipt_categories;

const SCHEMA_VERSION: &str = "2";
const STAGE_PARSED: &str = "parsed";

#[derive(Clone, Debug)]
pub struct StageRuleLayers {
    pub category_rules: receipt_categories::CategoryRuleLayers,
    pub account_mapping: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct ReceiptItemInput {
    pub description: String,
    pub price: Option<String>,
    pub quantity: i32,
    pub category: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ReceiptWarningInput {
    pub message: String,
    pub after_item_index: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct TenderInput {
    pub amount: String,
    pub account: Option<String>,
    pub kind: String,
    pub raw_label: String,
}

#[derive(Clone, Debug)]
pub struct ReceiptInput {
    pub merchant: String,
    pub date_iso: String,
    pub total: String,
    pub date_is_placeholder: bool,
    pub items: Vec<ReceiptItemInput>,
    pub tax: Option<String>,
    pub subtotal: Option<String>,
    pub raw_text: String,
    pub image_filename: String,
    pub warnings: Vec<ReceiptWarningInput>,
    pub tenders: Vec<TenderInput>,
}

#[derive(Clone, Debug)]
pub struct ClassificationData {
    pub category: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct StructuredWarning {
    pub message: String,
    pub source: String,
    pub stage: String,
}

#[derive(Clone, Debug)]
pub struct BuiltStageItem {
    pub id: String,
    pub description: String,
    pub price: Option<String>,
    pub quantity: i32,
    pub classification: Option<ClassificationData>,
    pub warnings: Vec<StructuredWarning>,
    pub source: String,
}

#[derive(Clone, Debug)]
pub struct BuiltStageMeta {
    pub schema_version: String,
    pub receipt_id: String,
    pub stage: String,
    pub stage_index: i32,
    pub created_at: String,
    pub created_by: String,
    pub pass_name: String,
    pub image_filename: Option<String>,
    pub image_sha256: Option<String>,
    pub ocr_json_path: Option<String>,
}

#[derive(Clone, Debug)]
pub struct BuiltStageReceipt {
    pub merchant: Option<String>,
    pub date: Option<String>,
    pub currency: String,
    pub subtotal: Option<String>,
    pub tax: Option<String>,
    pub total: Option<String>,
}

#[derive(Clone, Debug)]
pub struct BuiltStageTender {
    pub amount: String,
    pub account: Option<String>,
    pub kind: String,
    pub raw_label: String,
}

#[derive(Clone, Debug)]
pub struct BuiltStageDocument {
    pub meta: BuiltStageMeta,
    pub receipt: BuiltStageReceipt,
    pub items: Vec<BuiltStageItem>,
    pub warnings: Vec<StructuredWarning>,
    pub raw_text: Option<String>,
    pub tenders: Vec<BuiltStageTender>,
}

#[derive(Clone, Debug)]
pub struct StageDocumentItemInput {
    pub removed: bool,
    pub description: Option<String>,
    pub price: Option<String>,
    pub quantity: Option<i32>,
    pub classification: Option<ClassificationData>,
    pub warning_messages: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct StageDocumentTenderInput {
    pub amount: Option<String>,
    pub account: Option<String>,
    pub kind: Option<String>,
    pub raw_label: Option<String>,
    pub removed: bool,
}

#[derive(Clone, Debug)]
pub struct StageDocumentInput {
    pub merchant: Option<String>,
    pub date_iso: Option<String>,
    pub total: Option<String>,
    pub tax: Option<String>,
    pub subtotal: Option<String>,
    pub raw_text: String,
    pub image_filename: String,
    pub items: Vec<StageDocumentItemInput>,
    pub top_level_warning_messages: Vec<String>,
    pub tenders: Vec<StageDocumentTenderInput>,
}

#[derive(Clone, Debug)]
pub struct ResolvedReceiptItem {
    pub description: String,
    pub price: String,
    pub quantity: i32,
    pub category: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ResolvedReceiptWarning {
    pub message: String,
    pub after_item_index: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct ResolvedTender {
    pub amount: String,
    pub account: Option<String>,
    pub kind: String,
    pub raw_label: String,
}

#[derive(Clone, Debug)]
pub struct ResolvedReceiptData {
    pub merchant: String,
    pub date_iso: Option<String>,
    pub date_is_placeholder: bool,
    pub total: String,
    pub tax: Option<String>,
    pub subtotal: Option<String>,
    pub raw_text: String,
    pub image_filename: String,
    pub items: Vec<ResolvedReceiptItem>,
    pub warnings: Vec<ResolvedReceiptWarning>,
    pub tenders: Vec<ResolvedTender>,
}

fn legacy_account_alias(target: &str) -> Option<&'static str> {
    match target {
        "Expenses:Food:Vegetable" => Some("Expenses:Food:Grocery:Vegetable"),
        "Expenses:Food:Grocery:Dumolings" => Some("Expenses:Food:Grocery:Frozen:Dumpling"),
        "Expenses:Food:Grocery:Dumplings" => Some("Expenses:Food:Grocery:Frozen:Dumpling"),
        "Expenses:Food:Grocery:Icecream" => Some("Expenses:Food:Grocery:Frozen:IceCream"),
        "Expenses:Food:Grocery:IceCream" => Some("Expenses:Food:Grocery:Frozen:IceCream"),
        _ => None,
    }
}

fn normalize_legacy_account_target(target: &str) -> String {
    legacy_account_alias(target).unwrap_or(target).to_string()
}

fn make_warning(message: &str, source: &str, stage: &str) -> StructuredWarning {
    StructuredWarning {
        message: message.to_string(),
        source: source.to_string(),
        stage: stage.to_string(),
    }
}

fn semantic_category_from_legacy_target(
    target: Option<&str>,
    rule_layers: &StageRuleLayers,
) -> Option<String> {
    let cleaned = target.map(str::trim).filter(|value| !value.is_empty())?;
    if rule_layers
        .account_mapping
        .iter()
        .any(|(key, _)| key == cleaned)
    {
        return Some(cleaned.to_string());
    }
    for (key, account) in &rule_layers.account_mapping {
        if account == cleaned {
            return Some(key.clone());
        }
    }
    None
}

fn resolve_account_target(
    target: Option<&str>,
    rule_layers: &StageRuleLayers,
    default: Option<&str>,
) -> Option<String> {
    match target {
        None => default.map(str::to_string),
        Some(raw) => {
            let cleaned = raw.trim();
            if cleaned.is_empty() {
                return default.map(str::to_string);
            }
            if cleaned.starts_with("Expenses:") {
                return Some(normalize_legacy_account_target(cleaned));
            }
            for (key, mapped) in &rule_layers.account_mapping {
                if key == cleaned {
                    return Some(normalize_legacy_account_target(mapped));
                }
            }
            default.map(str::to_string)
        }
    }
}

pub fn classify_item_semantic(
    description: &str,
    rule_layers: &StageRuleLayers,
    default_category: Option<String>,
) -> Option<ClassificationData> {
    let category = receipt_categories::classify_item_key(
        description,
        &rule_layers.category_rules,
        default_category,
    );
    let tags = receipt_categories::classify_item_tags(description, &rule_layers.category_rules);
    if category.is_none() && tags.is_empty() {
        return None;
    }
    Some(ClassificationData { category, tags })
}

pub fn build_parsed_receipt_stage(
    receipt: &ReceiptInput,
    rule_layers: &StageRuleLayers,
    receipt_id: &str,
    created_at: &str,
    ocr_json_path: Option<String>,
    image_sha256: Option<String>,
    created_by: &str,
    pass_name: &str,
) -> BuiltStageDocument {
    let mut item_docs = Vec::with_capacity(receipt.items.len());
    let mut top_level_warnings = Vec::new();

    for (idx, item) in receipt.items.iter().enumerate() {
        let semantic_category =
            semantic_category_from_legacy_target(item.category.as_deref(), rule_layers);
        item_docs.push(BuiltStageItem {
            id: format!("item-{:04}", idx + 1),
            description: item.description.clone(),
            price: item.price.clone(),
            quantity: item.quantity,
            classification: classify_item_semantic(
                &item.description,
                rule_layers,
                semantic_category,
            ),
            warnings: Vec::new(),
            source: "parser".to_string(),
        });
    }

    for warning in &receipt.warnings {
        let structured = make_warning(&warning.message, "parser", STAGE_PARSED);
        if let Some(index) = warning.after_item_index {
            if index < item_docs.len() {
                item_docs[index].warnings.push(structured);
                continue;
            }
        }
        top_level_warnings.push(structured);
    }

    let tenders = receipt
        .tenders
        .iter()
        .map(|tender| BuiltStageTender {
            amount: tender.amount.clone(),
            account: tender.account.clone(),
            kind: tender.kind.clone(),
            raw_label: tender.raw_label.clone(),
        })
        .collect();

    BuiltStageDocument {
        meta: BuiltStageMeta {
            schema_version: SCHEMA_VERSION.to_string(),
            receipt_id: receipt_id.to_string(),
            stage: STAGE_PARSED.to_string(),
            stage_index: 0,
            created_at: created_at.to_string(),
            created_by: created_by.to_string(),
            pass_name: pass_name.to_string(),
            image_filename: (!receipt.image_filename.is_empty())
                .then(|| receipt.image_filename.clone()),
            image_sha256,
            ocr_json_path,
        },
        receipt: BuiltStageReceipt {
            merchant: (!receipt.merchant.is_empty()).then(|| receipt.merchant.clone()),
            date: if receipt.date_is_placeholder {
                None
            } else {
                Some(receipt.date_iso.clone())
            },
            currency: "CAD".to_string(),
            subtotal: receipt.subtotal.clone(),
            tax: receipt.tax.clone(),
            total: Some(receipt.total.clone()),
        },
        items: item_docs,
        warnings: top_level_warnings,
        raw_text: (!receipt.raw_text.is_empty()).then(|| receipt.raw_text.clone()),
        tenders,
    }
}

pub fn get_stage_summary(
    document: &StageDocumentInput,
) -> (Option<String>, Option<String>, Option<String>) {
    (
        document.merchant.clone(),
        document.date_iso.clone(),
        document.total.clone(),
    )
}

pub fn account_from_classification(
    classification: Option<&ClassificationData>,
    rule_layers: &StageRuleLayers,
) -> Option<String> {
    let classification = classification?;

    if let Some(category) = classification.category.as_deref() {
        if let Some(mapped) = resolve_account_target(Some(category), rule_layers, None) {
            return Some(mapped);
        }
    }

    for tag in &classification.tags {
        if tag.is_empty() {
            continue;
        }
        for (key, mapped) in &rule_layers.account_mapping {
            if key.split('_').any(|part| part == tag) {
                return Some(normalize_legacy_account_target(mapped));
            }
        }
    }

    None
}

pub fn resolve_stage_document(
    document: &StageDocumentInput,
    rule_layers: &StageRuleLayers,
) -> ResolvedReceiptData {
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut active_item_index: isize = -1;

    for item in &document.items {
        if item.removed {
            continue;
        }

        let description = item
            .description
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("UNKNOWN_ITEM")
            .to_string();
        let price = item.price.clone().unwrap_or_else(|| "0".to_string());
        let quantity = item.quantity.unwrap_or(1);
        let category = account_from_classification(item.classification.as_ref(), rule_layers);

        items.push(ResolvedReceiptItem {
            description,
            price,
            quantity,
            category,
        });
        active_item_index += 1;

        for message in &item.warning_messages {
            warnings.push(ResolvedReceiptWarning {
                message: message.clone(),
                after_item_index: Some(active_item_index as usize),
            });
        }
    }

    for message in &document.top_level_warning_messages {
        warnings.push(ResolvedReceiptWarning {
            message: message.clone(),
            after_item_index: None,
        });
    }

    let tenders = document
        .tenders
        .iter()
        .filter(|tender| !tender.removed)
        .map(|tender| ResolvedTender {
            amount: tender.amount.clone().unwrap_or_else(|| "0".to_string()),
            account: tender
                .account
                .clone()
                .filter(|value| !value.trim().is_empty()),
            kind: tender
                .kind
                .clone()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "card".to_string()),
            raw_label: tender.raw_label.clone().unwrap_or_default(),
        })
        .collect();

    ResolvedReceiptData {
        merchant: document
            .merchant
            .clone()
            .unwrap_or_else(|| "UNKNOWN_MERCHANT".to_string()),
        date_iso: document.date_iso.clone(),
        date_is_placeholder: document.date_iso.is_none(),
        total: document.total.clone().unwrap_or_else(|| "0".to_string()),
        tax: document.tax.clone(),
        subtotal: document.subtotal.clone(),
        raw_text: document.raw_text.clone(),
        image_filename: document.image_filename.clone(),
        items,
        warnings,
        tenders,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stage layers over the bundled PUBLIC rules (no project/private overrides).
    /// The desktop tests use `load_item_category_rule_layers()`; here the public
    /// classifier + default account mapping is the equivalent stable input.
    fn public_stage_layers() -> StageRuleLayers {
        let parser = crate::rules::default_parser_rule_layers();
        StageRuleLayers {
            category_rules: parser.category_rules,
            account_mapping: parser.account_mapping,
        }
    }

    fn item(description: &str, price: &str, quantity: i32, category: Option<&str>) -> ReceiptItemInput {
        ReceiptItemInput {
            description: description.to_string(),
            price: Some(price.to_string()),
            quantity,
            category: category.map(str::to_string),
        }
    }

    /// `build_parsed_receipt_stage`: schema/meta shape, sequential item ids,
    /// classification of a known item, warning routing (item vs top-level), and
    /// tender pass-through. Mirrors the build-side assertions of the desktop
    /// `test_receipt_staged_json.py` round-trip tests.
    #[test]
    fn build_parsed_stage_sets_meta_ids_warnings_and_tenders() {
        let layers = public_stage_layers();
        let receipt = ReceiptInput {
            merchant: "COSTCO".to_string(),
            date_iso: "2026-03-07".to_string(),
            total: "466.68".to_string(),
            date_is_placeholder: false,
            items: vec![
                item("COORS LIGHT", "13.99", 1, None),  // classifies (alcoholic)
                item("ZZUNKNOWNQ", "2.00", 3, None),    // no classifier match
            ],
            tax: Some("5.72".to_string()),
            subtotal: Some("455.00".to_string()),
            raw_text: "COSTCO\nTOTAL 466.68".to_string(),
            image_filename: "costco.jpg".to_string(),
            warnings: vec![
                ReceiptWarningInput { message: "on item 0".to_string(), after_item_index: Some(0) },
                ReceiptWarningInput { message: "out of range".to_string(), after_item_index: Some(99) },
                ReceiptWarningInput { message: "no index".to_string(), after_item_index: None },
            ],
            tenders: vec![
                TenderInput {
                    amount: "441.68".to_string(),
                    account: None,
                    kind: "card".to_string(),
                    raw_label: "MasterCard".to_string(),
                },
                TenderInput {
                    amount: "25.00".to_string(),
                    account: None,
                    kind: "gift_card".to_string(),
                    raw_label: "Shop Card".to_string(),
                },
            ],
        };

        let doc = build_parsed_receipt_stage(
            &receipt,
            &layers,
            "receipt-123",
            "2026-03-07T00:00:00Z",
            Some("path/to.ocr.json".to_string()),
            Some("deadbeef".to_string()),
            "unit-test",
            "parsed-pass",
        );

        // meta
        assert_eq!(doc.meta.schema_version, "2");
        assert_eq!(doc.meta.stage, "parsed");
        assert_eq!(doc.meta.stage_index, 0);
        assert_eq!(doc.meta.receipt_id, "receipt-123");
        assert_eq!(doc.meta.created_by, "unit-test");
        assert_eq!(doc.meta.pass_name, "parsed-pass");
        assert_eq!(doc.meta.image_filename.as_deref(), Some("costco.jpg"));
        assert_eq!(doc.meta.image_sha256.as_deref(), Some("deadbeef"));
        assert_eq!(doc.meta.ocr_json_path.as_deref(), Some("path/to.ocr.json"));

        // receipt header
        assert_eq!(doc.receipt.merchant.as_deref(), Some("COSTCO"));
        assert_eq!(doc.receipt.currency, "CAD");
        assert_eq!(doc.receipt.date.as_deref(), Some("2026-03-07"));
        assert_eq!(doc.receipt.total.as_deref(), Some("466.68"));
        assert_eq!(doc.receipt.tax.as_deref(), Some("5.72"));
        assert_eq!(doc.receipt.subtotal.as_deref(), Some("455.00"));

        // items: sequential ids, classification only for the matching keyword
        assert_eq!(doc.items.len(), 2);
        assert_eq!(doc.items[0].id, "item-0001");
        assert_eq!(doc.items[1].id, "item-0002");
        assert_eq!(doc.items[1].quantity, 3);
        assert!(doc.items[0].classification.is_some(), "COORS LIGHT should classify");
        assert!(doc.items[1].classification.is_none(), "gibberish should not classify");

        // warnings: in-range attaches to the item; out-of-range and index-less go top-level
        assert_eq!(doc.items[0].warnings.len(), 1);
        assert_eq!(doc.items[0].warnings[0].message, "on item 0");
        let top: Vec<&str> = doc.warnings.iter().map(|w| w.message.as_str()).collect();
        assert_eq!(top, vec!["out of range", "no index"]);

        // tenders pass through unchanged, account still unassigned
        assert_eq!(doc.tenders.len(), 2);
        assert_eq!(doc.tenders[0].amount, "441.68");
        assert_eq!(doc.tenders[0].kind, "card");
        assert_eq!(doc.tenders[0].account, None);
        assert_eq!(doc.tenders[1].kind, "gift_card");
    }

    /// Placeholder dates are dropped from the built stage receipt.
    #[test]
    fn build_parsed_stage_omits_placeholder_date() {
        let layers = public_stage_layers();
        let receipt = ReceiptInput {
            merchant: "TEST".to_string(),
            date_iso: "2026-01-01".to_string(),
            total: "1.00".to_string(),
            date_is_placeholder: true,
            items: vec![],
            tax: None,
            subtotal: None,
            raw_text: String::new(),
            image_filename: String::new(),
            warnings: vec![],
            tenders: vec![],
        };
        let doc = build_parsed_receipt_stage(&receipt, &layers, "r", "t", None, None, "b", "p");
        assert_eq!(doc.receipt.date, None);
        // Empty image filename => no meta image filename.
        assert_eq!(doc.meta.image_filename, None);
    }

    fn doc_item(
        removed: bool,
        description: Option<&str>,
        price: Option<&str>,
        quantity: Option<i32>,
        classification: Option<ClassificationData>,
        warnings: &[&str],
    ) -> StageDocumentItemInput {
        StageDocumentItemInput {
            removed,
            description: description.map(str::to_string),
            price: price.map(str::to_string),
            quantity,
            classification,
            warning_messages: warnings.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// `resolve_stage_document`: removed items are dropped, defaults fill missing
    /// fields, the explicit-category classification resolves to its account, and
    /// warnings are re-indexed against the surviving items (mirrors the desktop
    /// `test_receipt_stage_resolves_review_overrides_and_removed_items` +
    /// `test_tender_review_patch_overrides_account`).
    #[test]
    fn resolve_stage_document_applies_removals_defaults_and_reindexes() {
        let layers = public_stage_layers();
        let document = StageDocumentInput {
            merchant: Some("NO FRILLS".to_string()),
            date_iso: Some("2026-03-03".to_string()),
            total: Some("46.56".to_string()),
            tax: Some("5.36".to_string()),
            subtotal: Some("41.20".to_string()),
            raw_text: "NO FRILLS".to_string(),
            image_filename: "nofrills.jpg".to_string(),
            items: vec![
                doc_item(
                    false,
                    Some("  Napa cabbage  "),
                    Some("3.99"),
                    Some(1),
                    Some(ClassificationData {
                        category: Some("grocery_vegetable".to_string()),
                        tags: vec![],
                    }),
                    &["parser warning"],
                ),
                doc_item(true, Some("Milk"), Some("4.99"), Some(1), None, &[]), // removed
                doc_item(false, Some("   "), None, None, None, &[]),            // defaults
            ],
            top_level_warning_messages: vec!["top warn".to_string()],
            tenders: vec![
                StageDocumentTenderInput {
                    amount: Some("25.00".to_string()),
                    account: Some("Assets:GiftCards:Costco".to_string()),
                    kind: Some("gift_card".to_string()),
                    raw_label: Some("Shop".to_string()),
                    removed: false,
                },
                StageDocumentTenderInput {
                    amount: Some("441.68".to_string()),
                    account: None,
                    kind: Some("card".to_string()),
                    raw_label: Some("MC".to_string()),
                    removed: true, // dropped
                },
                StageDocumentTenderInput {
                    amount: None,
                    account: Some("   ".to_string()),
                    kind: None,
                    raw_label: None,
                    removed: false,
                },
            ],
        };

        let resolved = resolve_stage_document(&document, &layers);

        assert_eq!(resolved.merchant, "NO FRILLS");
        assert!(!resolved.date_is_placeholder);

        // removed item dropped; empty description -> UNKNOWN_ITEM; description trimmed
        let descs: Vec<&str> = resolved.items.iter().map(|i| i.description.as_str()).collect();
        assert_eq!(descs, vec!["Napa cabbage", "UNKNOWN_ITEM"]);
        assert_eq!(resolved.items[0].price, "3.99");
        assert_eq!(resolved.items[0].quantity, 1);
        assert_eq!(resolved.items[0].category.as_deref(), Some("Expenses:Food:Grocery:Vegetable"));
        // missing price/quantity fall back to "0"/1; no classification -> no category
        assert_eq!(resolved.items[1].price, "0");
        assert_eq!(resolved.items[1].quantity, 1);
        assert_eq!(resolved.items[1].category, None);

        // warnings re-indexed to surviving items, then top-level appended
        assert_eq!(resolved.warnings.len(), 2);
        assert_eq!(resolved.warnings[0].message, "parser warning");
        assert_eq!(resolved.warnings[0].after_item_index, Some(0));
        assert_eq!(resolved.warnings[1].message, "top warn");
        assert_eq!(resolved.warnings[1].after_item_index, None);

        // tenders: removed dropped; account override kept; blank account -> None; kind default
        assert_eq!(resolved.tenders.len(), 2);
        assert_eq!(resolved.tenders[0].account.as_deref(), Some("Assets:GiftCards:Costco"));
        assert_eq!(resolved.tenders[0].kind, "gift_card");
        assert_eq!(resolved.tenders[1].amount, "0");
        assert_eq!(resolved.tenders[1].account, None);
        assert_eq!(resolved.tenders[1].kind, "card");
        assert_eq!(resolved.tenders[1].raw_label, "");
    }

    /// A schema-1 style document (no tenders) resolves to an empty tender list.
    #[test]
    fn resolve_stage_document_without_tenders_is_empty() {
        let layers = public_stage_layers();
        let document = StageDocumentInput {
            merchant: Some("TEST".to_string()),
            date_iso: None, // placeholder
            total: Some("10.00".to_string()),
            tax: None,
            subtotal: None,
            raw_text: String::new(),
            image_filename: "legacy.jpg".to_string(),
            items: vec![],
            top_level_warning_messages: vec![],
            tenders: vec![],
        };
        let resolved = resolve_stage_document(&document, &layers);
        assert!(resolved.tenders.is_empty());
        assert!(resolved.date_is_placeholder);
    }

    /// `account_from_classification`: explicit category key, legacy `Expenses:`
    /// alias normalization, a uniquely-matching tag, and the empty case.
    #[test]
    fn account_from_classification_resolves_key_alias_and_tag() {
        let layers = public_stage_layers();

        // explicit internal key -> mapped account
        let by_key = account_from_classification(
            Some(&ClassificationData { category: Some("grocery_dairy".to_string()), tags: vec![] }),
            &layers,
        );
        assert_eq!(by_key.as_deref(), Some("Expenses:Food:Grocery:Dairy"));

        // legacy full account is normalized through the alias table
        let by_alias = account_from_classification(
            Some(&ClassificationData {
                category: Some("Expenses:Food:Grocery:Icecream".to_string()),
                tags: vec![],
            }),
            &layers,
        );
        assert_eq!(by_alias.as_deref(), Some("Expenses:Food:Grocery:Frozen:IceCream"));

        // no category, but a tag that is a unique key-part ("dairy" only in grocery_dairy)
        let by_tag = account_from_classification(
            Some(&ClassificationData { category: None, tags: vec!["dairy".to_string()] }),
            &layers,
        );
        assert_eq!(by_tag.as_deref(), Some("Expenses:Food:Grocery:Dairy"));

        // nothing to resolve
        assert_eq!(account_from_classification(None, &layers), None);
    }

    /// `classify_item_semantic`: a known keyword classifies; gibberish yields
    /// nothing unless a default is supplied.
    #[test]
    fn classify_item_semantic_matches_keyword_else_default() {
        let layers = public_stage_layers();
        let hit = classify_item_semantic("COORS LIGHT", &layers, None);
        assert!(hit.is_some_and(|c| c.category.is_some()), "COORS LIGHT should classify");
        assert!(classify_item_semantic("ZZUNKNOWNQ", &layers, None).is_none());
        let defaulted = classify_item_semantic("ZZUNKNOWNQ", &layers, Some("grocery_dairy".to_string()));
        assert_eq!(
            defaulted.and_then(|c| c.category).as_deref(),
            Some("grocery_dairy")
        );
    }
}
