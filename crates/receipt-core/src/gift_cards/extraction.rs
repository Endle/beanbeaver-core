use super::*;
use crate::fields::TenderLine;
use crate::money::Money;
use crate::ocr_document::OcrDocument;
use crate::parser::ParsedReceiptItem;
use regex::Regex;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

// A source token belongs to one extraction, not to an issuer card. Including
// the OCR document and anchor prevents a stale edit attaching to a different
// occurrence after reprocessing. No cross-version/hash stability is promised.
fn source_id(role: &str, index: usize, document: &str, anchor: &str) -> String {
    let mut h = DefaultHasher::new();
    document.hash(&mut h);
    anchor.hash(&mut h);
    format!("{role}:{index}:{:016x}", h.finish())
}

struct Row<'a> {
    index: usize,
    text: &'a str,
    y: f64,
}
fn rows(doc: &OcrDocument) -> Vec<Row<'_>> {
    let mut rows: Vec<_> = doc
        .lines
        .iter()
        .filter(|l| !l.text.trim().is_empty())
        .enumerate()
        .map(|(index, l)| Row {
            index,
            text: l.text.trim(),
            y: l.center_y,
        })
        .collect();
    // Geometry is authoritative when available. Text-only callers retain order.
    if rows.iter().all(|r| r.y > 0.0) {
        rows.sort_by(|a, b| a.y.total_cmp(&b.y));
    }
    rows
}
fn evidence(out: &mut Vec<GiftCardEvidence>, field: &str, row: &Row<'_>) {
    let e = GiftCardEvidence {
        field: field.into(),
        line_index: row.index as u32,
        text: row.text.into(),
    };
    if !out.contains(&e) {
        out.push(e);
    }
}
fn identifier_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)[0-9x×*]*[x×*]{2,}[0-9x×*]*[0-9][0-9x×*]*").unwrap())
}
fn balance_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(?:BAL\s*:|REMAINING\s+BALANCE\s*:|GIFT\s+CARD\s+BALANCE\s*:)").unwrap()
    })
}
fn amount_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:\$\s*)?\d+(?:,\d{3})*\.\d{2}\b").unwrap())
}
fn auth_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(?:AUTH[O0]R\.?|AUTH|APP)\b\s*[.#:]*\s*([A-Z0-9]+)").unwrap()
    })
}
fn expiry_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\bEXP\s*[:.]\s*(NONE\b|\d{4}[-/]\d{1,2}[-/]\d{1,2}\b|\d{1,2}[-/]\d{4}\b|\d{1,2}[-/]\d{1,2}(?:[-/]\d{2,4})?\b|\d{1,2} [A-Z]{3} \d{4}\b)").unwrap())
}
fn amounts(s: &str) -> Vec<i64> {
    amount_re()
        .find_iter(s)
        .filter(|m| {
            // Reject a substring of a malformed or negative amount. Never turn
            // "1,50.00" into 50.00 or "-10.00" into a positive balance.
            let boundary = |c: char| !c.is_alphanumeric() && !matches!(c, ',' | '.' | '-');
            s[..m.start()].chars().next_back().map_or(true, boundary)
                && s[m.end()..].chars().next().map_or(true, boundary)
        })
        .filter_map(|m| {
            Money::parse_strict(&m.as_str().trim_start_matches('$').trim().replace(',', ""))
                .ok()
                .map(Money::cents)
        })
        .collect()
}
fn unique<T: Eq + Clone>(
    values: &[T],
    missing: bool,
    unresolved_fields: &mut Vec<String>,
    field: &str,
) -> Option<T> {
    if missing || values.windows(2).any(|v| v[0] != v[1]) {
        unresolved(unresolved_fields, field);
        None
    } else {
        values.first().cloned()
    }
}
fn block_end(text: &str) -> bool {
    let u = text.to_ascii_uppercase();
    [
        "UNITS PURCHASED",
        "AEROPLAN",
        "POINTS",
        "HST",
        "ALL RETURNS",
        "CUSTOMER COPY",
        "IMPORTANT",
        "SUBTOTAL",
        "TOTAL",
    ]
    .iter()
    .any(|s| u.contains(s))
}

