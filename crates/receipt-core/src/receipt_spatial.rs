use regex::Regex;
use std::sync::OnceLock;

const SCALE: i64 = 10_000;
const MIN_CONFIDENCE: f64 = 0.5;
const PRICE_X_THRESHOLD: f64 = 0.65;
const Y_TOLERANCE: f64 = 0.02;
const MAX_ITEM_DISTANCE: f64 = 0.08;
const SPATIAL_FLOAT_EPSILON: f64 = 1e-6;

#[derive(Clone, Debug)]
pub struct BboxInput {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

#[derive(Clone, Debug)]
pub struct WordInput {
    pub text: String,
    pub bbox: BboxInput,
    pub confidence: f64,
}

#[derive(Clone, Debug)]
pub struct LineInput {
    pub text: String,
    pub words: Vec<WordInput>,
}

#[derive(Clone, Debug)]
pub struct PageInput {
    pub lines: Vec<LineInput>,
}

#[derive(Clone, Debug)]
pub struct SpatialExtractedItem {
    pub description: String,
    pub price_scaled: i64,
}

#[derive(Clone, Debug)]
pub struct SpatialParserWarning {
    pub message: String,
    pub after_item_index: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct SpatialExtractionOutcome {
    pub items: Vec<SpatialExtractedItem>,
    pub warnings: Vec<SpatialParserWarning>,
}

#[derive(Clone, Debug)]
struct ParsedLine {
    line_y: f64,
    full_text: String,
    left_text: String,
}

#[derive(Clone, Debug)]
struct PriceCandidate {
    price_y: f64,
    price_scaled: i64,
    source_line_index: usize,
}

fn re_digits_dots_only() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[\d.]+$").unwrap())
}

fn re_long_digits_only() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d{8,}\s*$").unwrap())
}

fn re_standalone_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\$?\d+\.\d{2}\s*$").unwrap())
}

fn re_trailing_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(\d+\.\d{2})(-?)(?:\s*[HhTtJjGgPp])*\s*$").unwrap())
}

fn re_weight_info() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d+\.\d+\s*kg").unwrap())
}

fn re_w_dollar() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^W\s*\$").unwrap())
}

fn re_malformed_ocr_prefix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\(H{1,2}E[DI]?\b").unwrap())
}

fn re_mangled_reg_marker() -> &'static Regex {
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

fn re_multibuy_parenthetical() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\(\d+\s*/\s*for\s+\$[\d.]+\)").unwrap())
}

fn re_short_parenthetical_code() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\([^)]{1,5}\)").unwrap())
}

fn re_footer_address_patterns() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"\b(AVE|AVENUE|ST|STREET|RD|ROAD|BLVD|BOULEVARD|DR|DRIVE|HWY|HIGHWAY)\b|\b(MARKHAM|TORONTO|MISSISSAUGA|RICHMOND\s+HILL|ON|ONTARIO)\b|\b(L\d[A-Z]\d)\b|\(\d{3}\)\s*\d{3}-\d{4}",
        )
        .unwrap()
    })
}

fn re_receipt_metadata_patterns() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)WS#|RECEIPT#|CASHIER|ITEM\s+COUNT|NUMBER\s+OF\s+ITEMS|HAPPY\s+SHOPPING|CREDIT\s+CARD|DEBIT|APPROVED|AUTH|REFERENCE|TERMINAL|CUSTOMER\s+COPY",
        )
        .unwrap()
    })
}

fn re_count_at_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\d+\s*@\s*\$?-?\d+\.\d{2}").unwrap())
}

fn re_weight_at_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d+\.?\d*\s*(?:lb|lk|kg|k[g9]|1b|1k)\s*@").unwrap())
}

fn re_multi_for_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\(?\d+\s*/\s*for\s+\$?\d+\.\d{2}\)?").unwrap())
}

fn re_compact_offer_fragment() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d+\s*@\s*\d+\s*/\s*\$?\d+\.\d{2}\b").unwrap())
}

fn re_parenthetical_offer_prefix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\([^)]+\)\s+\d+\s*/\s*for\b").unwrap())
}

fn re_section_header_with_aisle() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[^A-Z0-9]*\d{1,2}\s*[-:]\s*[A-Z]{3,}$").unwrap())
}

fn re_summary_patterns() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^(?:SUB\s*TOTAL|SUBTOTAL|TOTAL|HST|GST|PST|TAX|MASTER(?:CARD)?|VISA|DEBIT|CREDIT|POINTS|CASH|CHANGE|BALANCE|APPROVED|CARD|TERMINAL|MEMBER)\b",
        )
        .unwrap()
    })
}

fn re_tax_tokens() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(HST|GST|PST|TAX)\b").unwrap())
}

fn re_section_aisle_prefix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[^A-Z0-9]*\d{1,2}\s*[-:]").unwrap())
}

fn re_dept_marker_prefix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[&8]{2}\.?\s").unwrap())
}

fn re_total_ocr_variants() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"T[O0C]TA[L1I]").unwrap())
}

fn re_leading_section_item_prefix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^[^A-Z0-9]*\d{1,2}\s*[-:]\s*(MEAT|SEAFOOD|PRODUCE|DELI|GROCERY|BAKERY|FROZEN|FOOD)\b\s*",
        )
        .unwrap()
    })
}

fn re_ascii_words() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[A-Z]+").unwrap())
}

fn re_price_word() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Trailing `-` is Costco's convention for discount/refund lines
    // (e.g. TPD/<sku> 3.00-); LEADING `-` is Loblaws-family convention for
    // discount lines (e.g. "Member Pricing MRJ -1.49"). Either marks the
    // amount as negative. The optional trailing letters are tax flags that
    // can fuse with the price into a single OCR token: Costco's H/T/J and
    // T&T's G (GST) / P (PST), and T&T may print several space-separated
    // (e.g. "$6.87 G P").
    RE.get_or_init(|| Regex::new(r"^(-?)\$?(\d+\.\d{2})(-?)(?:\s*[HhTtJjGgPp])*$").unwrap())
}

fn re_embedded_trailing_price_word() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)[A-Z]{1,6}\$?(\d+\.\d{2})$").unwrap())
}

fn re_leading_qty_prefix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\(\d+\)\s*").unwrap())
}

fn re_leading_long_sku() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d{6,}[A-Za-z]?\s*").unwrap())
}

// Short numeric item codes (Costco prints 3-5 digit codes, e.g. "458 MILK 2%").
// Only stripped when followed by whitespace and more text, so a bare numeric
// line is left to the digits-only guards. Used for the description-quality
// (alpha-ratio) check, not for the displayed description.
fn re_leading_short_code() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d{3,5}\s+(?P<rest>\S)").unwrap())
}

// Scale weight-block lines on weighed-produce receipts: No Frills prints
// "0.985 kg Gross" / "-0.010 kg Tare =" / "0.975 kg Net @ $4.39/kg" under a
// single produce label, once per weighing. Never descriptions; a contiguous
// run of them means the label above is shared by several priced weighings.
// Prefix match: OCR mangles the tail freely ("Grosks", "Gros ed ...").
fn re_weight_info_line() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^-?\d+(?:[.,]\d+)?\s*kg\s+(?:gro|tare|net)").unwrap())
}

// "<weight> kg @ $<unit>/kg" with both numbers capturable, to check whether a
// weighed qty row's trailing price is actually its own line total.
fn re_weight_at_unit_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^\s*(\d+(?:\.\d+)?)\s*(?:kg|k9|kg\.|lb|1b|lk|1k)\s*@\s*\$?(\d+(?:\.\d+)?)")
            .unwrap()
    })
}

fn re_sale_marker() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\(SALE\)\s*").unwrap())
}

fn re_hed_marker() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\(HED[^)]*\)\s*").unwrap())
}

fn re_hhed_marker() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\(HHED[^)]*\)\s*").unwrap())
}

fn re_qty_price_marker() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"@?\d+/[A-Za-z]?\$?\d+\.\d{2}").unwrap())
}

fn re_qty_price_marker_2() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\d+/\$?\d+\.\d{2}").unwrap())
}

fn re_unit_price_marker() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\$\d+\.\d+/\w+").unwrap())
}

fn re_inline_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\$\d+\.\d{2}").unwrap())
}

fn re_garbled_price_artifact() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\d+s\d+\.\d+ea").unwrap())
}

fn re_cahrd() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bCAHRD\b").unwrap())
}

fn re_costco_discount_line() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Costco's "tier price discount" lines reference another SKU and are
    // therefore mostly digits ("TPD/1234567", "TPD/1234567/7"), which
    // fails the generic alpha-ratio filter in `is_valid_item_line`.
    // Allow embedded whitespace too — Costco OCR sometimes reads a digit
    // as a space (e.g. "TPD/1 96144" for "TPD/1796144"). Allow `TP[A-Z]/`
    // because OCR also occasionally reads the `D` as `U` etc.
    RE.get_or_init(|| Regex::new(r"^TP[A-Z]/[\d/\s]+$").unwrap())
}

fn re_hed_word() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bHED\b").unwrap())
}

fn re_leading_non_alnum() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[^A-Za-z0-9]+").unwrap())
}

fn re_trailing_non_alnum() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[^A-Za-z0-9)]+$").unwrap())
}

fn re_multi_spaces() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+").unwrap())
}

