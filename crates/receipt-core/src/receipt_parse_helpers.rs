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

const MIN_LINE_CONFIDENCE: f64 = 0.6;

fn clean_merchant_candidate(value: &str) -> String {
    re_clean_merchant()
        .replace_all(value, "")
        .trim()
        .to_string()
}

pub fn extract_merchant_with_confidence(pages: &[MerchantPageInput]) -> Option<String> {
    if pages.is_empty() {
        return None;
    }

    let mut lines_checked = 0usize;
    for page in pages {
        for line in &page.lines {
            if lines_checked >= 10 {
                return None;
            }
            if line.words.is_empty() {
                continue;
            }
            let avg_confidence = line.words.iter().map(|word| word.confidence).sum::<f64>()
                / line.words.len() as f64;
            if avg_confidence < MIN_LINE_CONFIDENCE {
                lines_checked += 1;
                continue;
            }

            let line_text = line.text.trim();
            if line_text.len() <= 3 || re_numeric_date_like().is_match(line_text) {
                lines_checked += 1;
                continue;
            }

            let cleaned = clean_merchant_candidate(line_text);
            if cleaned.len() > 2 {
                return Some(cleaned);
            }

            lines_checked += 1;
        }
    }

    None
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
    use super::extract_merchant_match;
    use crate::merchant_match::MerchantFamily;

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
        assert_eq!(display("OSTCO\nWHOLESALE\nBranch #001\n1268728 UNREAL 17.99"), "COSTCO");
    }

    #[test]
    fn canonicalizes_freshco_from_address_when_banner_misocrd() {
        // Banner OCR'd as "FRESHCC"; correct spelling appears in the address.
        assert_eq!(display("FRESHCC\n123 Example St FreshCo\nCilantro $0.99"), "FRESHCO");
    }

    #[test]
    fn canonicalizes_foody_mart_from_noisy_banner() {
        // Banner runs name into branch/address on one OCR line.
        assert_eq!(display("FOODY MART(Branch) 123 Example Rd\nAsahi 1.99"), "FOODY MART");
    }

    #[test]
    fn does_not_rewrite_unrelated_merchants() {
        // Neither a Costco banner nor a FreshCo token: keep the OCR'd line.
        assert_eq!(display("SHOPRITE\n123 Main Street\nMilk $2.99"), "SHOPRITE");
    }
}