pub(crate) fn extract_redemptions(
    doc: &OcrDocument,
    merchant: &str,
    tenders: &[TenderLine],
) -> Vec<Option<GiftCardRedemption>> {
    let document_text = doc.full_text();
    let rows = rows(doc);
    let positions: Vec<_> = tenders
        .iter()
        .map(|t| rows.iter().position(|r| r.index == t.source_line_index))
        .collect();
    tenders
        .iter()
        .enumerate()
        .map(|(i, t)| {
            if t.kind != "gift_card" {
                return None;
            }
            let mut out = GiftCardRedemption {
                source_id: source_id(
                    "tender",
                    i,
                    &document_text,
                    &format!("{}:{}", t.raw_label, t.amount_cents),
                ),
                ..Default::default()
            };
            let shop = t.raw_label.to_ascii_uppercase().contains("SHOP CARD");
            let lcbo = merchant.to_ascii_uppercase().contains("LCBO");
            if shop {
                out.issuer = Some("Costco Shop Card".into());
            } else if lcbo {
                out.issuer = Some("LCBO".into());
            }
            let Some(pos) = positions[i] else {
                unresolved(&mut out.unresolved_fields, "association");
                return Some(out);
            };
            evidence(&mut out.evidence, "amount_used", &rows[pos]);
            // LCBO prints details AFTER its tender; Costco Shop Card prints an
            // authorization block BEFORE the amount-bearing tender. Adjacent
            // payment anchors always bound the search; a summary/footer ends it.
            let (start, end) = if shop {
                let previous = positions
                    .iter()
                    .flatten()
                    .copied()
                    .filter(|p| *p < pos)
                    .max()
                    .map_or(0, |p| p + 1);
                let start = (previous..pos)
                    .rev()
                    .find(|p| block_end(rows[*p].text))
                    .map_or(previous, |p| p + 1);
                (start, pos + 1)
            } else {
                let next = positions
                    .iter()
                    .flatten()
                    .copied()
                    .filter(|p| *p > pos)
                    .min()
                    .unwrap_or(rows.len());
                let end = (pos + 1..next)
                    .find(|p| {
                        block_end(rows[*p].text)
                            || crate::fields::classify_tender_line(
                                &rows[*p].text.to_ascii_uppercase(),
                            )
                            .is_some()
                    })
                    .unwrap_or(next);
                (pos, end)
            };
            let block = &rows[start..end];
            let amount_row = if amounts(rows[pos].text).is_empty() {
                rows.get(pos + 1).map(|r| r.index)
            } else {
                None
            };
            let mut ids = Vec::new();
            let mut balances = Vec::new();
            let mut auths = Vec::new();
            let mut expiries = Vec::new();
            let mut unread_balance = false;
            let mut unread_expiry = false;
            for (j, row) in block.iter().enumerate() {
                for m in identifier_re().find_iter(row.text) {
                    if m.as_str().chars().count() >= 8
                        && row.text[..m.start()]
                            .chars()
                            .next_back()
                            .map_or(true, |c| !c.is_alphanumeric())
                        && row.text[m.end()..]
                            .chars()
                            .next()
                            .map_or(true, |c| !c.is_alphanumeric())
                    {
                        ids.push(m.as_str().to_string());
                        evidence(&mut out.evidence, "printed_identifier", row);
                    }
                }
                for c in auth_re().captures_iter(row.text) {
                    auths.push(c[1].to_string());
                    evidence(&mut out.evidence, "authorization_reference", row);
                }
                if row.text.to_ascii_uppercase().contains("EXP") {
                    evidence(&mut out.evidence, "expiry", row);
                    let found: Vec<_> = expiry_re().captures_iter(row.text).collect();
                    if found.is_empty() {
                        unread_expiry = true;
                    }
                    expiries.extend(found.iter().map(|c| c[1].trim().to_ascii_uppercase()));
                }
                if let Some(label) = balance_re().find(row.text) {
                    evidence(&mut out.evidence, "remaining_balance_cents", row);
                    let tail = &row.text[label.end()..];
                    let cut = tail
                        .to_ascii_uppercase()
                        .find("AMOUNT:")
                        .unwrap_or(tail.len());
                    let mut found = amounts(&tail[..cut]);
                    if found.is_empty() {
                        let prefix = &row.text[..label.start()];
                        let upper = prefix.to_ascii_uppercase();
                        if !upper.contains("AMOUNT:")
                            && !upper.contains(&t.raw_label.to_ascii_uppercase())
                        {
                            found = amounts(prefix);
                        }
                    }
                    // Split rows can run value-before-label in OCR order. Only an
                    // otherwise standalone amount qualifies; never borrow a tender.
                    if found.is_empty() {
                        for near in [j.checked_sub(1), (j + 1 < block.len()).then_some(j + 1)]
                            .into_iter()
                            .flatten()
                        {
                            if amount_row == Some(block[near].index) {
                                continue;
                            }
                            let text = block[near].text.trim();
                            if amount_re().find(text).is_some_and(|m| m.as_str() == text) {
                                found.extend(amounts(text));
                                evidence(
                                    &mut out.evidence,
                                    "remaining_balance_cents",
                                    &block[near],
                                );
                            }
                        }
                    }
                    if found.len() != 1 {
                        unread_balance = true;
                    }
                    balances.extend(found);
                }
            }
            // Different mask glyphs can represent identical evidence, but the
            // original first OCR spelling is kept for display/correction.
            let normalized: Vec<_> = ids.iter().map(|s| normalize_identifier(s)).collect();
            out.normalized_identifier = unique(
                &normalized,
                false,
                &mut out.unresolved_fields,
                "printed_identifier",
            );
            if out.normalized_identifier.is_some() {
                out.printed_identifier = ids.first().cloned();
            }
            out.remaining_balance_cents = unique(
                &balances,
                unread_balance,
                &mut out.unresolved_fields,
                "remaining_balance_cents",
            );
            out.authorization_reference = unique(
                &auths,
                false,
                &mut out.unresolved_fields,
                "authorization_reference",
            );
            if let Some(exp) = unique(
                &expiries,
                unread_expiry,
                &mut out.unresolved_fields,
                "expiry",
            ) {
                if exp == "NONE" {
                    out.expiry = GiftCardExpiry::NoExpiry;
                } else {
                    out.expiry = GiftCardExpiry::PrintedDate;
                    out.expiry_date = Some(exp);
                }
            }
            Some(out)
        })
        .collect()
}