fn normalize_decimal_spacing(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'.' && i > 0 && bytes[i - 1].is_ascii_digit() {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j > i + 1
                && j + 1 < bytes.len()
                && bytes[j].is_ascii_digit()
                && bytes[j + 1].is_ascii_digit()
                && (j + 2 == bytes.len() || !bytes[j + 2].is_ascii_digit())
            {
                out.push('.');
                out.push(bytes[j] as char);
                out.push(bytes[j + 1] as char);
                i = j + 2;
                continue;
            }
        }
        // OCR sometimes reads a price's decimal point as a comma ("0,99").
        // Only treat a comma as a decimal point when it sits directly between
        // a digit and exactly two fraction digits, so thousands separators
        // ("1,000") and prose ("Anytown, ON") are left untouched.
        if bytes[i] == b','
            && i > 0
            && bytes[i - 1].is_ascii_digit()
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_digit()
            && bytes[i + 2].is_ascii_digit()
            && (i + 3 == bytes.len() || !bytes[i + 3].is_ascii_digit())
        {
            out.push('.');
            out.push(bytes[i + 1] as char);
            out.push(bytes[i + 2] as char);
            i += 3;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn parse_scaled_decimal(token: &str) -> Option<i64> {
    let trimmed = token.trim();
    let (whole, frac) = trimmed.split_once('.')?;
    if whole.is_empty() || frac.len() != 2 {
        return None;
    }
    if !whole.chars().all(|ch| ch.is_ascii_digit()) || !frac.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let whole_value = whole.parse::<i64>().ok()?;
    let frac_value = frac.parse::<i64>().ok()?;
    Some(whole_value * SCALE + frac_value * 100)
}

fn format_scaled_currency(value: i64) -> String {
    let abs_value = value.abs();
    let cents = abs_value / 100;
    let dollars = cents / 100;
    let rem = cents % 100;
    if value < 0 {
        format!("-{dollars}.{rem:02}")
    } else {
        format!("{dollars}.{rem:02}")
    }
}

fn alpha_ratio(value: &str) -> f64 {
    let non_ws_count = value.chars().filter(|ch| !ch.is_whitespace()).count();
    if non_ws_count == 0 {
        return 0.0;
    }
    let alpha_count = value.chars().filter(|ch| ch.is_ascii_alphabetic()).count();
    alpha_count as f64 / non_ws_count as f64
}

fn is_section_name(text: &str) -> bool {
    matches!(
        text,
        "MEAT" | "SEAFOOD" | "PRODUCE" | "DELI" | "GROCERY" | "BAKERY" | "FROZEN" | "FOOD"
    )
}

fn strip_leading_receipt_codes(text: &str) -> String {
    let trimmed = text.trim();
    let trimmed = re_leading_qty_prefix().replace(trimmed, "");
    let trimmed = re_leading_long_sku().replace(trimmed.as_ref(), "");
    let trimmed = re_leading_short_code().replace(trimmed.as_ref(), "$rest");
    let trimmed = re_leading_section_item_prefix().replace(trimmed.as_ref(), "");
    trimmed.trim().to_string()
}

fn is_section_header_text(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let normalized = re_multi_spaces()
        .replace(&text.trim().to_ascii_uppercase(), " ")
        .to_string();
    if re_dept_marker_prefix().is_match(&normalized) {
        return true;
    }
    if is_section_name(normalized.as_str()) {
        return true;
    }
    if re_section_header_with_aisle().is_match(&normalized) {
        return true;
    }
    if re_section_aisle_prefix().is_match(&normalized) {
        let remainder = re_section_aisle_prefix()
            .replace(&normalized, "")
            .trim()
            .to_string();
        let words = re_ascii_words()
            .find_iter(&remainder)
            .map(|m| m.as_str())
            .collect::<Vec<_>>();
        if words.len() == 1 && is_section_name(words[0]) {
            return true;
        }
    }
    false
}

fn is_summary_line(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let upper = text.trim().to_ascii_uppercase();
    // "Member Pricing" / "Manager's Special" rows on Loblaws-family receipts
    // are line-item discounts (negative price), not membership/store-info
    // metadata, so they must NOT match the `^MEMBER\b` arm of
    // re_summary_patterns. Without this carve-out the discount line is
    // filtered, the negative price is dropped, and the items sum overshoots
    // the printed subtotal (RCSS rcss_20260130 drops -$1.49 and -$0.98).
    if upper.starts_with("MEMBER PRICING")
        || upper.starts_with("MANAGER'S SPECIAL")
        || upper.starts_with("MANAGER SPECIAL")
    {
        return false;
    }
    if re_summary_patterns().is_match(&upper) {
        return true;
    }
    if upper.contains("SUBTOTAL") || upper.contains("SUB TOTAL") || upper.contains("TOTAL") {
        return true;
    }
    if re_total_ocr_variants().is_match(&upper) {
        return true;
    }
    if re_tax_tokens().is_match(&upper) {
        return true;
    }
    upper.starts_with("H=") && re_tax_tokens().is_match(&upper)
}

fn trailing_price_scaled(text: &str) -> Option<i64> {
    let normalized = normalize_decimal_spacing(text.trim());
    let captures = re_trailing_price().captures(&normalized)?;
    let value = parse_scaled_decimal(captures.get(1)?.as_str())?;
    let is_negative = captures.get(2).map(|m| m.as_str() == "-").unwrap_or(false);
    Some(if is_negative { -value } else { value })
}

fn line_has_trailing_price(text: &str) -> bool {
    trailing_price_scaled(text).is_some()
}

fn looks_like_onsale_marker(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let normalized = normalize_decimal_spacing(&text.trim().to_ascii_uppercase());
    let without_price = re_trailing_price().replace(&normalized, "").to_string();
    let compact: String = without_price
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    if compact.ends_with("ONSALE") || compact.ends_with("ONSAL") {
        let prefix_len = compact.len().saturating_sub(6);
        return prefix_len <= 3;
    }
    false
}

fn is_priced_generic_item_label(left_text: &str, full_text: &str) -> bool {
    if left_text.trim().is_empty() {
        return false;
    }
    line_has_trailing_price(full_text)
        && matches!(
            left_text.trim().to_ascii_uppercase().as_str(),
            "MEAT" | "BAKERY"
        )
}

fn parse_quantity_modifier(text: &str) -> bool {
    re_count_at_price().is_match(text)
        || re_weight_at_price().is_match(text)
        || re_multi_for_price().is_match(text)
}

fn looks_like_quantity_expression(text: &str) -> bool {
    let normalized = normalize_decimal_spacing(text.trim());
    if normalized.is_empty() {
        return false;
    }
    if parse_quantity_modifier(&normalized) {
        return true;
    }
    let upper = normalized.to_ascii_uppercase();
    if upper.starts_with('(') && upper.contains('@') && upper.contains("/$") {
        let alpha_count = upper.chars().filter(|ch| ch.is_ascii_alphabetic()).count();
        if alpha_count <= 2 {
            return true;
        }
    }
    if upper.contains('@') && upper.contains("/$") {
        let compact: String = upper
            .chars()
            .filter(|ch| !ch.is_ascii_whitespace())
            .collect();
        let alpha_count = compact
            .chars()
            .filter(|ch| ch.is_ascii_alphabetic())
            .count();
        let digit_count = compact.chars().filter(|ch| ch.is_ascii_digit()).count();
        if digit_count >= 3 && alpha_count <= 4 {
            return true;
        }
    }
    re_multi_for_price().is_match(&normalized)
        || re_compact_offer_fragment().is_match(&normalized)
        || re_parenthetical_offer_prefix().is_match(&normalized)
}

/// True when a line looks like the store's address / branch header.
///
/// The pattern alternates over bare tokens — `RD`, `ST`, `DR`, `ON` — so it
/// fires on any item whose description happens to contain one: `ON THE GO
/// BOTTLE`, `TNS RD HT BF BRR`, `5 ON CITRUS`, `YIN ON SWEETENED SOYA DRINK`.
/// Widening the token list is not the answer; `ON` and `DR` are ordinary
/// English (`DR PEPPER`), and no spelling of the alternation fixes that.
///
/// What separates the two cases is not the words but the **price**: the store
/// address is printed in the header/footer and never carries one, while an item
/// row that owns a trailing price is an item by construction. So the address
/// veto only applies to unpriced lines — see [`is_valid_item_line`], the sole
/// caller. Priced lines still face every other guard there (summary, section
/// header, metadata, alpha-ratio), which is what keeps `Tota1$ On Promotion
/// Item(*)16.93` out.
fn footer_address_like(text: &str) -> bool {
    if line_has_trailing_price(text) {
        return false;
    }
    re_footer_address_patterns().is_match(&text.to_ascii_uppercase())
}

fn receipt_metadata_like(text: &str) -> bool {
    re_receipt_metadata_patterns().is_match(text.trim())
}

fn clean_description(desc: &str) -> String {
    let mut cleaned = desc.to_string();
    cleaned = re_leading_qty_prefix().replace(&cleaned, "").to_string();
    cleaned = re_sale_marker().replace_all(&cleaned, "").to_string();
    cleaned = re_hed_marker().replace_all(&cleaned, "").to_string();
    cleaned = re_hhed_marker().replace_all(&cleaned, "").to_string();
    cleaned = re_qty_price_marker().replace_all(&cleaned, "").to_string();
    cleaned = re_qty_price_marker_2()
        .replace_all(&cleaned, "")
        .to_string();
    cleaned = re_unit_price_marker().replace_all(&cleaned, "").to_string();
    cleaned = re_inline_price().replace_all(&cleaned, "").to_string();
    cleaned = re_garbled_price_artifact()
        .replace_all(&cleaned, "")
        .to_string();
    cleaned = re_leading_section_item_prefix()
        .replace(&cleaned, "")
        .to_string();
    cleaned = re_cahrd().replace_all(&cleaned, "").to_string();
    cleaned = re_hed_word().replace_all(&cleaned, "").to_string();
    cleaned = re_leading_non_alnum().replace(&cleaned, "").to_string();
    cleaned = re_trailing_non_alnum().replace(&cleaned, "").to_string();
    cleaned = re_multi_spaces().replace_all(&cleaned, " ").to_string();
    cleaned.trim().to_string()
}

fn is_deposit_stub(text: &str) -> bool {
    let cleaned = clean_description(text);
    let upper = cleaned.to_ascii_uppercase();
    upper == "DEPOSIT" || upper.starts_with("DEPOSIT ")
}

fn lacks_description_context(text: &str) -> bool {
    let stripped = strip_leading_receipt_codes(text);
    stripped.is_empty() || alpha_ratio(&stripped) < 0.5
}

fn is_price_word(text: &str) -> Option<i64> {
    let normalized = normalize_decimal_spacing(text.trim());
    let stripped = normalized
        .strip_prefix('W')
        .map(str::trim_start)
        .or_else(|| normalized.strip_prefix('w').map(str::trim_start))
        .unwrap_or(normalized.as_str());
    if let Some(captures) = re_price_word().captures(stripped) {
        let value = parse_scaled_decimal(captures.get(2)?.as_str())?;
        let leading_minus = captures.get(1).map(|m| m.as_str() == "-").unwrap_or(false);
        let trailing_minus = captures.get(3).map(|m| m.as_str() == "-").unwrap_or(false);
        let is_negative = leading_minus || trailing_minus;
        return Some(if is_negative { -value } else { value });
    }
    if stripped.contains('@') || stripped.contains('/') {
        return None;
    }
    let captures = re_embedded_trailing_price_word().captures(stripped)?;
    parse_scaled_decimal(captures.get(1)?.as_str())
}

fn is_short_alpha_item(text: &str) -> bool {
    let letters_only: String = text.chars().filter(|ch| ch.is_ascii_alphabetic()).collect();
    letters_only.len() >= 3 && letters_only.chars().all(|ch| ch.is_ascii_alphabetic())
}

fn is_valid_onsale_target(line: &ParsedLine) -> bool {
    if line.left_text.is_empty() {
        return false;
    }
    if receipt_metadata_like(&line.left_text) || receipt_metadata_like(&line.full_text) {
        return false;
    }
    if is_summary_line(&line.left_text) || is_summary_line(&line.full_text) {
        return false;
    }
    if is_section_header_text(&line.left_text) || is_section_header_text(&line.full_text) {
        return false;
    }
    if looks_like_quantity_expression(&line.left_text) {
        return false;
    }
    if line_has_trailing_price(&line.full_text) {
        return false;
    }
    let stripped = strip_leading_receipt_codes(&line.left_text);
    !stripped.is_empty() && alpha_ratio(&stripped) >= 0.5
}

fn is_valid_item_line(line: &ParsedLine, total_line_y: Option<f64>) -> bool {
    let left_text_for_ratio = strip_leading_receipt_codes(&line.left_text);
    if left_text_for_ratio.is_empty() || line.left_text.is_empty() {
        return false;
    }
    if receipt_metadata_like(&line.left_text) || receipt_metadata_like(&line.full_text) {
        return false;
    }
    let short_alpha = is_short_alpha_item(&left_text_for_ratio);
    if line.left_text.len() < 5
        && !is_priced_generic_item_label(&line.left_text, &line.full_text)
        && !short_alpha
    {
        return false;
    }
    if let Some(total_y) = total_line_y {
        if line.line_y > total_y + Y_TOLERANCE {
            return false;
        }
    }
    if is_summary_line(&line.left_text) || is_summary_line(&line.full_text) {
        return false;
    }
    let left_is_header = is_section_header_text(&line.left_text)
        && !is_priced_generic_item_label(&line.left_text, &line.full_text);
    if left_is_header || is_section_header_text(&line.full_text) {
        return false;
    }
    if re_long_digits_only().is_match(&line.full_text) {
        return false;
    }
    let is_costco_discount = re_costco_discount_line().is_match(&left_text_for_ratio);
    if !is_costco_discount && alpha_ratio(&left_text_for_ratio) < 0.5 {
        return false;
    }
    if re_malformed_ocr_prefix().is_match(&line.left_text) {
        return false;
    }
    if re_mangled_reg_marker().is_match(line.left_text.trim()) {
        return false;
    }
    if line.left_text.len() < 8
        && !line.left_text.contains(' ')
        && !is_priced_generic_item_label(&line.left_text, &line.full_text)
        && !short_alpha
    {
        return false;
    }
    if footer_address_like(&line.full_text) {
        return false;
    }
    if looks_like_onsale_marker(&line.left_text) {
        return false;
    }
    if re_multibuy_parenthetical().is_match(&line.left_text) {
        return false;
    }
    if re_short_parenthetical_code().is_match(&line.left_text)
        && line.left_text.len() < 12
        && !is_short_alpha_item(&clean_description(&line.left_text))
    {
        // "(4001)"-style code stubs are not items, but a short name behind a
        // strippable promo marker — e.g. T&T's "(SALE) NAPA" — still is.
        return false;
    }
    if re_weight_info_line().is_match(line.left_text.trim()) {
        return false;
    }
    true
}

/// Whether a weighed qty row's own math (`<w> kg @ $<u>/kg`) reproduces
/// `price_scaled`. `None` when the row isn't that shape. `Some(false)` means
/// the trailing price drifted in from another row during line grouping.
fn weight_row_price_reconciles(left_text: &str, price_scaled: i64) -> Option<bool> {
    let captures = re_weight_at_unit_price().captures(left_text.trim())?;
    let weight: f64 = captures.get(1)?.as_str().parse().ok()?;
    let unit: f64 = captures.get(2)?.as_str().parse().ok()?;
    let expected_cents = (weight * unit * 100.0).round() as i64;
    let price_cents = (price_scaled as f64 / 100.0).round() as i64;
    Some((expected_cents - price_cents).abs() <= 1)
}

fn has_nearby_quantity_expression_above(all_lines: &[ParsedLine], line_index: usize) -> bool {
    let anchor_y = all_lines[line_index].line_y;
    all_lines
        .iter()
        .enumerate()
        .filter(|(index, candidate)| {
            *index != line_index
                && candidate.line_y < anchor_y
                && anchor_y - candidate.line_y <= MAX_ITEM_DISTANCE
        })
        .max_by(|(_, left), (_, right)| left.line_y.partial_cmp(&right.line_y).unwrap())
        .is_some_and(|(_, candidate)| looks_like_quantity_expression(&candidate.left_text))
}

fn nearest_unpriced_deposit_stub_below(
    all_lines: &[ParsedLine],
    line_index: usize,
    used_line_indices: &[bool],
) -> Option<(usize, f64)> {
    let anchor_y = all_lines[line_index].line_y;
    let nearest_below = all_lines
        .iter()
        .enumerate()
        .filter(|(index, candidate)| {
            *index != line_index
                && candidate.line_y > anchor_y
                && candidate.line_y - anchor_y <= MAX_ITEM_DISTANCE
        })
        .min_by(|(_, left), (_, right)| left.line_y.partial_cmp(&right.line_y).unwrap())?;
    let (index, candidate) = nearest_below;
    if used_line_indices[index]
        || !is_deposit_stub(&candidate.left_text)
        || line_has_trailing_price(&candidate.full_text)
    {
        return None;
    }
    Some((index, candidate.line_y - anchor_y))
}

fn y_center(word: &WordInput) -> f64 {
    (word.bbox.top + word.bbox.bottom) / 2.0
}

fn x_center(word: &WordInput) -> f64 {
    (word.bbox.left + word.bbox.right) / 2.0
}

pub fn extract_spatial_items(pages: Vec<PageInput>) -> SpatialExtractionOutcome {
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    if pages.is_empty() {
        return SpatialExtractionOutcome { items, warnings };
    }

    let mut all_lines = Vec::new();
    let mut price_candidates = Vec::new();

    for page in &pages {
        for line in &page.lines {
            if line.words.is_empty() {
                continue;
            }
            let full_text = line.text.clone();
            let line_has_price = line_has_trailing_price(&full_text);
            let mut left_words = Vec::new();
            let mut left_y = None;
            for word in &line.words {
                let x = x_center(word);
                // PRICE_X_THRESHOLD is the description/price boundary;
                // there's no dead zone (Costco's "2% 4L" pack-size token
                // sits at cx≈0.6 and must count as description text).
                if x < PRICE_X_THRESHOLD {
                    let text = word.text.as_str();
                    if text.len() <= 1 || re_digits_dots_only().is_match(text) {
                        continue;
                    }
                    if is_section_header_text(text) && !line_has_price {
                        continue;
                    }
                    left_words.push(text.to_string());
                    if left_y.is_none() {
                        left_y = Some(y_center(word));
                    }
                }
            }
            let line_y = left_y.unwrap_or_else(|| y_center(&line.words[0]));
            let line_index = all_lines.len();
            all_lines.push(ParsedLine {
                line_y,
                full_text: full_text.clone(),
                left_text: left_words.join(" "),
            });
            for word in &line.words {
                if word.confidence < MIN_CONFIDENCE {
                    continue;
                }
                let x = x_center(word);
                if x <= PRICE_X_THRESHOLD {
                    continue;
                }
                if let Some(price_scaled) = is_price_word(&word.text) {
                    if price_scaled != 0 {
                        price_candidates.push(PriceCandidate {
                            price_y: y_center(word),
                            price_scaled,
                            source_line_index: line_index,
                        });
                    }
                }
            }
        }
    }

    let total_line_y = all_lines
        .iter()
        .filter(|line| {
            let upper = line.full_text.to_ascii_uppercase();
            upper.contains("TOTAL")
                && !upper.contains("SUBTOTAL")
                && !upper.contains("TOTAL NUMBER")
                && !upper.contains("TOTAL DISCOUNT")
                && !upper.contains("TOTAL ITEMS")
                && !upper.contains("TOTAL SAVINGS")
                && !upper.contains("TOTAL SAVED")
        })
        .map(|line| line.line_y)
        .min_by(|a, b| a.partial_cmp(b).unwrap());

    let mut used_line_indices = vec![false; all_lines.len()];

    for price_candidate in price_candidates {
        let price_y = price_candidate.price_y;
        if let Some(total_y) = total_line_y {
            if price_y > total_y + Y_TOLERANCE {
                continue;
            }
        }
        if all_lines.is_empty() {
            continue;
        }

        let closest_line_index = all_lines
            .iter()
            .enumerate()
            .min_by(|(_, left), (_, right)| {
                (left.line_y - price_y)
                    .abs()
                    .partial_cmp(&(right.line_y - price_y).abs())
                    .unwrap()
            })
            .map(|(index, _)| index);
        let Some(closest_line_index) = closest_line_index else {
            continue;
        };
        let source_line = &all_lines[price_candidate.source_line_index];
        let closest_line = &all_lines[closest_line_index];

        let context_full_text = if source_line.full_text.is_empty() {
            &closest_line.full_text
        } else {
            &source_line.full_text
        };
        let context_left_text = if source_line.left_text.is_empty() {
            &closest_line.left_text
        } else {
            &source_line.left_text
        };
        let full_upper = context_full_text.to_ascii_uppercase();
        let price_line_has_onsale = looks_like_onsale_marker(&full_upper);
        let left_is_header = is_section_header_text(context_left_text)
            && !is_priced_generic_item_label(context_left_text, context_full_text);
        let mut prefer_below = left_is_header
            || is_section_header_text(context_full_text)
            || context_left_text.is_empty();
        if price_line_has_onsale {
            prefer_below = true;
        }

        let mut is_summary = false;
        if let Some(total_y) = total_line_y {
            if price_y > total_y - MAX_ITEM_DISTANCE {
                for candidate in &all_lines {
                    if (candidate.line_y - price_y).abs() > Y_TOLERANCE {
                        continue;
                    }
                    if candidate.line_y > price_y + SPATIAL_FLOAT_EPSILON {
                        continue;
                    }
                    if is_summary_line(&candidate.left_text)
                        || is_summary_line(&candidate.full_text)
                    {
                        is_summary = true;
                        break;
                    }
                }
            }
        }

        if !is_summary {
            let full_text_stripped = closest_line.full_text.trim();
            if is_summary_line(&closest_line.left_text) || is_summary_line(&closest_line.full_text)
            {
                is_summary = true;
            } else if re_standalone_price().is_match(full_text_stripped) {
                let nearest_above = all_lines
                    .iter()
                    .enumerate()
                    .filter(|(_, candidate)| candidate.line_y < closest_line.line_y)
                    .max_by(|(_, left), (_, right)| {
                        left.line_y.partial_cmp(&right.line_y).unwrap()
                    });
                if let Some((_, above)) = nearest_above {
                    if closest_line.line_y - above.line_y <= MAX_ITEM_DISTANCE
                        && (is_summary_line(&above.left_text) || is_summary_line(&above.full_text))
                    {
                        is_summary = true;
                    }
                }
                if !is_summary {
                    if let Some(total_y) = total_line_y {
                        if closest_line.line_y > total_y - MAX_ITEM_DISTANCE {
                            for candidate in &all_lines {
                                if (candidate.line_y - closest_line.line_y).abs()
                                    > MAX_ITEM_DISTANCE
                                {
                                    continue;
                                }
                                if is_summary_line(&candidate.left_text)
                                    || is_summary_line(&candidate.full_text)
                                {
                                    is_summary = true;
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut onsale_target_line_index = None;
        if !is_summary && price_line_has_onsale {
            let anchor_y = source_line.line_y;
            let nearest_above = all_lines
                .iter()
                .enumerate()
                .filter(|(_, candidate)| {
                    candidate.line_y < anchor_y
                        && anchor_y - candidate.line_y <= MAX_ITEM_DISTANCE
                        && is_valid_onsale_target(candidate)
                })
                .max_by(|(_, left), (_, right)| left.line_y.partial_cmp(&right.line_y).unwrap());
            let nearest_below = all_lines
                .iter()
                .enumerate()
                .filter(|(_, candidate)| {
                    candidate.line_y > anchor_y
                        && candidate.line_y - anchor_y <= MAX_ITEM_DISTANCE
                        && is_valid_onsale_target(candidate)
                })
                .min_by(|(_, left), (_, right)| left.line_y.partial_cmp(&right.line_y).unwrap());
            match (nearest_above, nearest_below) {
                (Some((above_index, above)), Some((below_index, below))) => {
                    let above_distance = anchor_y - above.line_y;
                    let below_distance = below.line_y - anchor_y;
                    onsale_target_line_index = Some(if above_distance <= below_distance {
                        above_index
                    } else {
                        below_index
                    });
                }
                (Some((index, _)), None) | (None, Some((index, _))) => {
                    onsale_target_line_index = Some(index);
                }
                (None, None) => {
                    is_summary = true;
                }
            }
        }

        if is_summary {
            continue;
        }

        let line_selection_candidates = all_lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                crate::spatial::SpatialLineCandidate::new(
                    line.line_y,
                    used_line_indices[index],
                    is_valid_item_line(line, total_line_y),
                    line_has_trailing_price(&line.full_text),
                    looks_like_quantity_expression(&line.left_text),
                )
            })
            .collect::<Vec<_>>();

        let mut found_item = false;
        let mut chosen_line_index = None;
        let mut chosen_distance = f64::INFINITY;
        let mut suppress_fallback_for_ambiguous_code_only_source = false;
        let selection_anchor_y = source_line.line_y;
        let source_line_is_quantity_expression =
            looks_like_quantity_expression(&source_line.left_text);
        let source_line_needs_item_context = lacks_description_context(&source_line.left_text);
        let source_line_repeats_previous_priced_item = source_line_needs_item_context
            && all_lines
                .iter()
                .enumerate()
                .filter(|(_, candidate)| {
                    candidate.line_y < selection_anchor_y
                        && selection_anchor_y - candidate.line_y <= MAX_ITEM_DISTANCE
                        && is_valid_item_line(candidate, total_line_y)
                        && line_has_trailing_price(&candidate.full_text)
                        && trailing_price_scaled(&candidate.full_text)
                            == Some(price_candidate.price_scaled)
                })
                .max_by(|(_, left), (_, right)| left.line_y.partial_cmp(&right.line_y).unwrap())
                .is_some();

        // A priced scale-weight row belongs to the produce label sitting above
        // its contiguous weight block, however tall the block is (No Frills
        // prints four gross/tare/net weighings under one "CHERRIES RED" label).
        // Content-verified, so it bypasses the geometric distance gates, and
        // the label is deliberately reusable across the block's weighings.
        if re_weight_info_line().is_match(source_line.left_text.trim()) {
            let mut j = price_candidate.source_line_index;
            while j > 0 {
                j -= 1;
                let candidate = &all_lines[j];
                let candidate_text = candidate.left_text.trim();
                if candidate_text.is_empty()
                    || re_weight_info_line().is_match(candidate_text)
                    || looks_like_quantity_expression(candidate_text)
                {
                    continue;
                }
                let cleaned = strip_leading_receipt_codes(candidate_text);
                if !is_section_header_text(candidate_text)
                    && !is_summary_line(candidate_text)
                    && !is_summary_line(&candidate.full_text)
                    && !line_has_trailing_price(&candidate.full_text)
                    && !cleaned.is_empty()
                    && alpha_ratio(&cleaned) >= 0.5
                {
                    chosen_line_index = Some(j);
                    chosen_distance = 0.0;
                }
                break;
            }
        }

        if source_line_is_quantity_expression && chosen_line_index.is_none() {
            let source_modifier = parse_quantity_modifier(&source_line.left_text);
            let mut nearest_unpriced_above = None;
            let mut nearest_unpriced_below = None;
            let mut nearest_priced_below_with_deposit_stub = None;
            // Deposit stubs (e.g. "DEPOSIT 1") are normally skipped so a regular
            // quantity expression like "3@$3.49" doesn't pair with a deposit label
            // above it.  But when a quantity expression IS for a deposit (e.g.
            // "2@$0.10 0.20"), the deposit stub immediately above IS the correct
            // target.  Track the closest unused deposit stub within Y_TOLERANCE so
            // we can fall back to it when no regular item is found above.
            let mut nearest_deposit_stub_above_within_tolerance: Option<(usize, f64)> = None;

            for (index, candidate) in all_lines.iter().enumerate() {
                if used_line_indices[index] || !is_valid_item_line(candidate, total_line_y) {
                    continue;
                }

                let distance = (candidate.line_y - selection_anchor_y).abs();
                if distance > MAX_ITEM_DISTANCE + SPATIAL_FLOAT_EPSILON {
                    continue;
                }

                let candidate_has_trailing_price = line_has_trailing_price(&candidate.full_text);
                if candidate_has_trailing_price {
                    if candidate.line_y > selection_anchor_y
                        && nearest_unpriced_deposit_stub_below(
                            &all_lines,
                            index,
                            &used_line_indices,
                        )
                        .is_some()
                    {
                        match nearest_priced_below_with_deposit_stub {
                            Some((_, current_distance)) if distance >= current_distance => {}
                            _ => nearest_priced_below_with_deposit_stub = Some((index, distance)),
                        }
                    }
                    continue;
                }

                if is_deposit_stub(&candidate.left_text) {
                    // Track closest deposit stub above within Y_TOLERANCE as a
                    // fallback for deposit-quantity expressions.
                    if candidate.line_y < selection_anchor_y
                        && distance <= Y_TOLERANCE + SPATIAL_FLOAT_EPSILON
                    {
                        match nearest_deposit_stub_above_within_tolerance {
                            Some((_, current_distance)) if distance >= current_distance => {}
                            _ => {
                                nearest_deposit_stub_above_within_tolerance =
                                    Some((index, distance))
                            }
                        }
                    }
                    continue;
                }

                if candidate.line_y < selection_anchor_y {
                    match nearest_unpriced_above {
                        Some((_, current_distance)) if distance >= current_distance => {}
                        _ => nearest_unpriced_above = Some((index, distance)),
                    }
                } else if candidate.line_y > selection_anchor_y {
                    match nearest_unpriced_below {
                        Some((_, current_distance)) if distance >= current_distance => {}
                        _ => nearest_unpriced_below = Some((index, distance)),
                    }
                }
            }

            // A weighed qty row usually sits under its item label, so with a
            // real quantity modifier the label above wins. But when the row's
            // own math does not reproduce the trailing price, that price
            // drifted in from another row during line grouping (No Frills'
            // "1.775 kg @ $1.52/kg" carrying the 5.99 of the melon below), so
            // fall back to plain nearest-line resolution.
            let own_price =
                weight_row_price_reconciles(&source_line.left_text, price_candidate.price_scaled);
            chosen_line_index = match (
                nearest_unpriced_above,
                nearest_unpriced_below,
                source_modifier && own_price != Some(false),
            ) {
                (Some((index, distance)), Some(_), true) => {
                    chosen_distance = distance;
                    Some(index)
                }
                (
                    Some((above_index, above_distance)),
                    Some((below_index, below_distance)),
                    false,
                ) => {
                    if above_distance <= below_distance {
                        chosen_distance = above_distance;
                        Some(above_index)
                    } else {
                        chosen_distance = below_distance;
                        Some(below_index)
                    }
                }
                (Some((index, distance)), None, _) => {
                    chosen_distance = distance;
                    Some(index)
                }
                // No regular item above: prefer a deposit stub within Y_TOLERANCE
                // over a non-deposit item below, so "2@$0.10" pairs with "DEPOSIT 1"
                // rather than the next real item below.
                (None, Some((below_index, below_distance)), _) => {
                    if let Some((stub_index, stub_distance)) =
                        nearest_deposit_stub_above_within_tolerance
                    {
                        chosen_distance = stub_distance;
                        Some(stub_index)
                    } else {
                        chosen_distance = below_distance;
                        Some(below_index)
                    }
                }
                (None, None, _) => nearest_priced_below_with_deposit_stub
                    .or(nearest_deposit_stub_above_within_tolerance)
                    .map(|(index, distance)| {
                        chosen_distance = distance;
                        index
                    }),
            };
        }

        if !prefer_below && source_line_is_quantity_expression {
            let mut nearest_same_row_above = None;
            let mut nearest_same_row_below = None;

            for (index, candidate) in all_lines.iter().enumerate() {
                if used_line_indices[index] || !is_valid_item_line(candidate, total_line_y) {
                    continue;
                }
                let distance = (candidate.line_y - selection_anchor_y).abs();
                if distance > Y_TOLERANCE + SPATIAL_FLOAT_EPSILON {
                    continue;
                }
                if candidate.line_y < selection_anchor_y {
                    match nearest_same_row_above {
                        Some(current_distance) if distance >= current_distance => {}
                        _ => nearest_same_row_above = Some(distance),
                    }
                } else if candidate.line_y > selection_anchor_y {
                    match nearest_same_row_below {
                        Some(current_distance) if distance >= current_distance => {}
                        _ => nearest_same_row_below = Some(distance),
                    }
                }
            }

            if nearest_same_row_below.is_some() && nearest_same_row_above.is_none() {
                prefer_below = true;
            }
        }

        let source_distance = (source_line.line_y - price_y).abs();
        let shifted_deposit_target = if source_distance <= Y_TOLERANCE
            && is_valid_item_line(source_line, total_line_y)
            && !looks_like_quantity_expression(&source_line.left_text)
            && has_nearby_quantity_expression_above(&all_lines, price_candidate.source_line_index)
        {
            nearest_unpriced_deposit_stub_below(
                &all_lines,
                price_candidate.source_line_index,
                &used_line_indices,
            )
        } else {
            None
        };

        if onsale_target_line_index.is_none()
            && !used_line_indices[price_candidate.source_line_index]
        {
            if shifted_deposit_target.is_none()
                && trailing_price_scaled(&source_line.full_text)
                    == Some(price_candidate.price_scaled)
                && is_valid_item_line(source_line, total_line_y)
                && !looks_like_quantity_expression(&source_line.left_text)
            {
                chosen_line_index = Some(price_candidate.source_line_index);
                chosen_distance = source_distance;
            } else if let Some((index, distance)) = shifted_deposit_target {
                chosen_line_index = Some(index);
                chosen_distance = distance;
            } else if source_distance <= Y_TOLERANCE
                && is_valid_item_line(source_line, total_line_y)
                && !looks_like_quantity_expression(&source_line.left_text)
            {
                chosen_line_index = Some(price_candidate.source_line_index);
                chosen_distance = source_distance;
            }
        }

        if chosen_line_index.is_none() {
            if let Some(index) = onsale_target_line_index {
                if !used_line_indices[index] {
                    chosen_line_index = Some(index);
                    chosen_distance = (all_lines[index].line_y - price_y).abs();
                }
            }
        }

        if chosen_line_index.is_none() {
            if let Some((index, distance)) = crate::spatial::select_spatial_item_line(
                selection_anchor_y,
                Y_TOLERANCE,
                MAX_ITEM_DISTANCE,
                prefer_below,
                price_line_has_onsale,
                line_selection_candidates,
            ) {
                let selected_line = &all_lines[index];
                let selected_line_is_next_priced_row = source_line_needs_item_context
                    && selected_line.line_y > price_y + SPATIAL_FLOAT_EPSILON
                    && line_has_trailing_price(&selected_line.full_text);
                if selected_line_is_next_priced_row {
                    suppress_fallback_for_ambiguous_code_only_source = true;
                } else {
                    chosen_line_index = Some(index);
                    chosen_distance = distance;
                }
            }
        }

        if let Some(index) = chosen_line_index {
            let direct_match_tolerance = if source_line_is_quantity_expression || prefer_below {
                MAX_ITEM_DISTANCE + SPATIAL_FLOAT_EPSILON
            } else {
                Y_TOLERANCE + SPATIAL_FLOAT_EPSILON
            };
            if chosen_distance <= direct_match_tolerance {
                let description = clean_description(&all_lines[index].left_text);
                if description.len() > 2
                    && !re_mangled_reg_marker().is_match(all_lines[index].left_text.trim())
                    && !re_mangled_reg_marker().is_match(description.trim())
                {
                    used_line_indices[index] = true;
                    items.push(SpatialExtractedItem {
                        description,
                        price_scaled: price_candidate.price_scaled,
                    });
                    found_item = true;
                }
            }
        }

        if !found_item
            && shifted_deposit_target.is_none()
            && !used_line_indices[price_candidate.source_line_index]
            && trailing_price_scaled(&source_line.full_text) == Some(price_candidate.price_scaled)
            && is_valid_item_line(source_line, total_line_y)
            && !looks_like_quantity_expression(&source_line.left_text)
        {
            let description = clean_description(&source_line.left_text);
            if description.len() > 2 && !re_mangled_reg_marker().is_match(description.trim()) {
                used_line_indices[price_candidate.source_line_index] = true;
                items.push(SpatialExtractedItem {
                    description,
                    price_scaled: price_candidate.price_scaled,
                });
                found_item = true;
            }
        }

        if !found_item && !suppress_fallback_for_ambiguous_code_only_source {
            if source_line_repeats_previous_priced_item {
                continue;
            }
            let mut lines_above = all_lines
                .iter()
                .enumerate()
                .filter(|(_, line)| {
                    line.line_y < price_y - Y_TOLERANCE
                        && (price_y - line.line_y) <= MAX_ITEM_DISTANCE
                })
                .collect::<Vec<_>>();
            lines_above
                .sort_by(|(_, left), (_, right)| right.line_y.partial_cmp(&left.line_y).unwrap());

            for (index, line) in lines_above.into_iter().take(5) {
                if used_line_indices[index] {
                    continue;
                }
                if price_line_has_onsale && line_has_trailing_price(&line.full_text) {
                    continue;
                }
                if line.left_text.len() < 3 {
                    continue;
                }
                if is_summary_line(&line.left_text) || is_summary_line(&line.full_text) {
                    continue;
                }
                if re_weight_info().is_match(&line.full_text.to_ascii_lowercase()) {
                    continue;
                }
                if re_w_dollar().is_match(&line.full_text) {
                    continue;
                }
                if re_standalone_price().is_match(line.full_text.trim()) {
                    continue;
                }
                let left_is_header = is_section_header_text(&line.left_text)
                    && !is_priced_generic_item_label(&line.left_text, &line.full_text);
                if left_is_header || is_section_header_text(&line.full_text) {
                    continue;
                }
                let left_text_for_ratio = strip_leading_receipt_codes(&line.left_text);
                if left_text_for_ratio.is_empty() {
                    continue;
                }
                let is_costco_discount = re_costco_discount_line().is_match(&left_text_for_ratio);
                if !is_costco_discount && alpha_ratio(&left_text_for_ratio) < 0.4 {
                    continue;
                }
                let description = clean_description(&line.left_text);
                if description.len() > 2 && !re_mangled_reg_marker().is_match(description.trim()) {
                    used_line_indices[index] = true;
                    items.push(SpatialExtractedItem {
                        description,
                        price_scaled: price_candidate.price_scaled,
                    });
                    found_item = true;
                    break;
                }
            }
        }

        if !found_item {
            let mut context_text = source_line.full_text.trim().to_string();
            if context_text.is_empty() {
                context_text = closest_line.full_text.trim().to_string();
            }
            // Truncate by characters, not bytes: byte 80 may split a multibyte
            // CJK char (Asian-grocery receipts) and panic.
            if let Some((byte_idx, _)) = context_text.char_indices().nth(80) {
                context_text.truncate(byte_idx);
            }
            let mut message = format!(
                "maybe missed item near price {}",
                format_scaled_currency(price_candidate.price_scaled)
            );
            if !context_text.is_empty() {
                message.push_str(&format!(" (context: \"{}\")", context_text));
            }
            warnings.push(SpatialParserWarning {
                message,
                after_item_index: if items.is_empty() {
                    None
                } else {
                    Some(items.len() - 1)
                },
            });
        }
    }

    SpatialExtractionOutcome { items, warnings }
}

#[cfg(test)]
mod tests {
    // Word bounding boxes below are real OCR-derived, normalized coordinates;
    // some (e.g. 0.318) land near a math constant (1/π) purely by coincidence.
    #![allow(clippy::approx_constant)]

    use super::{
        extract_spatial_items, footer_address_like, is_price_word, BboxInput, LineInput, PageInput,
        WordInput,
    };

    #[test]
    fn address_veto_spares_item_rows_that_own_a_price() {
        // The pattern alternates over bare tokens, so ordinary items trip it:
        // No Frills' "ON THE GO BOTTLE" (ON) and "TNS RD HT BF BRR" (RD), plus
        // T&T's "YIN ON SWEETENED SOYA DRINK". A priced row is an item by
        // construction, so the veto must not apply to it.
        for priced in [
            "03760401787 ON THE GO BOTTLE HMRJ 5.00",
            "07960651131 TNS RD HT BF BRR MRJ 1.69",
            "YIN ON SWEETENED SOYA DRINK 4.47",
            "DR PEPPER 2.99",
        ] {
            assert!(
                !footer_address_like(priced),
                "priced item row must survive the address veto: {priced:?}"
            );
        }

        // Unpriced header/footer address lines are still vetoed — that is the
        // whole reason the check exists.
        for address in [
            "7075 MARKHAM RD, MARKHAM, ON, L3S 3J9",
            "5762 HWY 7 E, MARKHAM",
            "Scarborough, ON",
            "1 Yorktech Dr",
        ] {
            assert!(
                footer_address_like(address),
                "unpriced address line must still be vetoed: {address:?}"
            );
        }
    }

    #[test]
    fn parses_tt_price_with_gst_pst_tax_flags() {
        // T&T prints GST/PST flags after the price, sometimes several
        // space-separated (e.g. "W $6.87 G P"). Costco's H/T/J must still work.
        assert_eq!(is_price_word("W $6.87 G P"), Some(68_700));
        assert_eq!(is_price_word("W $13.97"), Some(139_700));
        assert_eq!(is_price_word("6.87 G"), Some(68_700));
        assert_eq!(is_price_word("5.00- H"), Some(-50_000));
    }

    fn word(text: &str, left: f64, top: f64, right: f64, bottom: f64) -> WordInput {
        WordInput {
            text: text.to_string(),
            bbox: BboxInput {
                left,
                top,
                right,
                bottom,
            },
            confidence: 0.99,
        }
    }

    #[test]
    fn keeps_short_produce_name_alignment() {
        let page = PageInput {
            lines: vec![
                LineInput {
                    text: "&& 02-Vegetable".to_string(),
                    words: vec![word("&& 02-Vegetable", 0.15, 0.355, 0.30, 0.364)],
                },
                LineInput {
                    text: "Napa".to_string(),
                    words: vec![word("Napa", 0.06, 0.365, 0.09, 0.372)],
                },
                LineInput {
                    text: "2.46 1b @ $1.29/1b 3.17".to_string(),
                    words: vec![
                        word("2.46 1b @ $1.29/1b", 0.20, 0.378, 0.27, 0.386),
                        word("3.17", 0.89, 0.377, 0.92, 0.384),
                    ],
                },
                LineInput {
                    text: "Soybean Sprout".to_string(),
                    words: vec![word("Soybean Sprout", 0.12, 0.388, 0.24, 0.395)],
                },
                LineInput {
                    text: "0.65 1b @ $1.58/1b 1.03".to_string(),
                    words: vec![
                        word("0.65 1b @ $1.58/1b", 0.21, 0.401, 0.28, 0.409),
                        word("1.03", 0.89, 0.400, 0.92, 0.407),
                    ],
                },
            ],
        };

        let outcome = extract_spatial_items(vec![page]);
        let observed = outcome
            .items
            .into_iter()
            .map(|item| (item.description, item.price_scaled))
            .collect::<Vec<_>>();

        assert!(observed.contains(&("Napa".to_string(), 31_700)));
        assert!(observed.contains(&("Soybean Sprout".to_string(), 10_300)));
    }

    #[test]
    fn keeps_sale_marker_short_produce_name_as_qty_row_target() {
        // T&T 2026-06-29_t_t_supermarket_49_59: "(SALE) NAPA" is 11 chars, so
        // the short-parenthetical stub check rejected it as an item line. Its
        // weight-row price $4.75 then skipped to "(SALE) STRAWBERRY" below,
        // and strawberry's own $5.00 cascaded onto the garbled "TMERE" line.
        let page = PageInput {
            lines: vec![
                LineInput {
                    text: "PRODUCE".to_string(),
                    words: vec![word("PRODUCE", 0.05, 0.326, 0.16, 0.337)],
                },
                LineInput {
                    text: "(SALE) NAPA".to_string(),
                    words: vec![word("(SALE) NAPA", 0.05, 0.336, 0.22, 0.347)],
                },
                LineInput {
                    text: "2.200 kg @ $2.16/kg W $4.75".to_string(),
                    words: vec![
                        word("2.200 kg @ $2.16/kg", 0.06, 0.345, 0.36, 0.359),
                        word("W $4.75", 0.72, 0.345, 0.83, 0.359),
                    ],
                },
                LineInput {
                    text: "(SALE) STRAWBERRY".to_string(),
                    words: vec![word("(SALE) STRAWBERRY", 0.06, 0.376, 0.31, 0.387)],
                },
                LineInput {
                    text: "594143 2 @2/$5.00 W $5.00".to_string(),
                    words: vec![
                        word("594143 2 @2/$5.00", 0.06, 0.385, 0.36, 0.398),
                        word("W $5.00", 0.72, 0.385, 0.83, 0.398),
                    ],
                },
                LineInput {
                    text: "DELI".to_string(),
                    words: vec![word("DELI", 0.05, 0.414, 0.13, 0.426)],
                },
                LineInput {
                    text: "T&T PRESERVED DUCK EGGS W $5.99".to_string(),
                    words: vec![
                        word("T&T PRESERVED DUCK EGGS", 0.06, 0.424, 0.41, 0.436),
                        word("W $5.99", 0.72, 0.424, 0.83, 0.436),
                    ],
                },
                LineInput {
                    text: "TMERE".to_string(),
                    words: vec![word("TMERE", 0.06, 0.434, 0.27, 0.448)],
                },
            ],
        };

        let outcome = extract_spatial_items(vec![page]);
        let observed = outcome
            .items
            .into_iter()
            .map(|item| (item.description, item.price_scaled))
            .collect::<Vec<_>>();

        assert!(
            observed.contains(&("NAPA".to_string(), 47_500)),
            "{observed:?}"
        );
        assert!(
            observed.contains(&("STRAWBERRY".to_string(), 50_000)),
            "{observed:?}"
        );
        assert!(
            observed.contains(&("T&T PRESERVED DUCK EGGS".to_string(), 59_900)),
            "{observed:?}"
        );
        assert!(
            !observed.iter().any(|(desc, _)| desc.contains("TMERE")),
            "{observed:?}"
        );
    }

    #[test]
    fn weight_block_prices_reuse_produce_label_and_drifted_qty_price_pairs_nearest() {
        // No Frills 2026-07-02_no_frills_52_16: one "CHERRIES RED" label with
        // four gross/tare/net weighings (each priced), and a banana qty row
        // that absorbed the melon's 5.99 during line grouping (its own math
        // says 1.775 x 1.52 = 2.70).
        fn row(text: &str, y: f64, price: Option<&str>) -> LineInput {
            let mut words = vec![word(text, 0.06, y, 0.45, y + 0.012)];
            let mut full = text.to_string();
            if let Some(p) = price {
                words.push(word(p, 0.80, y, 0.90, y + 0.012));
                full = format!("{text} {p}");
            }
            LineInput { text: full, words }
        }
        let page = PageInput {
            lines: vec![
                row("4011 BANANA MRJ", 0.300, Some("2.70")),
                row("1.775 kg @ $1.52/kg", 0.317, Some("5.99")),
                row("4032 WMELON RED SOLS MRJ", 0.334, None),
                row("4045 CHERRIES RED MRJ", 0.351, None),
                row("0.985 kg Grosks", 0.368, None),
                row("-0.010 kg Tare =", 0.385, Some("4.28")),
                row("0.975 kg Net @ $4.39/kg", 0.402, None),
                row("1.035 kg Gross", 0.419, None),
                row("-0.010 kg Tare =", 0.436, Some("4.50")),
                row("1.025 kg Net @ $4.39/k9", 0.453, None),
                row("1.105 kg Gross", 0.470, None),
                row("-0.010 kg Tare =", 0.487, Some("4.81")),
                row("1.095 kg Net @ $4.39/k9", 0.504, None),
                row("1.020 kg Gros ed", 0.521, None),
                row("-0.010 kg Tare =", 0.538, None),
                row("1.010 kg Net @ $4.39/kg", 0.555, Some("4.43")),
                row("4403 PEACH YELLOW MRJ", 0.572, None),
                row("0.900 kg @ $2.18/kg", 0.589, Some("1.96")),
            ],
        };

        let outcome = extract_spatial_items(vec![page]);
        let observed = outcome
            .items
            .into_iter()
            .map(|item| (item.description, item.price_scaled))
            .collect::<Vec<_>>();

        let has = |needle: &str, price: i64| {
            observed
                .iter()
                .any(|(desc, p)| desc.contains(needle) && *p == price)
        };
        assert!(has("BANANA", 27_000), "{observed:?}");
        assert!(has("WMELON RED SOLS", 59_900), "{observed:?}");
        for price in [42_800, 45_000, 48_100, 44_300] {
            assert!(
                has("CHERRIES RED", price),
                "missing CHERRIES RED {price}: {observed:?}"
            );
        }
        assert!(has("PEACH YELLOW", 19_600), "{observed:?}");
        assert!(
            !observed
                .iter()
                .any(|(desc, _)| desc.contains("Tare") || desc.contains("Gro")),
            "{observed:?}"
        );
    }

    #[test]
    fn keeps_costco_short_code_item_with_percent_suffix() {
        // Costco 2026-05-24_costco_56_42: "458 MILK 2% 6.09". The 3-digit item
        // code "458" plus the "2%" fat suffix dragged the alpha-ratio below 0.5
        // so both MILK lines were dropped, while "1346909 KS ORG 2% 4L" (6-digit
        // code, stripped) survived. Stripping the short code restores them.
        let page = PageInput {
            lines: vec![
                LineInput {
                    text: "458 MILK 2% 6.09".to_string(),
                    words: vec![
                        word("458 MILK 2%", 0.30, 0.200, 0.54, 0.212),
                        word("6.09", 0.82, 0.200, 0.90, 0.212),
                    ],
                },
                LineInput {
                    text: "458 MILK 2% 6.09".to_string(),
                    words: vec![
                        word("458 MILK 2%", 0.30, 0.220, 0.54, 0.232),
                        word("6.09", 0.82, 0.220, 0.90, 0.232),
                    ],
                },
                LineInput {
                    text: "1346909 KS ORG 2% 4L 10.29".to_string(),
                    words: vec![
                        word("1346909 KS ORG 2% 4L", 0.30, 0.240, 0.58, 0.252),
                        word("10.29", 0.82, 0.240, 0.90, 0.252),
                    ],
                },
                LineInput {
                    text: "TOTAL 26.47".to_string(),
                    words: vec![
                        word("TOTAL", 0.09, 0.500, 0.18, 0.512),
                        word("26.47", 0.82, 0.500, 0.90, 0.512),
                    ],
                },
            ],
        };

        let outcome = extract_spatial_items(vec![page]);
        let milk_count = outcome
            .items
            .iter()
            .filter(|item| item.price_scaled == 60_900)
            .count();
        assert_eq!(
            milk_count, 2,
            "both MILK 2% lines expected, got {:?}",
            outcome.items
        );
    }

    #[test]
    fn prefers_item_above_onsale_price() {
        let page = PageInput {
            lines: vec![
                LineInput {
                    text: "*S & B Wasabi".to_string(),
                    words: vec![word("*S & B Wasabi", 0.08, 0.100, 0.260, 0.112)],
                },
                LineInput {
                    text: "(E)ON SALE 1.98".to_string(),
                    words: vec![
                        word("(E)ON SALE", 0.09, 0.120, 0.210, 0.132),
                        word("1.98", 0.88, 0.120, 0.93, 0.132),
                    ],
                },
                LineInput {
                    text: "2 @ $0.99 4.59".to_string(),
                    words: vec![
                        word("2 @ $0.99", 0.22, 0.140, 0.320, 0.152),
                        word("4.59", 0.88, 0.140, 0.93, 0.152),
                    ],
                },
                LineInput {
                    text: "Hot Kid Honey Flavour Bal".to_string(),
                    words: vec![word("Hot Kid Honey Flavour Bal", 0.08, 0.160, 0.360, 0.172)],
                },
                LineInput {
                    text: "TOTAL 6.57".to_string(),
                    words: vec![
                        word("TOTAL", 0.09, 0.500, 0.180, 0.512),
                        word("6.57", 0.88, 0.500, 0.93, 0.512),
                    ],
                },
            ],
        };

        let outcome = extract_spatial_items(vec![page]);
        let observed = outcome
            .items
            .into_iter()
            .map(|item| (item.description, item.price_scaled))
            .collect::<Vec<_>>();

        assert_eq!(
            observed,
            vec![
                ("S & B Wasabi".to_string(), 19_800),
                ("Hot Kid Honey Flavour Bal".to_string(), 45_900),
            ]
        );
    }

    #[test]
    fn quantity_price_row_with_ea_suffix_uses_item_above() {
        let page = PageInput {
            lines: vec![
                LineInput {
                    text: "FF SHEPHERDS PURSE FILLING".to_string(),
                    words: vec![word("FF SHEPHERDS PURSE FILLING", 0.05, 0.700, 0.40, 0.712)],
                },
                LineInput {
                    text: "2 @ $3.49ea. W $6.98".to_string(),
                    words: vec![
                        word("2 @ $3.49ea.", 0.07, 0.723, 0.23, 0.735),
                        word("W $6.98", 0.88, 0.723, 0.95, 0.735),
                    ],
                },
                LineInput {
                    text: "TOTAL 6.98".to_string(),
                    words: vec![
                        word("TOTAL", 0.10, 0.900, 0.18, 0.912),
                        word("6.98", 0.88, 0.900, 0.93, 0.912),
                    ],
                },
            ],
        };

        let outcome = extract_spatial_items(vec![page]);
        let observed = outcome
            .items
            .into_iter()
            .map(|item| (item.description, item.price_scaled))
            .collect::<Vec<_>>();

        assert_eq!(
            observed,
            vec![("FF SHEPHERDS PURSE FILLING".to_string(), 69_800)]
        );
    }

    #[test]
    fn skips_receipt_metadata_when_quantity_row_needs_item_context() {
        let page = PageInput {
            lines: vec![
                LineInput {
                    text: "WS# P6 Cashier6".to_string(),
                    words: vec![word("WS# P6 Cashier6", 0.05, 0.100, 0.22, 0.112)],
                },
                LineInput {
                    text: "*S & B Wasabi".to_string(),
                    words: vec![word("*S & B Wasabi", 0.08, 0.140, 0.260, 0.152)],
                },
                LineInput {
                    text: "(E)ON SALE 1.98".to_string(),
                    words: vec![
                        word("(E)ON SALE", 0.09, 0.160, 0.210, 0.172),
                        word("1.98", 0.88, 0.160, 0.93, 0.172),
                    ],
                },
                LineInput {
                    text: "2 @ $0.99 4.59".to_string(),
                    words: vec![
                        word("2 @ $0.99", 0.22, 0.180, 0.320, 0.192),
                        word("4.59", 0.88, 0.180, 0.93, 0.192),
                    ],
                },
                LineInput {
                    text: "Hot Kid Honey Flavour Bal".to_string(),
                    words: vec![word("Hot Kid Honey Flavour Bal", 0.08, 0.200, 0.360, 0.212)],
                },
                LineInput {
                    text: "TOTAL 6.57".to_string(),
                    words: vec![
                        word("TOTAL", 0.09, 0.500, 0.180, 0.512),
                        word("6.57", 0.88, 0.500, 0.93, 0.512),
                    ],
                },
            ],
        };

        let outcome = extract_spatial_items(vec![page]);
        let observed = outcome
            .items
            .into_iter()
            .map(|item| (item.description, item.price_scaled))
            .collect::<Vec<_>>();

        assert_eq!(
            observed,
            vec![
                ("S & B Wasabi".to_string(), 19_800),
                ("Hot Kid Honey Flavour Bal".to_string(), 45_900),
            ]
        );
    }

    // --- ported regressions from desktop tests/test_receipt_spatial_parser_regressions.py ---
    // Prices are ×10000 fixed-point (3.17 -> 31_700); the desktop asserts on Decimal.
    // The Rust spatial extractor takes no rule layers (categorization is a later stage).

    fn line(text: &str, words: Vec<WordInput>) -> LineInput {
        LineInput {
            text: text.to_string(),
            words,
        }
    }

    fn pairs_of(lines: Vec<LineInput>) -> Vec<(String, i64)> {
        extract_spatial_items(vec![PageInput { lines }])
            .items
            .into_iter()
            .map(|item| (item.description, item.price_scaled))
            .collect()
    }

    /// A code-only price row (barcode + deposit price) between two items must not
    /// have its price stolen by the following item.
    #[test]
    fn keeps_next_priced_item_from_stealing_code_only_price_row() {
        let lines = vec![
            line(
                "26-L.IQUOR COORS LIGHT 6 PK HQ 15.79",
                vec![
                    word("26-L.IQUOR", 0.031, 0.289, 0.191, 0.309),
                    word("COORS LIGHT 6 PK HQ", 0.318, 0.301, 0.688, 0.328),
                    word("15.79", 0.760, 0.298, 0.860, 0.324),
                ],
            ),
            line(
                "05632700795 0.60",
                vec![
                    word("05632700795", 0.068, 0.309, 0.257, 0.328),
                    word("0.60", 0.881, 0.317, 0.963, 0.340),
                ],
            ),
            line(
                "DEPOSIT 1 COORS PINEAPPLE HQ 3.19",
                vec![
                    word("DEPOSIT 1", 0.102, 0.328, 0.258, 0.348),
                    word("COORS PINEAPPLE", 0.322, 0.342, 0.579, 0.365),
                    word("HQ", 0.657, 0.341, 0.701, 0.362),
                    word("3.19", 0.793, 0.338, 0.876, 0.361),
                ],
            ),
            line(
                "05632702339 0.10",
                vec![
                    word("05632702339", 0.070, 0.347, 0.259, 0.366),
                    word("0.10", 0.882, 0.356, 0.963, 0.377),
                ],
            ),
            line(
                "DEPOSIT 1",
                vec![word("DEPOSIT 1", 0.103, 0.366, 0.260, 0.385)],
            ),
            line(
                "TOTAL 19.68",
                vec![
                    word("TOTAL", 0.090, 0.500, 0.180, 0.512),
                    word("19.68", 0.880, 0.500, 0.950, 0.512),
                ],
            ),
        ];
        let pairs = pairs_of(lines);
        assert!(pairs.contains(&("26-L.IQUOR COORS LIGHT 6 PK HQ".to_string(), 157_900)));
        assert!(pairs
            .iter()
            .any(|(d, p)| d.contains("COORS PINEAPPLE") && *p == 31_900));
        assert!(!pairs
            .iter()
            .any(|(d, p)| d.contains("COORS PINEAPPLE") && *p == 6_000));
    }

    /// A quantity total ("3@$0.10 2.79") must attach to the multi-buy item, not
    /// to the intervening "DEPOSIT 1" stub.
    #[test]
    fn keeps_quantity_total_off_deposit_stub() {
        let lines = vec![
            line(
                "(3)06365703339 GROWERS CIDER HQ 10.47",
                vec![
                    word("(3)06365703339", 0.074, 0.385, 0.310, 0.403),
                    word("GROWERS CIDER", 0.373, 0.381, 0.597, 0.403),
                    word("HQ", 0.657, 0.379, 0.702, 0.400),
                    word("10.47", 0.869, 0.393, 0.963, 0.415),
                ],
            ),
            line(
                "3 @ $3.49",
                vec![word("3 @ $3.49", 0.102, 0.403, 0.261, 0.422)],
            ),
            line(
                "DEPOSIT 1 0.30",
                vec![
                    word("DEPOSIT 1", 0.100, 0.421, 0.258, 0.439),
                    word("0.30", 0.884, 0.430, 0.961, 0.452),
                ],
            ),
            line(
                "3@$0.10 2.79",
                vec![
                    word("3@$0.10", 0.098, 0.438, 0.225, 0.457),
                    word("2.79", 0.812, 0.451, 0.893, 0.473),
                ],
            ),
            line(
                "06365703620 GROW CIDER HQ 0.10",
                vec![
                    word("06365703620", 0.061, 0.457, 0.259, 0.475),
                    word("GROW CIDER", 0.322, 0.456, 0.498, 0.476),
                    word("HQ", 0.676, 0.454, 0.720, 0.475),
                    word("0.10", 0.882, 0.469, 0.964, 0.492),
                ],
            ),
            line(
                "DEPOSIT 1",
                vec![word("DEPOSIT 1", 0.094, 0.475, 0.256, 0.495)],
            ),
            line(
                "TOTAL 13.66",
                vec![
                    word("TOTAL", 0.090, 0.500, 0.180, 0.512),
                    word("13.66", 0.880, 0.500, 0.950, 0.512),
                ],
            ),
        ];
        let pairs = pairs_of(lines);
        assert!(pairs
            .iter()
            .any(|(d, p)| d.contains("GROW CIDER") && *p == 27_900));
        assert!(!pairs.iter().any(|(d, p)| d == "DEPOSIT 1" && *p == 27_900));
    }

    /// A duplicate code row that repeats the previous item's price must lend that
    /// price to the following unpriced item.
    #[test]
    fn assigns_duplicate_code_row_price_to_next_unpriced_item() {
        let lines = vec![
            line(
                "27-PRODUCE CANTALOUPE MRJ 1.99",
                vec![
                    word("27-PRODUCE", 0.017, 0.493, 0.205, 0.514),
                    word("CANTALOUPE", 0.318, 0.510, 0.496, 0.529),
                    word("MRJ", 0.676, 0.510, 0.738, 0.529),
                    word("1.99", 0.817, 0.507, 0.896, 0.528),
                ],
            ),
            line(
                "4050 1.99",
                vec![
                    word("4050", 0.055, 0.513, 0.132, 0.532),
                    word("1.99", 0.784, 0.525, 0.862, 0.547),
                ],
            ),
            line(
                "81363501124 BLACKBERRIES 60Z MRJ",
                vec![
                    word("81363501124", 0.054, 0.531, 0.254, 0.551),
                    word("BLACKBERRIES 60Z", 0.319, 0.528, 0.602, 0.549),
                    word("MRJ", 0.641, 0.528, 0.704, 0.547),
                ],
            ),
            line(
                "TOTAL 3.98",
                vec![
                    word("TOTAL", 0.090, 0.600, 0.180, 0.612),
                    word("3.98", 0.880, 0.600, 0.950, 0.612),
                ],
            ),
        ];
        let pairs = pairs_of(lines);
        assert!(pairs.contains(&("CANTALOUPE".to_string(), 19_900)));
        assert!(pairs.contains(&("BLACKBERRIES 60Z".to_string(), 19_900)));
    }

    /// Two rows whose price is embedded in an OCR-garbled trailing word
    /// ("gnigoQq bn14.99") must both surface as priced items.
    #[test]
    fn accepts_embedded_trailing_price_word() {
        let lines = vec![
            line(
                "2146010 SEAFOOD CNTR gnigoQq bn14.99",
                vec![
                    word("2146010", 0.056, 0.568, 0.190, 0.589),
                    word("SEAFOOD CNTR", 0.320, 0.565, 0.539, 0.585),
                    word("gnigoQq bn14.99", 0.567, 0.564, 0.899, 0.586),
                ],
            ),
            line(
                "2146010b SEAFOOD CNTR noitqQ 14.99",
                vec![
                    word("2146010b", 0.060, 0.586, 0.233, 0.606),
                    word("SEAFOOD CNTR", 0.320, 0.584, 0.534, 0.606),
                    word("noitqQ", 0.581, 0.586, 0.706, 0.611),
                    word("14.99", 0.803, 0.584, 0.898, 0.606),
                ],
            ),
            line(
                "TOTAL 29.98",
                vec![
                    word("TOTAL", 0.090, 0.650, 0.180, 0.662),
                    word("29.98", 0.880, 0.650, 0.950, 0.662),
                ],
            ),
        ];
        let pairs = pairs_of(lines);
        let seafood = pairs
            .iter()
            .filter(|(d, p)| d.contains("SEAFOOD CNTR") && *p == 149_900)
            .count();
        assert_eq!(seafood, 2);
    }
}
