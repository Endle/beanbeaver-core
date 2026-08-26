//! Tuning constants and the compiled regexes for spatial item extraction.
//!
//! Same split as `text/patterns.rs`: the shapes live apart from the logic that
//! applies them, so a pattern can be read against the receipt it came from.

use regex::Regex;
use std::sync::OnceLock;

use crate::common::{WEIGHT_UNIT_AT_SEP, WEIGHT_UNIT_CLASS};

pub(crate) const SCALE: i64 = 10_000;

pub(crate) const MIN_CONFIDENCE: f64 = 0.5;

pub(crate) const PRICE_X_THRESHOLD: f64 = 0.65;

pub(crate) const Y_TOLERANCE: f64 = 0.02;

pub(crate) const MAX_ITEM_DISTANCE: f64 = 0.08;

pub(crate) const SPATIAL_FLOAT_EPSILON: f64 = 1e-6;

pub(crate) fn re_digits_dots_only() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[\d.]+$").unwrap())
}

pub(crate) fn re_long_digits_only() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d{8,}\s*$").unwrap())
}

pub(crate) fn re_standalone_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\$?\d+\.\d{2}\s*$").unwrap())
}

pub(crate) fn re_trailing_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(\d+\.\d{2})(-?)(?:\s*[HhTtJjGgPp])*\s*$").unwrap())
}

pub(crate) fn re_weight_info() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d+\.\d+\s*kg").unwrap())
}

pub(crate) fn re_w_dollar() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^W\s*\$").unwrap())
}

pub(crate) fn re_malformed_ocr_prefix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\(H{1,2}E[DI]?\b").unwrap())
}

pub(crate) fn re_mangled_reg_marker() -> &'static Regex {
    // Matches OCR-corrupted REG-price marker fragments where OCR mangled the
    // leading R (into "#", "4", "@", "(") and/or dropped the G (so "REG$" was
    // captured as "E$"). Also catches the "EREG" / "REG$" forms.
    //
    // Hits: "#EG", "4EG62.99", "(EG$5.99", "#E$", "#E$5.99", "REG$5.99",
    // "EREG12.99". Misses real items because each branch requires the
    // marker shape (non-alpha prefix or literal REG) and a tight content
    // pattern, not just any text containing those substrings.
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^(?:[^A-Za-z\s]{1,3}E(?:G(?:\$?\d+\.\d{2})?|\$(?:\d+\.\d{2})?)|E?REG\$?\d+\.\d{2})\.?$",
        )
        .unwrap()
    })
}

pub(crate) fn re_multibuy_parenthetical() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\(\d+\s*/\s*for\s+\$[\d.]+\)").unwrap())
}

pub(crate) fn re_short_parenthetical_code() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\([^)]{1,5}\)").unwrap())
}

pub(crate) fn re_footer_address_patterns() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"\b(AVE|AVENUE|ST|STREET|RD|ROAD|BLVD|BOULEVARD|DR|DRIVE|HWY|HIGHWAY)\b|\b(MARKHAM|TORONTO|MISSISSAUGA|RICHMOND\s+HILL|ON|ONTARIO)\b|\b(L\d[A-Z]\d)\b|\(\d{3}\)\s*\d{3}-\d{4}",
        )
        .unwrap()
    })
}

pub(crate) fn re_receipt_metadata_patterns() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)WS#|RECEIPT#|CASHIER|ITEM\s+COUNT|NUMBER\s+OF\s+ITEMS|HAPPY\s+SHOPPING|CREDIT\s+CARD|DEBIT|APPROVED|AUTH|REFERENCE|TERMINAL|CUSTOMER\s+COPY",
        )
        .unwrap()
    })
}

pub(crate) fn re_count_at_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\d+\s*@\s*\$?-?\d+\.\d{2}").unwrap())
}

pub(crate) fn re_weight_at_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"^\d+\.?\d*\s*{WEIGHT_UNIT_CLASS}{WEIGHT_UNIT_AT_SEP}@"
        ))
        .unwrap()
    })
}

pub(crate) fn re_multi_for_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\(?\d+\s*/\s*for\s+\$?\d+\.\d{2}\)?").unwrap())
}

pub(crate) fn re_compact_offer_fragment() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d+\s*@\s*\d+\s*/\s*\$?\d+\.\d{2}\b").unwrap())
}

pub(crate) fn re_parenthetical_offer_prefix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\([^)]+\)\s+\d+\s*/\s*for\b").unwrap())
}

pub(crate) fn re_section_header_with_aisle() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[^A-Z0-9]*\d{1,2}\s*[-:]\s*[A-Z]{3,}$").unwrap())
}

pub(crate) fn re_summary_patterns() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^(?:SUB\s*TOTAL|SUBTOTAL|TOTAL|HST|GST|PST|TAX|MASTER(?:CARD)?|VISA|DEBIT|CREDIT|POINTS|CASH|CHANGE|BALANCE|APPROVED|CARD|TERMINAL|MEMBER)\b",
        )
        .unwrap()
    })
}

pub(crate) fn re_tax_tokens() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(HST|GST|PST|TAX)\b").unwrap())
}

pub(crate) fn re_section_aisle_prefix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[^A-Z0-9]*\d{1,2}\s*[-:]").unwrap())
}

pub(crate) fn re_dept_marker_prefix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[&8]{2}\.?\s").unwrap())
}

pub(crate) fn re_total_ocr_variants() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"T[O0C]TA[L1I]").unwrap())
}

