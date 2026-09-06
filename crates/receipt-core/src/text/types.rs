//! Public and private types for text-line item extraction.

use crate::common::ReceiptWarningKind;
use crate::money::Money;

pub(super) use crate::common::ReceiptWarning as TextParserWarning;
pub(super) use crate::extraction::{ExtractedItem as ParsedTextItem, ExtractionOutcome};

#[derive(Clone, Debug)]
pub(crate) struct MalformedTrailingPriceCandidate {
    pub(crate) description: String,
    pub(crate) category_source: String,
    pub(crate) observed_token: String,
    pub(crate) observed_fraction: String,
    pub(crate) whole_dollars: i64,
    pub(crate) context: String,
}

#[derive(Clone, Debug)]
pub(crate) struct CandidatePriceOption {
    pub(crate) price: Money,
    pub(crate) score: usize,
}

#[derive(Clone, Debug)]
pub(crate) enum DeferredTextOutcome {
    Item(ParsedTextItem),
    Warning(ReceiptWarningKind, String),
    Malformed(MalformedTrailingPriceCandidate),
}

#[derive(Clone, Debug)]
pub(crate) struct QuantityModifier {
    pub(crate) quantity: i32,
    pub(crate) unit_price: Option<Money>,
    pub(crate) weight_text: Option<String>,
    pub(crate) deal_price: Option<Money>,
    pub(crate) pattern_type: QuantityPatternType,
}

/// Which shape of quantity expression a row printed.
///
/// The shared `Price` suffix is load-bearing rather than noise, so
/// `enum_variant_names` is allowed here: each name says what is measured *and*
/// how it relates to the amount — "2 @ $1.99" is a count at a unit price,
/// "0.41 lb @ $1.98/lb" is a weight at a unit price, "3 for $5" is a multiple
/// for one price. Trimming to `Count` / `Weight` / `MultiFor` would drop the
/// half that distinguishes a unit price from a bundle price, which is exactly
/// what `validate_quantity_price` keys on.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QuantityPatternType {
    CountAtPrice,
    WeightAtPrice,
    MultiForPrice,
}

#[derive(Clone, Debug)]
pub(crate) struct ReconciliationState {
    pub(crate) score: usize,
    pub(crate) prices: Vec<Money>,
    pub(crate) ambiguous: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ReconciledMalformedPrices {
    pub(crate) prices: Vec<Money>,
}

/// The rows one pass over a receipt reads, plus the receipt-level verdict that
/// changes how it reads them.
///
/// Every stage below wants all three at once: which rows exist, which of them
/// an earlier price already claimed, and whether the right-hand price column
/// drifted a row up. Bundling them is what keeps the extracted stages under
/// `too_many_arguments` without an `#[allow]`, and it is honest rather than
/// convenient — a stage that consults one of these consults all of them.
///
/// `used` is a shared borrow on purpose. Claiming a row is the caller's job, so
/// a stage returns *which* row it claimed and never marks it: that keeps the
/// order of claims in one place, which is what the cross-row-leak guards
/// (bugs C, H, K) depend on.
#[derive(Clone, Copy)]
pub(super) struct Lines<'a> {
    pub(super) all: &'a [String],
    pub(super) used: &'a [bool],
    pub(super) drift: bool,
}

impl<'a> Lines<'a> {
    pub(super) fn of(all: &'a [String], used: &'a [bool], drift: bool) -> Self {
        Lines { all, used, drift }
    }
}
