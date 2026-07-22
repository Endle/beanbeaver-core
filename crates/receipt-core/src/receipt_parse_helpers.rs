use regex::Regex;
use std::sync::OnceLock;

use crate::merchant_match::{self, MerchantFamily, MerchantMatch};

#[derive(Clone, Debug)]
pub struct MerchantWordInput {
    pub confidence: f64,
    pub has_bbox: bool,
}

#[derive(Clone, Debug)]
pub struct MerchantLineInput {
    pub text: String,
    pub words: Vec<MerchantWordInput>,
    /// Tallest detection-box height (px, de-padded image space) among this line's
    /// words. A large-font store banner sits well above the body text height, so
    /// this drives the size-prior in [`extract_merchant_with_confidence`].
    pub height: f64,
    /// Line center Y (px). Used to restrict the banner search to the receipt top.
    pub center_y: f64,
}

#[derive(Clone, Debug)]
pub struct MerchantPageInput {
    pub lines: Vec<MerchantLineInput>,
}

fn re_numeric_date_like() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[\d/\-:]+$").unwrap())
}

fn re_clean_merchant() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[^\w\s&'-]").unwrap())
}

fn re_spatial_w_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"W\s+\$\d+\.\d{2}").unwrap())
}

/// Tall header lines that are decidedly NOT the store banner: dates,
/// `Receipt#`, loyalty `Member`/sign-up slogans, Canadian postal codes,
/// `, ON`/`Ontario` address lines, and priced item rows (`@`/`$`). Used to
/// veto a coincidentally-large line before the size-prior trusts it.
fn re_banner_reject() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\d{4}[/\-]\d{2}|RECEIPT|MEMBER|[A-Z]\d[A-Z]\s?\d[A-Z]\d|,\s*ON\b|ONTARIO|@|\$\d|DOWNLOAD|JOIN NOW|\bWIN\b",
        )
        .unwrap()
    })
}

const MIN_LINE_CONFIDENCE: f64 = 0.6;

/// A header line must be at least this many times the receipt's median line
/// height to be trusted as the large-font store banner. Calibrated on the
/// private corpus: ~66% of receipts have a banner this dominant; the rest
/// (uniform-font thermal receipts) fall back to the first-plausible-line path.
const MIN_BANNER_HEIGHT_RATIO: f64 = 1.8;

/// Only the top fraction of the receipt (by line center Y) is eligible to hold
/// the banner, so a large-font total/footer can't masquerade as the merchant.
const BANNER_TOP_FRACTION: f64 = 0.25;

fn clean_merchant_candidate(value: &str) -> String {
    re_clean_merchant()
        .replace_all(value, "")
        .trim()
        .to_string()
}

/// Loose plausibility used by both the size-prior and the first-line fallback:
/// a confident, non-empty, non-date line that cleans to a usable candidate.
/// Returns the cleaned candidate, or `None` if the line can't be a merchant.
fn line_merchant_candidate(line: &MerchantLineInput) -> Option<String> {
    if line.words.is_empty() {
        return None;
    }
    let avg_confidence =
        line.words.iter().map(|word| word.confidence).sum::<f64>() / line.words.len() as f64;
    if avg_confidence < MIN_LINE_CONFIDENCE {
        return None;
    }
    let line_text = line.text.trim();
    if line_text.len() <= 3 || re_numeric_date_like().is_match(line_text) {
        return None;
    }
    let cleaned = clean_merchant_candidate(line_text);
    (cleaned.len() > 2).then_some(cleaned)
}

