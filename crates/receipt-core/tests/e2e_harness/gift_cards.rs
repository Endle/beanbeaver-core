//! Shared by cached E2E and device_sim so live OCR checks the same metadata.
use receipt_core::parser::ParsedReceiptData;
use serde_json::Value;

fn subset(expected: &Value, actual: &Value, path: &str, errors: &mut Vec<String>) {
    if let Some(object) = expected.as_object() {
        for (key, value) in object {
            match actual.get(key) {
                Some(got) => subset(value, got, &format!("{path}.{key}"), errors),
                None => errors.push(format!("{path}.{key}: missing (expected {value})")),
            }
        }
    } else if expected != actual {
        errors.push(format!("{path}: expected {expected}, got {actual}"));
    }
}

/// Metadata is positional and anchored to the expected item description or
/// tender kind/amount. JSON null asserts absence; an omitted key is unasserted.
pub fn check(parsed: &ParsedReceiptData, expected: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    if let Some(tenders) = expected.get("tenders").and_then(Value::as_array) {
        if tenders.iter().any(|t| t.get("gift_card").is_some())
            && tenders.len() != parsed.tenders.len()
        {
            errors.push(format!(
                "gift-card tender count: expected {}, got {}",
                tenders.len(),
                parsed.tenders.len()
            ));
        }
        for (i, t) in tenders.iter().enumerate() {
            if let Some(want) = t.get("gift_card") {
                match parsed.tenders.get(i) {
                    Some(got) => {
                        if t.get("kind")
                            .and_then(Value::as_str)
                            .is_some_and(|k| k != got.kind)
                            || t.get("amount")
                                .and_then(Value::as_str)
                                .is_some_and(|a| a != got.amount.to_string())
                        {
                            errors.push(format!(
                                "tender[{i}]: metadata tender anchor does not match"
                            ));
                        }
                        subset(
                            want,
                            &serde_json::to_value(&got.gift_card).unwrap(),
                            &format!("tender[{i}].gift_card"),
                            &mut errors,
                        );
                    }
                    None => errors.push(format!("tender[{i}]: missing gift-card tender")),
                }
            }
        }
    }
    if let Some(items) = expected.get("critical_items").and_then(Value::as_array) {
        for item in items {
            if let Some(want) = item.get("gift_card") {
                let Some(index) = item.get("item_index").and_then(Value::as_u64) else {
                    errors.push("gift_card item expectation requires item_index".into());
                    continue;
                };
                match parsed.items.get(index as usize) {
                    Some(got) => {
                        let desc = item
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if !got
                            .description
                            .to_ascii_uppercase()
                            .contains(&desc.to_ascii_uppercase())
                            || item
                                .get("price")
                                .and_then(Value::as_str)
                                .is_some_and(|p| p != got.price.to_string())
                        {
                            errors.push(format!(
                                "item[{index}]: gift-card description/price anchor does not match"
                            ));
                        }
                        subset(
                            want,
                            &serde_json::to_value(&got.gift_card).unwrap(),
                            &format!("item[{index}].gift_card"),
                            &mut errors,
                        );
                    }
                    None => errors.push(format!("item[{index}]: missing gift-card purchase")),
                }
            }
        }
    }
    errors
}
