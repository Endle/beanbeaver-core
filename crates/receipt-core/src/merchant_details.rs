//! Merchant contact and branch details printed on a receipt.
//!
//! This module extracts evidence from OCR text only. It deliberately does not
//! geocode, validate against a directory, or turn a printed address into a
//! geographic identity; those are network-backed enrichment jobs for a future
//! consumer.

use regex::Regex;
use std::sync::OnceLock;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MerchantDetails {
    pub street_address: Option<String>,
    pub city: Option<String>,
    pub region: Option<String>,
    /// Postal code as a separate value, normalized for lookup when recognized.
    pub postal_code: Option<String>,
    /// Phone number as printed, apart from surrounding whitespace.
    pub phone_number: Option<String>,
    /// Branch/store identifier. A string because leading zeroes and letters are meaningful.
    pub store_number: Option<String>,
    /// OCR lines that contributed at least one extracted value, in receipt order.
    pub raw_lines: Vec<String>,
}

fn canadian_postal_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b([ABCEGHJ-NPRSTVXY]\d[ABCEGHJ-NPRSTV-Z])[ -]?(\d[ABCEGHJ-NPRSTV-Z]\d)\b")
            .expect("valid Canadian postal-code regex")
    })
}

fn us_zip_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b\d{5}(?:-\d{4})?\b").expect("valid US ZIP regex"))
}

fn us_region_before_zip_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(?:^|[, ])\s*[A-Z]{2}\s*$").expect("valid US region regex"))
}

fn phone_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?:^|[^0-9])((?:\+?1[ .-]?)?(?:\([0-9]{3}\)|[0-9]{3})[ .-]?[0-9]{3}[ .-]?[0-9]{4})\b",
        )
        .expect("valid phone regex")
    })
}

fn store_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\bSTORE\s*(?:#|NO\.?|NUMBER|:)?\s*([A-Z0-9-]{3,})\b")
            .expect("valid store-number regex")
    })
}

fn street_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b\d+[A-Z-]*\s+.+\b(?:ST(?:REET)?|RD|ROAD|AVE(?:NUE)?|BLVD|BOULEVARD|DR(?:IVE)?|HWY|HIGHWAY|LANE|LN|COURT|CT|PKWY|PARKWAY)\.?\b",
        )
        .expect("valid street-address regex")
    })
}

fn trailing_price_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:^|\s)\$?\d+[.,]\d{2}-?\s*$").expect("valid trailing-price regex")
    })
}

fn street_like(line: &str) -> bool {
    street_re().is_match(line) && !trailing_price_re().is_match(line)
}

fn clean_component(value: &str) -> Option<String> {
    let cleaned = value
        .trim_matches(|c: char| c.is_ascii_whitespace() || c == ',' || c == '-')
        .trim();
    (!cleaned.is_empty()).then(|| cleaned.to_string())
}

fn add_raw(raw_lines: &mut Vec<String>, line: &str) {
    if !raw_lines.iter().any(|existing| existing == line) {
        raw_lines.push(line.to_string());
    }
}

fn postal_in(line: &str) -> Option<(String, std::ops::Range<usize>)> {
    if let Some(captures) = canadian_postal_re().captures(line) {
        let whole = captures.get(0)?;
        let postal = format!(
            "{} {}",
            captures.get(1)?.as_str().to_ascii_uppercase(),
            captures.get(2)?.as_str().to_ascii_uppercase()
        );
        return Some((postal, whole.range()));
    }
    let found = us_zip_re().find(line)?;
    let prefix = line[..found.start()].trim_end();
    let region_before_zip = us_region_before_zip_re().is_match(prefix);
    (street_like(prefix) || region_before_zip).then(|| (found.as_str().to_string(), found.range()))
}

fn address_parts_before_postal(prefix: &str, out: &mut MerchantDetails) {
    let parts: Vec<&str> = prefix
        .trim_end_matches(|c: char| c.is_ascii_whitespace() || c == ',')
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return;
    }

    if street_like(parts[0]) {
        out.street_address = clean_component(parts[0]);
        if parts.len() >= 2 {
            out.city = clean_component(parts[1]);
        }
        if parts.len() >= 3 {
            out.region = clean_component(parts[2]);
        }
    } else {
        out.city = clean_component(parts[0]);
        if parts.len() >= 2 {
            out.region = clean_component(parts[1]);
        }
    }
}

