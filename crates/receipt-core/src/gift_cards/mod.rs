//! Receipt evidence, never a live card balance or proof of card identity.
//! Source indices address nonempty, trimmed lines of `raw_text`. The evidence
//! text is retained independently so edits and persistence need no OCR backfill.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GiftCardEvidence {
    pub field: String,
    pub line_index: u32,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GiftCardExpiry {
    #[default]
    Unknown,
    NoExpiry,
    PrintedDate,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GiftCardActivation {
    #[default]
    Unknown,
    Activated,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GiftCardRedemption {
    /// Receipt-local extraction identity. Carry through edits; not a card ID
    /// and not stable across a fresh OCR parse.
    pub source_id: String,
    pub issuer: Option<String>,
    /// Currency supplied by the scan context, not inferred from a dollar sign.
    pub currency: Option<String>,
    pub printed_identifier: Option<String>,
    pub normalized_identifier: Option<String>,
    pub remaining_balance_cents: Option<i64>,
    pub authorization_reference: Option<String>,
    pub expiry: GiftCardExpiry,
    pub expiry_date: Option<String>,
    pub evidence: Vec<GiftCardEvidence>,
    /// Field names for observed but unreadable/conflicting evidence.
    pub unresolved_fields: Vec<String>,
    /// Fields explicitly corrected by the user; original evidence stays intact.
    pub corrected_fields: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GiftCardPurchase {
    /// Receipt-local extraction identity. Carry through edits; not a card ID
    /// and not stable across a fresh OCR parse.
    pub source_id: String,
    pub issuer: Option<String>,
    pub currency: Option<String>,
    pub activation: GiftCardActivation,
    pub reference_label: Option<String>,
    pub reference: Option<String>,
    pub card_count: Option<u32>,
    pub denomination_cents: Option<i64>,
    pub total_face_value_cents: Option<i64>,
    pub face_value_derived: bool,
    pub evidence: Vec<GiftCardEvidence>,
    pub unresolved_fields: Vec<String>,
    pub corrected_fields: Vec<String>,
}

/// Only mask typography is normalized. Visible digits (including a masked final
/// digit) are never removed, guessed, padded, or reduced to a last-four key.
pub fn normalize_identifier(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            'x' | 'X' | '×' | '*' => '*',
            _ => c,
        })
        .collect()
}

mod corrections;
mod extraction;
#[cfg(test)]
mod tests;
pub(crate) use extraction::{attach_purchases, extract_redemptions};

fn unresolved(out: &mut Vec<String>, field: &str) {
    if !out.iter().any(|s| s == field) {
        out.push(field.into());
    }
}
pub(crate) fn set_currency(parsed: &mut crate::parser::ParsedReceiptData, currency: &str) {
    for item in &mut parsed.items {
        if let Some(g) = &mut item.gift_card {
            if g.currency.is_none() {
                g.currency = Some(currency.into());
            }
        }
    }
    for tender in &mut parsed.tenders {
        if let Some(g) = &mut tender.gift_card {
            if g.currency.is_none() {
                g.currency = Some(currency.into());
            }
        }
    }
}