fn median(mut values: Vec<f64>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// Size-prior: when a large-font store banner clearly dominates the receipt
/// top, prefer it over the first plausible line (which real receipts often make
/// a deskew-mangled artifact or a loyalty slogan). Returns `None` when no line
/// stands out by size — the uniform-font case — so the caller falls back to the
/// historical first-line behavior. Only the tallest banner-eligible line in the
/// top region wins, and [`re_banner_reject`] vetoes tall date/address/price rows.
fn banner_by_size(lines: &[&MerchantLineInput]) -> Option<String> {
    let heights: Vec<f64> = lines
        .iter()
        .map(|l| l.height)
        .filter(|h| *h > 0.0)
        .collect();
    if heights.len() < 3 {
        return None; // too few measured lines to judge relative size
    }
    let median_height = median(heights);
    if median_height <= 0.0 {
        return None;
    }

    let min_y = lines
        .iter()
        .map(|l| l.center_y)
        .fold(f64::INFINITY, f64::min);
    let max_y = lines
        .iter()
        .map(|l| l.center_y)
        .fold(f64::NEG_INFINITY, f64::max);
    let cut = min_y + BANNER_TOP_FRACTION * (max_y - min_y);

    let mut best: Option<(f64, String)> = None;
    for line in lines {
        if line.center_y > cut {
            continue;
        }
        if line.height < MIN_BANNER_HEIGHT_RATIO * median_height {
            continue;
        }
        if re_banner_reject().is_match(&line.text) {
            continue;
        }
        let Some(cleaned) = line_merchant_candidate(line) else {
            continue;
        };
        if best.as_ref().map_or(true, |(h, _)| line.height > *h) {
            best = Some((line.height, cleaned));
        }
    }
    best.map(|(_, cleaned)| cleaned)
}

pub fn extract_merchant_with_confidence(pages: &[MerchantPageInput]) -> Option<String> {
    if pages.is_empty() {
        return None;
    }

    let lines: Vec<&MerchantLineInput> = pages.iter().flat_map(|page| page.lines.iter()).collect();

    // Prefer a dominant large-font banner when one exists...
    if let Some(banner) = banner_by_size(&lines) {
        return Some(banner);
    }

    // ...otherwise fall back to the first plausible header line (historical
    // behavior), bounded to the first 10 lines of the header region.
    lines
        .iter()
        .take(10)
        .find_map(|line| line_merchant_candidate(line))
}

/// Best-effort OCR'd merchant name straight from the receipt header, *before*
/// any canonicalization — prefer the highest-confidence early line, then fall
/// back to the first plausible non-date line. This is the `raw` the matcher
/// preserves; `"UNKNOWN_MERCHANT"` is the last-resort sentinel (kept for parity
/// with the prior behavior).
fn ocr_header_candidate(lines: &[String], pages: &[MerchantPageInput]) -> String {
    if let Some(confident) = extract_merchant_with_confidence(pages) {
        return confident;
    }

    for line in lines.iter().take(5) {
        if line.len() > 3 && !re_numeric_date_like().is_match(line) {
            let cleaned = clean_merchant_candidate(line);
            if cleaned.len() > 2 {
                return cleaned;
            }
        }
    }

    "UNKNOWN_MERCHANT".to_string()
}

/// Resolve the receipt's merchant to a `MerchantMatch`: the raw OCR header plus,
/// when we can determine it safely, the canonical family and how much to trust
/// it. Generalizes the former hardcoded Costco/FreshCo/Foody Mart branches via
/// the data-driven [`merchant_match`] matcher.
pub fn extract_merchant_match(
    lines: &[String],
    full_text: &str,
    pages: &[MerchantPageInput],
    known_merchants: &[String],
    families: &[MerchantFamily],
) -> MerchantMatch {
    let raw = ocr_header_candidate(lines, pages);
    let full_text_upper = full_text.to_ascii_uppercase();
    merchant_match::resolve(&raw, &full_text_upper, known_merchants, families)
}

pub fn has_useful_bbox_data(pages: &[MerchantPageInput]) -> bool {
    if pages.is_empty() {
        return false;
    }
    for line in pages[0].lines.iter().take(10) {
        for word in &line.words {
            if word.has_bbox {
                return true;
            }
        }
    }
    false
}

pub fn is_spatial_layout_receipt(full_text: &str) -> bool {
    let full_text_upper = full_text.to_ascii_uppercase();
    for merchant in [
        "T&T",
        "T & T",
        "REAL CANADIAN",
        "SUPERSTORE",
        "C&C",
        "C & C",
        "NOFRILLS",
        "NO FRILLS",
        "COSTCO",
        "WHOLESALE",
    ] {
        if full_text_upper.contains(merchant) {
            return true;
        }
    }
    re_spatial_w_price().is_match(full_text)
}

#[cfg(test)]
mod tests {
    use super::{
        extract_merchant_match, extract_merchant_with_confidence, MerchantLineInput,
        MerchantPageInput, MerchantWordInput,
    };
    use crate::merchant_match::MerchantFamily;

    /// One header line with a single word at the given confidence, box `height`,
    /// and line `center_y` — the geometry the size-prior reads.
    fn mline(text: &str, confidence: f64, height: f64, center_y: f64) -> MerchantLineInput {
        MerchantLineInput {
            text: text.to_string(),
            words: vec![MerchantWordInput {
                confidence,
                has_bbox: true,
            }],
            height,
            center_y,
        }
    }

    fn one_page(lines: Vec<MerchantLineInput>) -> Vec<MerchantPageInput> {
        vec![MerchantPageInput { lines }]
    }

    fn families() -> Vec<MerchantFamily> {
        vec![
            MerchantFamily {
                canonical: "COSTCO".to_string(),
                aliases: vec!["COSTCO WHOLESALE".to_string(), "COSICO".to_string()],
                corroborators: vec!["WHOLESALE".to_string()],
            },
            MerchantFamily {
                canonical: "FRESHCO".to_string(),
                aliases: vec!["FRESHCO".to_string()],
                corroborators: vec![],
            },
            MerchantFamily {
                canonical: "FOODY MART".to_string(),
                aliases: vec!["FOODY MART".to_string()],
                corroborators: vec![],
            },
        ]
    }

    fn display(full_text: &str) -> String {
        let lines: Vec<String> = full_text.lines().map(str::to_string).collect();
        extract_merchant_match(&lines, full_text, &[], &["COSTCO".to_string()], &families())
            .display()
            .to_string()
    }

    #[test]
    fn canonicalizes_costco_ocr_dropped_leading_c() {
        // OCR dropped the leading C; "WHOLESALE" banner confirms Costco.
        assert_eq!(
            display("OSTCO\nWHOLESALE\nBranch #001\n1268728 UNREAL 17.99"),
            "COSTCO"
        );
    }

    #[test]
    fn canonicalizes_freshco_from_address_when_banner_misocrd() {
        // Banner OCR'd as "FRESHCC"; correct spelling appears in the address.
        assert_eq!(
            display("FRESHCC\n123 Example St FreshCo\nCilantro $0.99"),
            "FRESHCO"
        );
    }

    #[test]
    fn canonicalizes_foody_mart_from_noisy_banner() {
        // Banner runs name into branch/address on one OCR line.
        assert_eq!(
            display("FOODY MART(Branch) 123 Example Rd\nAsahi 1.99"),
            "FOODY MART"
        );
    }

    #[test]
    fn does_not_rewrite_unrelated_merchants() {
        // Neither a Costco banner nor a FreshCo token: keep the OCR'd line.
        assert_eq!(display("SHOPRITE\n123 Main Street\nMilk $2.99"), "SHOPRITE");
    }

    #[test]
    fn banner_size_prior_beats_a_mangled_first_line() {
        // The first plausible line is deskew garbage; the real store name is a
        // tall banner just below it. Size-prior must pick the banner.
        let pages = one_page(vec![
            mline("roegnoroxeholbem", 0.9, 30.0, 30.0), // garbage, first-plausible
            mline("Loblaws", 0.95, 75.0, 65.0),         // tall banner (top region)
            mline("Milk", 0.9, 28.0, 150.0),
            mline("Bread", 0.9, 28.0, 190.0),
            mline("Eggs", 0.9, 28.0, 230.0),
        ]);
        assert_eq!(
            extract_merchant_with_confidence(&pages).as_deref(),
            Some("Loblaws")
        );
    }

    #[test]
    fn uniform_font_falls_back_to_first_line() {
        // pharmasave-style: the whole receipt is one font size, so no line
        // dominates and we keep the historical first-plausible-line behavior.
        let pages = one_page(vec![
            mline("GRAND GENESIS", 0.99, 92.0, 40.0),
            mline("PHARMASAVE", 0.99, 98.0, 90.0),
            mline("TOOTHPASTE", 0.98, 85.0, 150.0),
            mline("SUBTOTAL", 0.98, 85.0, 190.0),
            mline("TOTAL", 0.98, 85.0, 230.0),
        ]);
        assert_eq!(
            extract_merchant_with_confidence(&pages).as_deref(),
            Some("GRAND GENESIS")
        );
    }

    #[test]
    fn tall_date_receipt_line_is_vetoed_banner_picks_next() {
        // A tall date/`Receipt#` header outsizes the banner but must be vetoed,
        // so the next-tallest eligible line (the store name) wins.
        let pages = one_page(vec![
            mline("2026/02/04 10:26 Receipt# P42502", 0.95, 90.0, 40.0), // tallest, vetoed
            mline("Bestco Fresh Foodmart", 0.95, 70.0, 80.0),            // banner
            mline("Produce", 0.9, 30.0, 150.0),
            mline("Dairy", 0.9, 30.0, 190.0),
            mline("Meat", 0.9, 30.0, 230.0),
        ]);
        assert_eq!(
            extract_merchant_with_confidence(&pages).as_deref(),
            Some("Bestco Fresh Foodmart")
        );
    }

    #[test]
    fn tall_footer_outside_top_region_is_ignored() {
        // A large-font TOTAL at the bottom must not be mistaken for the banner;
        // only the top region is eligible, so we fall back to the first line.
        let pages = one_page(vec![
            mline("Corner Store", 0.9, 30.0, 40.0),
            mline("Chips", 0.9, 30.0, 100.0),
            mline("Soda", 0.9, 30.0, 140.0),
            mline("Water", 0.9, 30.0, 180.0),
            mline("TOTAL", 1.0, 90.0, 250.0), // tall, but bottom of receipt
        ]);
        assert_eq!(
            extract_merchant_with_confidence(&pages).as_deref(),
            Some("Corner Store")
        );
    }
}
