//! Gift-card records crossing the native seam.
use receipt_core::gift_cards as core;
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum GiftCardExpiry {
    Unknown,
    NoExpiry,
    PrintedDate,
}
impl From<core::GiftCardExpiry> for GiftCardExpiry {
    fn from(v: core::GiftCardExpiry) -> Self {
        match v {
            core::GiftCardExpiry::Unknown => Self::Unknown,
            core::GiftCardExpiry::NoExpiry => Self::NoExpiry,
            core::GiftCardExpiry::PrintedDate => Self::PrintedDate,
        }
    }
}
impl From<GiftCardExpiry> for core::GiftCardExpiry {
    fn from(v: GiftCardExpiry) -> Self {
        match v {
            GiftCardExpiry::Unknown => Self::Unknown,
            GiftCardExpiry::NoExpiry => Self::NoExpiry,
            GiftCardExpiry::PrintedDate => Self::PrintedDate,
        }
    }
}
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum GiftCardActivation {
    Unknown,
    Activated,
}
impl From<core::GiftCardActivation> for GiftCardActivation {
    fn from(v: core::GiftCardActivation) -> Self {
        match v {
            core::GiftCardActivation::Unknown => Self::Unknown,
            core::GiftCardActivation::Activated => Self::Activated,
        }
    }
}
impl From<GiftCardActivation> for core::GiftCardActivation {
    fn from(v: GiftCardActivation) -> Self {
        match v {
            GiftCardActivation::Unknown => Self::Unknown,
            GiftCardActivation::Activated => Self::Activated,
        }
    }
}
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct GiftCardEvidence {
    pub field: String,
    pub line_index: u32,
    pub text: String,
}
impl From<core::GiftCardEvidence> for GiftCardEvidence {
    fn from(v: core::GiftCardEvidence) -> Self {
        Self {
            field: v.field,
            line_index: v.line_index,
            text: v.text,
        }
    }
}
impl From<GiftCardEvidence> for core::GiftCardEvidence {
    fn from(v: GiftCardEvidence) -> Self {
        Self {
            field: v.field,
            line_index: v.line_index,
            text: v.text,
        }
    }
}
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct GiftCardRedemption {
    /// Receipt-local extraction identity, not a card identity.
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
impl From<core::GiftCardRedemption> for GiftCardRedemption {
    fn from(v: core::GiftCardRedemption) -> Self {
        Self {
            source_id: v.source_id,
            issuer: v.issuer,
            currency: v.currency,
            printed_identifier: v.printed_identifier,
            normalized_identifier: v.normalized_identifier,
            remaining_balance_cents: v.remaining_balance_cents,
            authorization_reference: v.authorization_reference,
            expiry: v.expiry.into(),
            expiry_date: v.expiry_date,
            evidence: v.evidence.into_iter().map(Into::into).collect(),
            unresolved_fields: v.unresolved_fields,
            corrected_fields: v.corrected_fields,
        }
    }
}
impl From<GiftCardRedemption> for core::GiftCardRedemption {
    fn from(v: GiftCardRedemption) -> Self {
        Self {
            source_id: v.source_id,
            issuer: v.issuer,
            currency: v.currency,
            printed_identifier: v.printed_identifier,
            normalized_identifier: v.normalized_identifier,
            remaining_balance_cents: v.remaining_balance_cents,
            authorization_reference: v.authorization_reference,
            expiry: v.expiry.into(),
            expiry_date: v.expiry_date,
            evidence: v.evidence.into_iter().map(Into::into).collect(),
            unresolved_fields: v.unresolved_fields,
            corrected_fields: v.corrected_fields,
        }
    }
}
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct GiftCardPurchase {
    /// Receipt-local extraction identity, not a card identity.
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
impl From<core::GiftCardPurchase> for GiftCardPurchase {
    fn from(v: core::GiftCardPurchase) -> Self {
        Self {
            source_id: v.source_id,
            issuer: v.issuer,
            currency: v.currency,
            activation: v.activation.into(),
            reference_label: v.reference_label,
            reference: v.reference,
            card_count: v.card_count,
            denomination_cents: v.denomination_cents,
            total_face_value_cents: v.total_face_value_cents,
            face_value_derived: v.face_value_derived,
            evidence: v.evidence.into_iter().map(Into::into).collect(),
            unresolved_fields: v.unresolved_fields,
            corrected_fields: v.corrected_fields,
        }
    }
}
impl From<GiftCardPurchase> for core::GiftCardPurchase {
    fn from(v: GiftCardPurchase) -> Self {
        Self {
            source_id: v.source_id,
            issuer: v.issuer,
            currency: v.currency,
            activation: v.activation.into(),
            reference_label: v.reference_label,
            reference: v.reference,
            card_count: v.card_count,
            denomination_cents: v.denomination_cents,
            total_face_value_cents: v.total_face_value_cents,
            face_value_derived: v.face_value_derived,
            evidence: v.evidence.into_iter().map(Into::into).collect(),
            unresolved_fields: v.unresolved_fields,
            corrected_fields: v.corrected_fields,
        }
    }
}