fn compact(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect()
}
fn program(description: &str) -> Option<&'static str> {
    let d = compact(description);
    if d.contains("DOORDASH") {
        Some("DoorDash")
    } else if d.contains("LCBOCARD") {
        Some("LCBO")
    } else {
        None
    }
}
fn pc_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\bPC\s*[:#]?\s*(\d+)\b").unwrap())
}
fn product_row_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d+\s+[A-Za-z]").unwrap())
}
fn pack_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(\d+)\s*[X×]\s*\$?(\d+(?:\.\d{2})?)\b").unwrap())
}

pub(crate) fn attach_purchases(doc: &OcrDocument, merchant: &str, items: &mut [ParsedReceiptItem]) {
    if !merchant.to_ascii_uppercase().contains("COSTCO") {
        return;
    }
    let document_text = doc.full_text();
    let rows = rows(doc);
    // Establish an occurrence mapping before assigning any activation. Equal
    // products stay separate. If extraction lost an occurrence, no reference
    // is guessed from its position in the surviving item list.
    let mut claimed = std::collections::HashSet::new();
    let all_descriptions: Vec<_> = items.iter().map(|i| compact(&i.description)).collect();
    for i in 0..items.len() {
        let Some(issuer) = program(&items[i].description) else {
            continue;
        };
        let key = &all_descriptions[i];
        let candidates: Vec<_> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                let row = compact(r.text);
                row.contains(key.as_str())
                    || (items[i]
                        .item_number
                        .as_ref()
                        .is_some_and(|n| r.text.starts_with(&format!("{n} ")))
                        && program(r.text) == Some(issuer))
            })
            .map(|(p, _)| p)
            .collect();
        let count = all_descriptions.iter().filter(|d| *d == key).count();
        let mut out = GiftCardPurchase {
            source_id: source_id("item", i, &document_text, &items[i].description),
            issuer: Some(issuer.into()),
            ..Default::default()
        };
        if candidates.len() != count {
            unresolved(&mut out.unresolved_fields, "association");
            for p in candidates {
                evidence(&mut out.evidence, "association", &rows[p]);
            }
            items[i].gift_card = Some(out);
            continue;
        }
        let Some(pos) = candidates.into_iter().find(|p| !claimed.contains(p)) else {
            continue;
        };
        claimed.insert(pos);
        evidence(&mut out.evidence, "item", &rows[pos]);
        evidence(&mut out.evidence, "issuer", &rows[pos]);
        // A different product or the subtotal closes this annotation block.
        let end = (pos + 1..rows.len())
            .find(|p| {
                block_end(rows[*p].text)
                    || product_row_re().is_match(rows[*p].text)
                    || all_descriptions
                        .iter()
                        .any(|d| compact(rows[*p].text).contains(d.as_str()))
            })
            .unwrap_or(rows.len());
        let mut refs = Vec::new();
        let mut activated = false;
        let mut unread_reference = false;
        let mut negative_activation = false;
        for row in &rows[pos..end] {
            let upper = row.text.to_ascii_uppercase();
            if upper.starts_with("PC ") || upper.starts_with("PC:") || upper == "PC" {
                out.reference_label = Some("PC".into());
                evidence(&mut out.evidence, "reference", row);
                if !pc_re().is_match(row.text) {
                    unread_reference = true;
                }
            }
            for c in pc_re().captures_iter(row.text) {
                refs.push(c[1].to_string());
                evidence(&mut out.evidence, "reference", row);
            }
            if row
                .text
                .split_whitespace()
                .any(|s| s.eq_ignore_ascii_case("ACTIVATED"))
            {
                if upper.contains("NOT ACTIVATED") || upper.contains("FAILED") {
                    negative_activation = true;
                } else {
                    activated = true;
                }
                evidence(&mut out.evidence, "activation", row);
            }
        }
        out.reference = unique(
            &refs,
            unread_reference,
            &mut out.unresolved_fields,
            "reference",
        );
        if !refs.is_empty() {
            out.reference_label = Some("PC".into());
        }
        if negative_activation {
            unresolved(&mut out.unresolved_fields, "activation");
        }
        if activated && !negative_activation {
            out.activation = GiftCardActivation::Activated;
        }
        // Pack notation establishes count and denomination, never card IDs.
        // A purchase price by itself does not establish any loaded/face value.
        if let Some(c) = pack_re().captures(rows[pos].text) {
            let count = c[1].parse::<u32>().ok().filter(|n| *n > 0);
            let denomination = Money::parse_strict(&c[2])
                .ok()
                .map(Money::cents)
                .filter(|n| *n > 0);
            if let (Some(n), Some(v)) = (count, denomination) {
                out.card_count = Some(n);
                out.denomination_cents = Some(v);
                out.total_face_value_cents = v.checked_mul(i64::from(n));
                out.face_value_derived = out.total_face_value_cents.is_some();
                evidence(&mut out.evidence, "face_value", &rows[pos]);
            }
        }
        items[i].gift_card = Some(out);
    }
}