pub(crate) fn re_leading_section_item_prefix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^[^A-Z0-9]*\d{1,2}\s*[-:]\s*(MEAT|SEAFOOD|PRODUCE|DELI|GROCERY|BAKERY|FROZEN|FOOD)\b\s*",
        )
        .unwrap()
    })
}

pub(crate) fn re_ascii_words() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[A-Z]+").unwrap())
}

pub(crate) fn re_price_word() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Trailing `-` is Costco's convention for discount/refund lines
    // (e.g. TPD/<sku> 3.00-); LEADING `-` is Loblaws-family convention for
    // discount lines (e.g. "Member Pricing MRJ -1.49"). Either marks the
    // amount as negative. The optional trailing letters are tax flags that
    // can fuse with the price into a single OCR token: Costco's H/T/J and
    // T&T's G (GST) / P (PST) / F (food, zero-rated), and T&T may print
    // several space-separated (e.g. "$6.87 G P", "$12.81 G F").
    RE.get_or_init(|| Regex::new(r"^(-?)\$?(\d+\.\d{2})(-?)(?:\s*[HhTtJjGgPpFf])*$").unwrap())
}

pub(crate) fn re_embedded_trailing_price_word() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)[A-Z]{1,6}\$?(\d+\.\d{2})$").unwrap())
}

pub(crate) fn re_leading_qty_prefix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\(\d+\)\s*").unwrap())
}

pub(crate) fn re_leading_long_sku() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d{6,}[A-Za-z]?\s*").unwrap())
}

// Short numeric item codes (Costco prints 3-5 digit codes, e.g. "458 MILK 2%").
// Only stripped when followed by whitespace and more text, so a bare numeric
// line is left to the digits-only guards. Used for the description-quality
// (alpha-ratio) check, not for the displayed description.
pub(crate) fn re_leading_short_code() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d{3,5}\s+(?P<rest>\S)").unwrap())
}

// Scale weight-block lines on weighed-produce receipts: No Frills prints
// "0.985 kg Gross" / "-0.010 kg Tare =" / "0.975 kg Net @ $4.39/kg" under a
// single produce label, once per weighing. Never descriptions; a contiguous
// run of them means the label above is shared by several priced weighings.
// Prefix match: OCR mangles the tail freely ("Grosks", "Gros ed ...").
pub(crate) fn re_weight_info_line() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^-?\d+(?:[.,]\d+)?\s*kg\s+(?:gro|tare|net)").unwrap())
}

// "<weight> kg @ $<unit>/kg" with both numbers capturable, to check whether a
// weighed qty row's trailing price is actually its own line total.
pub(crate) fn re_weight_at_unit_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^\s*(\d+(?:\.\d+)?)\s*(?:kg|k9|kg\.|lb|1b|lk|1k)\s*@\s*\$?(\d+(?:\.\d+)?)")
            .unwrap()
    })
}

pub(crate) fn re_sale_marker() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\(SALE\)\s*").unwrap())
}

pub(crate) fn re_hed_marker() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\(HED[^)]*\)\s*").unwrap())
}

pub(crate) fn re_hhed_marker() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\(HHED[^)]*\)\s*").unwrap())
}

pub(crate) fn re_qty_price_marker() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"@?\d+/[A-Za-z]?\$?\d+\.\d{2}").unwrap())
}

pub(crate) fn re_qty_price_marker_2() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\d+/\$?\d+\.\d{2}").unwrap())
}

pub(crate) fn re_unit_price_marker() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\$\d+\.\d+/\w+").unwrap())
}

pub(crate) fn re_inline_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\$\d+\.\d{2}").unwrap())
}

pub(crate) fn re_garbled_price_artifact() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\d+s\d+\.\d+ea").unwrap())
}

pub(crate) fn re_cahrd() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bCAHRD\b").unwrap())
}

pub(crate) fn re_costco_discount_line() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Costco's "tier price discount" lines reference another SKU and are
    // therefore mostly digits ("TPD/1234567", "TPD/1234567/7"), which
    // fails the generic alpha-ratio filter in `is_valid_item_line`.
    // Allow embedded whitespace too — Costco OCR sometimes reads a digit
    // as a space (e.g. "TPD/1 96144" for "TPD/1796144"). Allow `TP[A-Z]/`
    // because OCR also occasionally reads the `D` as `U` etc.
    //
    // The leading `[\d\s]*` is the discount row's own SKU and tier count.
    // Costco prints them in the left column ahead of the reference
    // ("2030193 3 TPD/1944033"), and whether they land in the same grouped
    // line as the `TPD/` token is a fact about the OCR's column splitting,
    // not about the receipt — so anchoring hard at `TP` made the rule fire
    // on some Costco receipts and not others. Still anchored at both ends,
    // and still digits-only either side, so this widens what counts as the
    // *prefix* without letting a prose line through.
    RE.get_or_init(|| Regex::new(r"^[\d\s]*TP[A-Z]/[\d/\s]+$").unwrap())
}

pub(crate) fn re_hed_word() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bHED\b").unwrap())
}

pub(crate) fn re_leading_non_alnum() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[^A-Za-z0-9]+").unwrap())
}

pub(crate) fn re_trailing_non_alnum() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[^A-Za-z0-9)]+$").unwrap())
}

pub(crate) fn re_multi_spaces() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+").unwrap())
}

/// Detections closer than half a character cell share a print-grid column.
pub(crate) const ANNOTATION_COLUMN_LINK: f64 = 0.5;
