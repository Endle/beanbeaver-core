use super::*;

// Corrections keep a receipt's source evidence; the typed values are the user's
// effective reading. Tracking can later distinguish the two without a backfill.
macro_rules! corrected_fields {
    ($new:ident, $old:ident, $($field:ident),+ $(,)?) => { $(
        if $new.$field != $old.$field {
            unresolved(&mut $new.corrected_fields, stringify!($field));
            $new.unresolved_fields.retain(|f| f != stringify!($field));
        }
    )+ };
}
impl GiftCardRedemption {
    pub(crate) fn correct_from(&mut self, old: &Self) -> Result<(), String> {
        self.evidence = old.evidence.clone();
        self.corrected_fields = old.corrected_fields.clone();
        self.unresolved_fields = old.unresolved_fields.clone();
        corrected_fields!(
            self,
            old,
            issuer,
            currency,
            printed_identifier,
            remaining_balance_cents,
            authorization_reference,
            expiry,
            expiry_date
        );
        self.normalized_identifier = self.printed_identifier.as_deref().map(normalize_identifier);
        if self.remaining_balance_cents.is_some_and(|v| v < 0) {
            return Err("gift-card balance must not be negative".into());
        }
        if self.expiry == GiftCardExpiry::PrintedDate
            && self
                .expiry_date
                .as_ref()
                .map_or(true, |s| s.trim().is_empty())
        {
            return Err("gift-card expiry date is missing".into());
        }
        if self.expiry != GiftCardExpiry::PrintedDate {
            self.expiry_date = None;
        }
        Ok(())
    }
}
impl GiftCardPurchase {
    pub(crate) fn correct_from(&mut self, old: &Self) -> Result<(), String> {
        self.evidence = old.evidence.clone();
        self.corrected_fields = old.corrected_fields.clone();
        self.unresolved_fields = old.unresolved_fields.clone();
        if self.face_value_derived {
            self.total_face_value_cents = Some(
                self.card_count
                    .zip(self.denomination_cents)
                    .and_then(|(n, v)| v.checked_mul(i64::from(n)))
                    .ok_or(
                        "derived gift-card face value requires a valid count and denomination",
                    )?,
            );
        }
        corrected_fields!(
            self,
            old,
            issuer,
            currency,
            activation,
            reference,
            reference_label,
            card_count,
            denomination_cents,
            total_face_value_cents,
            face_value_derived
        );
        if self.card_count == Some(0)
            || self.denomination_cents.is_some_and(|v| v < 0)
            || self.total_face_value_cents.is_some_and(|v| v < 0)
        {
            return Err("gift-card face value/count is invalid".into());
        }
        Ok(())
    }
}