/// Extract the merchant details that the receipt itself prints.
///
/// Values fail independently: a receipt may yield only a postal code, store
/// number, or phone number. `raw_lines` retains the evidence for later parser
/// improvements and auditing.
pub fn extract_merchant_details(lines: &[String]) -> MerchantDetails {
    let mut out = MerchantDetails::default();

    for (index, line) in lines.iter().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if out.phone_number.is_none() {
            if let Some(found) = phone_re()
                .captures(line)
                .and_then(|captures| captures.get(1))
            {
                out.phone_number = Some(found.as_str().trim().to_string());
                add_raw(&mut out.raw_lines, line);
            }
        }

        if out.store_number.is_none() {
            if let Some(captures) = store_re().captures(line) {
                if let Some(value) = captures.get(1).map(|m| m.as_str()) {
                    // Reject ordinary phrases such as "store manager" while
                    // retaining OCR-damaged identifiers like Walmart's 30E3.
                    if value.chars().any(|ch| ch.is_ascii_digit()) {
                        out.store_number = Some(value.to_string());
                        add_raw(&mut out.raw_lines, line);
                    }
                }
            }
        }

        if out.postal_code.is_none() {
            if let Some((postal, range)) = postal_in(line) {
                out.postal_code = Some(postal);
                address_parts_before_postal(&line[..range.start], &mut out);
                add_raw(&mut out.raw_lines, line);

                // Multi-line headers commonly put the street immediately
                // above `City, Region, Postal`. Search only nearby header rows
                // so an item SKU cannot become an address.
                if out.street_address.is_none() {
                    for previous in lines[index.saturating_sub(3)..index].iter().rev() {
                        if street_like(previous) {
                            out.street_address = clean_component(previous);
                            add_raw(&mut out.raw_lines, previous.trim());
                            break;
                        }
                    }
                }
            }
        }

        if out.street_address.is_none() && street_like(line) {
            out.street_address = clean_component(line.split(',').next().unwrap_or(line));
            add_raw(&mut out.raw_lines, line);
        }
    }

    // Keep evidence in receipt order even when a preceding street line was
    // discovered while processing the following postal-code line.
    out.raw_lines.sort_by_key(|raw| {
        lines
            .iter()
            .position(|line| line.trim() == raw)
            .unwrap_or(usize::MAX)
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    #[test]
    fn extracts_combined_canadian_address_and_branch_details() {
        let found = extract_merchant_details(&lines(
            "T&T SUPERMARKET\n7070 Warden Ave., Markham, ON L3R 5Y2\nStore: 10011\nTel (905) 513-8818",
        ));
        assert_eq!(found.street_address.as_deref(), Some("7070 Warden Ave."));
        assert_eq!(found.city.as_deref(), Some("Markham"));
        assert_eq!(found.region.as_deref(), Some("ON"));
        assert_eq!(found.postal_code.as_deref(), Some("L3R 5Y2"));
        assert_eq!(found.store_number.as_deref(), Some("10011"));
        assert_eq!(found.phone_number.as_deref(), Some("(905) 513-8818"));
        assert_eq!(found.raw_lines.len(), 3);
    }

    #[test]
    fn joins_multiline_address_and_normalizes_hyphenated_postal_code() {
        let found = extract_merchant_details(&lines(
            "Store #390\n192 BULLOCK DRIVE\nMARKHAM, ON L3P-1W2\nTOTAL 10.00",
        ));
        assert_eq!(found.street_address.as_deref(), Some("192 BULLOCK DRIVE"));
        assert_eq!(found.city.as_deref(), Some("MARKHAM"));
        assert_eq!(found.region.as_deref(), Some("ON"));
        assert_eq!(found.postal_code.as_deref(), Some("L3P 1W2"));
        assert_eq!(found.store_number.as_deref(), Some("390"));
    }

    #[test]
    fn store_manager_and_item_phone_words_are_not_details() {
        let found = extract_merchant_details(&lines(
            "STORE MANAGER: PAT\nMeat, Phone Cards, Smokes and\n12 DR PEPPER 2.99\nITEM 90210",
        ));
        assert_eq!(found, MerchantDetails::default());
    }

    #[test]
    fn extracts_us_zip_only_in_address_context() {
        let found =
            extract_merchant_details(&lines("123 Main Street, Buffalo, NY 14201\nBANANAS 1.99"));
        assert_eq!(found.street_address.as_deref(), Some("123 Main Street"));
        assert_eq!(found.city.as_deref(), Some("Buffalo"));
        assert_eq!(found.region.as_deref(), Some("NY"));
        assert_eq!(found.postal_code.as_deref(), Some("14201"));
    }
}
