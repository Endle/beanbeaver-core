use super::*;
use crate::merchant_vocab::{Expansion, MerchantVocab};
use crate::rules::RuleBook;

fn scanned(description: &str) -> (ItemClassification, Vec<RuleMatch>) {
    classify_receipt_description(
        description,
        &RuleBook::bundled().layers().category_rules,
        None,
    )
}

#[test]
fn prefix_recovers_only_a_proper_word_anchored_tail() {
    let layers = &RuleBook::bundled().layers().category_rules;
    let (item, matches) = scanned("Example - Wild Hazel Mush");
    assert_eq!(
        item.account.as_deref(),
        Some("Expenses:Food:Grocery:Vegetable")
    );
    assert_eq!(item.tag_path.as_deref(), Some("grocery/vegetable"));
    assert_eq!(item.tags, ["grocery", "grocery/vegetable"]);
    assert!(matches.iter().all(|m| m.is_prefix && !m.is_exact));
    assert_eq!(matches[0].matched_keyword, "MUSHROOM");
    for description in [
        "Example - Wild Hazel Xmush",     // cannot start within a word
        "Example - Wild Hazel Mush 180g", // cannot drop trailing size
        "Example - Wild Hazel Mushroom",  // equality is not a prefix
        "Hazel Mush",                     // not a fixed-width cut
        "BABY DUCK",                      // wine, not a truncated meat name
        "Example - Super Slim- Hea",      // three letters are insufficient
    ] {
        assert!(
            prefix_fallback_matches(description, layers).is_empty(),
            "{description}"
        );
    }
    assert_eq!(scanned("Example - Wild Hazel Mush...").0, item);
    let (split, _) = scanned("Example - Garden Shepherd' s");
    assert_eq!(split.tag_path.as_deref(), Some("grocery/vegetable"));
}

#[test]
fn prefix_length_counts_characters_before_brand_masking() {
    let mut layers = RuleBook::bundled().layers().category_rules.clone();
    layers.brands.push("Example Brand".into());
    assert!(!prefix_fallback_matches("Example Brand -Z Mush", &layers)
        .iter()
        .any(|m| m.is_prefix)); // 21
    assert!(!prefix_fallback_matches("Example Brand - Z Mush", &layers).is_empty()); // 22
    assert!(prefix_fallback_matches("éééééééééééééééé Mush", &layers).is_empty()); // 21 chars, 37 bytes
    layers.brands.push("Hazel Mush".into());
    assert!(prefix_fallback_matches("Example Brand - Hazel Mush", &layers).is_empty());
}

