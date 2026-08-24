//! Public and private types for text-line item extraction.

use crate::common::ReceiptWarningKind;
use crate::money::Money;

#[derive(Clone, Debug)]
pub struct ParsedTextItem {
    pub description: String,
    pub category_source: String,
    pub price: Money,
    pub quantity: i32,
}

#[derive(Clone, Debug)]
pub struct TextParserWarning {
    pub kind: ReceiptWarningKind,
    pub message: String,
    pub after_item_index: Option<usize>,
}

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
