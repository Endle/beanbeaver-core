//! Pure receipt-parsing logic: OCR-detection normalization, line grouping, field /
//! item extraction, categorization, and beancount-text formatting.
//!
//! This crate is deliberately free of any Python (PyO3) or ledger (GPL beancount)
//! dependency so it can be compiled for host platforms and `aarch64-apple-ios`
//! alike. The desktop PyO3 extension (`_rust_matcher`) and the iOS app both depend
//! on this crate; the only output it produces is plain beancount-format text.
//!
//! # Module visibility
//!
//! Only the modules a consumer actually calls are `pub`; everything else is
//! `pub(crate)`. This is not tidiness — `pub` in a library suppresses the
//! `dead_code` lint, so publishing every module by default is what let ~1,550
//! lines of unreachable code accumulate here without a single warning.
//!
//! `unnameable_types` is warned on for the other half of that trade: narrowing
//! a module leaves any type still used in a `pub` signature reachable but
//! impossible for a caller to name, and rustc allows that silently by default.
//! Every type the lint finds is either re-exported below or a sign that the
//! signature should not have been public.
#![warn(unnameable_types)]

pub mod date;
pub(crate) mod detection_normalization;
pub mod merchant_match;
pub(crate) mod merchant_vocab;
pub mod money;
pub(crate) mod ocr_confusion;
pub(crate) mod ocr_line_grouping;
pub mod ocr_transform;
pub mod process;
pub mod receipt_categories;
pub mod receipt_common;
pub(crate) mod receipt_fields;
pub(crate) mod receipt_formatter;
pub(crate) mod receipt_parse_helpers;
pub mod receipt_parser;
pub(crate) mod receipt_spatial;
pub(crate) mod receipt_text;
pub mod rules;
pub(crate) mod spatial;

// Types that appear in the public signatures of `ocr_transform`, `receipt_parser`
// and `rules`, but whose defining modules are `pub(crate)`. Re-exported here so
// callers can name what they are already required to pass.
//
// The nine below are the whole set — `unnameable_types` fails the build's
// warning list if that stops being true. Note that seven of them are the OCR
// input structs; Phase 2 of `beanbeaver_core_refactor_plan.md` collapses those
// into one document type, and this block is the single place that then changes.
pub use merchant_vocab::{Expansion, MerchantVocab};
pub use receipt_parse_helpers::{MerchantLineInput, MerchantPageInput, MerchantWordInput};
pub use receipt_spatial::{BboxInput, LineInput, PageInput, WordInput};