#[test]
fn prefix_abstains_on_account_rivals_including_exact_only() {
    for description in [
        "Example - Golden Label Oyst",
        "Example Carnaby, Sweet",
        "2 X CARNABY, SWEET",
    ] {
        let (item, _) = scanned(description);
        assert!(item.account.is_none(), "{description}: {item:?}");
    }
    let book = RuleBook::with_overrides(&[r#"
[[rules]]
keywords = ["ZYXWVUTSRQP"]
tags = ["grocery/vegetable"]
exact_only = true
"#])
    .unwrap();
    assert!(
        prefix_fallback_matches("Example brand - ZYXW", &book.layers().category_rules).is_empty()
    );
    // Make the line long enough too: the exact-only check must be exercised.
    assert!(
        prefix_fallback_matches("Example long brand - ZYXW", &book.layers().category_rules)
            .is_empty()
    );
}

#[test]
fn prefix_does_not_touch_existing_accounts_tags_or_labels() {
    let layers = &RuleBook::bundled().layers().category_rules;
    for description in [
        "Example - Coconut Water",
        "Example - Green Pepper",
        "Example - CocaCola Zero Can",
        "Example - Fresh Navel Orange",
        "Example - Frozen Unknown Mush",
    ] {
        let before = classify_item(description, layers);
        assert!(before.account.is_some(), "{description}");
        let (after, matches) = scanned(description);
        assert_eq!(before, after, "{description}");
        assert!(matches.iter().all(|m| !m.is_prefix));
    }
}

#[test]
fn prefix_account_consensus_uses_resolved_accounts_and_keeps_rule_order() {
    let book = RuleBook::with_overrides(&[r#"
[accounts]
"grocery/snacks" = "Expenses:Food:Grocery:Vegetable"
[[rules]]
keywords = ["ZYXWAAA"]
tags = ["grocery/vegetable"]
[[rules]]
keywords = ["ZYXWBBB"]
tags = ["grocery/snacks"]
"#])
    .unwrap();
    let (item, matches) = classify_receipt_description(
        "Example long brand - ZYXW",
        &book.layers().category_rules,
        None,
    );
    assert_eq!(
        item.account.as_deref(),
        Some("Expenses:Food:Grocery:Vegetable")
    );
    assert_eq!(
        item.tags,
        ["grocery", "grocery/vegetable", "grocery/snacks"]
    );
    assert_eq!(matches.len(), 2);
    assert!(matches[0].rule_index < matches[1].rule_index);
}

#[test]
fn prefix_honors_literal_subtraction_and_does_not_revive_disabled_rules() {
    let book = RuleBook::with_overrides(&[r#"
[[rules]]
id = "synthetic_prefix"
keywords = ["ZYXWAAA"]
tags = ["grocery/vegetable"]
[[rules]]
keywords = ["EXAMPLE"]
tags = ["grocery"]
disables = ["synthetic_prefix"]
"#])
    .unwrap();
    let (item, _) = classify_receipt_description(
        "Example long brand - ZYXW",
        &book.layers().category_rules,
        None,
    );
    assert!(item.account.is_none());
    assert_eq!(item.tags, ["grocery"]);
    let book = RuleBook::with_overrides(&[r#"
[[rules]]
keywords = ["EXAMPLE"]
tags = ["grocery"]
remove_tags = ["grocery/vegetable"]
"#])
    .unwrap();
    let (item, _) = classify_receipt_description(
        "Example - Wild Hazel Mush",
        &book.layers().category_rules,
        None,
    );
    assert!(item.account.is_none());
    assert_eq!(item.tags, ["grocery"]);
}

#[test]
fn prefix_runs_after_merchant_vocabulary_and_on_original_text_only() {
    let layers = &RuleBook::bundled().layers().category_rules;
    let vocab = MerchantVocab {
        canonical: "EXAMPLE".into(),
        expansions: HashMap::from([(
            "MUSH".into(),
            Expansion {
                full: "Milk".into(),
                classify: true,
            },
        )]),
    };
    let (item, matches) =
        classify_receipt_description("Example - Wild Hazel Mush", layers, Some(&vocab));
    assert_eq!(item.tag_path.as_deref(), Some("grocery/dairy"));
    assert!(matches.iter().all(|m| !m.is_prefix));
    let vocab = MerchantVocab {
        canonical: "EXAMPLE".into(),
        expansions: HashMap::from([(
            "ZZ".into(),
            Expansion {
                full: "Example - Wild Hazel".into(),
                classify: true,
            },
        )]),
    };
    assert!(
        classify_receipt_description("ZZ Mush", layers, Some(&vocab))
            .0
            .account
            .is_none()
    );
}

#[test]
fn filling_rule_is_scoped_to_the_brand_and_product() {
    for description in [
        "Full Fortune - Shepherd's",
        "Full Fortune Shepherd' s P",
        "Full Fortune -Shepherd's",
        "Full Forture - Shepherd's",
    ] {
        let (item, matches) = scanned(description);
        assert_eq!(item.tag_path.as_deref(), Some("grocery/frozen"));
        assert_eq!(item.tags, ["grocery", "grocery/frozen"]);
        assert!(matches.iter().all(|m| !m.is_prefix));
    }
    assert_eq!(
        scanned("Example - Garden Shepherd' s")
            .0
            .tag_path
            .as_deref(),
        Some("grocery/vegetable")
    );
    assert!(scanned("Full Fortune - Unknown").0.account.is_none());
}
