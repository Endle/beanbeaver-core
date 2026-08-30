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
            r"(?:^|[^0-9])((?:\+?1[ .-]?)?(?:\(\s*[2-9][0-9]{2}\s*\)|[2-9][0-9]{2})[ .-]?[2-9][0-9]{2}[ .-]?[0-9]{4})\b",
        )
        .expect("valid phone regex")
    })
}

fn phone_in(value: &str) -> Option<regex::Match<'_>> {
    let found = phone_re()
        .captures(value)
        .and_then(|captures| captures.get(1))?;
    let printed = found.as_str();
    let upper = value.to_ascii_uppercase();
    let formatted = printed
        .chars()
        .any(|ch| matches!(ch, '(' | ')' | '-' | '.' | ' '));
    let labelled = ["TEL", "PHONE", "CALL"]
        .iter()
        .any(|label| upper.contains(label));
    let digits_only = printed.chars().all(|ch| ch.is_ascii_digit());
    let looks_like_compact_date = digits_only
        && printed.len() == 10
        && (printed.starts_with("19") || printed.starts_with("20"));
    (formatted || labelled || !looks_like_compact_date).then_some(found)
}

fn store_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\bSTORE\s*(?:#|NO\.?|NUMBER|:)?\s*([A-Z0-9-]{3,})\b")
            .expect("valid store-number regex")
    })
}

fn lcbo_store_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\bST\s*:\s*([0-9]{3,4})\b").expect("valid LCBO store regex"))
}

fn warehouse_store_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\bWHSE\s*:\s*([0-9]{2,5})\b").expect("valid warehouse store regex")
    })
}

fn header_branch_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^.*#\s*([0-9]{2,5})\s*$").expect("valid header branch regex")
    })
}

fn freshco_store_row_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^\s*[0-9]{2,6}\s+([0-9]{3,6})\s+[0-9]{2,6}(?:\s|$)")
            .expect("valid FreshCo store-row regex")
    })
}

fn street_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b\d+[A-Z-]*\s+(?:(?:HWY|HIGHWAY)\s+\d+[A-Z-]*\b|(?:[A-Z0-9.'-]+\s+){0,8}(?:ST(?:REET)?|RD|ROAD|AVE(?:NUE)?|BLVD|BOULEVARD|DR(?:IVE)?|LANE|LN|COURT|CT|CIRCLE|CIR|PKWY|PARKWAY)\b\.?)(?:\s+(?:EAST|WEST|NORTH|SOUTH|E|W|N|S)\b)?",
        )
        .expect("valid street-address regex")
    })
}

fn wrapped_saint_street_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b\d+\s+ST\.?\s+(?:[A-Z0-9.'-]+\s+){0,6}(?:RD|ROAD|AVE(?:NUE)?|BLVD|BOULEVARD|DR(?:IVE)?|LANE|LN|COURT|CT|CIRCLE|CIR|PKWY|PARKWAY)\b\.?(?:\s+(?:EAST|WEST|NORTH|SOUTH|E|W|N|S)\b)?",
        )
        .expect("valid wrapped Saint street regex")
    })
}

fn trailing_price_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:^|\s)\$?\d+[.,]\d{2}-?\s*$").expect("valid trailing-price regex")
    })
}

fn street_in(line: &str) -> Option<(String, std::ops::Range<usize>)> {
    if trailing_price_re().is_match(line) {
        return None;
    }
    let found = street_re().find(line)?;
    Some((found.as_str().trim().to_string(), found.range()))
}

fn street_like(line: &str) -> bool {
    street_in(line).is_some()
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

fn city_region_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^(.+?)(?:,\s*|\s+)(ALBERTA|BRITISH COLUMBIA|MANITOBA|NEW BRUNSWICK|NEWFOUNDLAND(?: AND LABRADOR)?|NOVA SCOTIA|ONTARIO|PRINCE EDWARD ISLAND|QUEBEC|SASKATCHEWAN|(?-i:[A-Z]{2})\b)(?:\s*[, -]\s*.*)?$",
        )
        .expect("valid city/region regex")
    })
}

fn remove_phone(value: &str) -> String {
    let Some(found) = phone_in(value) else {
        return value.to_string();
    };
    format!("{} {}", &value[..found.start()], &value[found.end()..])
}

fn city_region_in(value: &str) -> (Option<String>, Option<String>) {
    let without_phone = remove_phone(value);
    let cleaned = without_phone
        .trim_matches(|c: char| c.is_ascii_whitespace() || c == ',' || c == '-')
        .trim();
    if cleaned.is_empty() {
        return (None, None);
    }

    if let Some(captures) = city_region_re().captures(cleaned) {
        let city = captures.get(1).and_then(|m| clean_component(m.as_str()));
        let region = captures.get(2).and_then(|m| clean_component(m.as_str()));
        let city_is_plausible = city
            .as_deref()
            .is_some_and(|city| !city.chars().any(|ch| ch.is_ascii_digit() || ch == '*'));
        if city_is_plausible {
            return (city, region);
        }
    }
    if cleaned.chars().any(|ch| ch.is_ascii_digit()) {
        return (None, None);
    }

    let words = cleaned.split_whitespace().count();
    let plausible = words <= 4
        && cleaned.chars().any(char::is_alphabetic)
        && !cleaned
            .chars()
            .any(|ch| matches!(ch, '#' | '*' | '/' | '\\'))
        && !cleaned.contains(',')
        && !cleaned.to_ascii_uppercase().contains("WWW.")
        && !cleaned.eq_ignore_ascii_case("HAPPY SHOPPING DAY");
    (plausible.then(|| cleaned.to_string()), None)
}

fn address_in(value: &str) -> (Option<String>, Option<String>, Option<String>) {
    let without_phone = remove_phone(value);
    let Some((street, range)) = street_in(&without_phone) else {
        let (city, region) = city_region_in(&without_phone);
        return (None, city, region);
    };
    let after = without_phone[range.end..]
        .trim_matches(|c: char| c.is_ascii_whitespace() || c == ',' || c == '-');
    let before = without_phone[..range.start]
        .trim_matches(|c: char| c.is_ascii_whitespace() || c == ',' || c == '-');
    let context = if after.is_empty() { before } else { after };
    let (city, region) = city_region_in(context);
    (clean_component(&street), city, region)
}

/// Extract the merchant details that the receipt itself prints.
///
/// Values fail independently: a receipt may yield only a postal code, store
/// number, or phone number. `raw_lines` retains the evidence for later parser
/// improvements and auditing.
pub fn extract_merchant_details(lines: &[String]) -> MerchantDetails {
    let mut out = MerchantDetails::default();

    // Phone semantics are intentionally deterministic when a receipt prints
    // several numbers: keep the first valid NANP number in receipt order.
    for line in lines.iter().map(|line| line.trim()) {
        if let Some(found) = phone_in(line) {
            out.phone_number = Some(found.as_str().trim().to_string());
            add_raw(&mut out.raw_lines, line);
            break;
        }
    }

    // Prefer an explicitly labelled store number. Some merchant layouts use
    // a compact terminal label or an unlabelled branch number in the header;
    // FreshCo prints its store in a labelled footer table.
    for line in lines.iter().map(|line| line.trim()) {
        if let Some(value) = store_re()
            .captures(line)
            .and_then(|captures| captures.get(1))
            .map(|found| found.as_str())
        {
            // Reject ordinary phrases such as "store manager" while retaining
            // OCR-damaged identifiers like Walmart's 30E3.
            if value.chars().any(|ch| ch.is_ascii_digit()) {
                out.store_number = Some(value.to_string());
                add_raw(&mut out.raw_lines, line);
                break;
            }
        }
    }
    if out.store_number.is_none() {
        for line in lines.iter().map(|line| line.trim()) {
            if let Some(value) = lcbo_store_re()
                .captures(line)
                .and_then(|captures| captures.get(1))
            {
                out.store_number = Some(value.as_str().to_string());
                add_raw(&mut out.raw_lines, line);
                break;
            }
        }
    }
    if out.store_number.is_none() {
        for line in lines.iter().map(|line| line.trim()) {
            if let Some(value) = warehouse_store_re()
                .captures(line)
                .and_then(|captures| captures.get(1))
            {
                out.store_number = Some(value.as_str().to_string());
                add_raw(&mut out.raw_lines, line);
                break;
            }
        }
    }
    if out.store_number.is_none() {
        for line in lines.iter().take(12).map(|line| line.trim()) {
            let upper = line.to_ascii_uppercase();
            if ["HST", "GST", "TAX", "BUSINESS", "TRANS", "REF", "TERM"]
                .iter()
                .any(|label| upper.contains(label))
            {
                continue;
            }
            if let Some(value) = header_branch_re()
                .captures(line)
                .and_then(|captures| captures.get(1))
            {
                out.store_number = Some(value.as_str().to_string());
                add_raw(&mut out.raw_lines, line);
                break;
            }
        }
    }
    if out.store_number.is_none() {
        for (index, line) in lines.iter().enumerate() {
            if !line.to_ascii_uppercase().contains("STORE OPER") {
                continue;
            }
            for row in lines
                .iter()
                .take((index + 4).min(lines.len()))
                .skip(index + 1)
                .map(|line| line.trim())
            {
                if let Some(value) = freshco_store_row_re()
                    .captures(row)
                    .and_then(|captures| captures.get(1))
                {
                    out.store_number = Some(value.as_str().to_string());
                    add_raw(&mut out.raw_lines, row);
                    break;
                }
            }
            break;
        }
    }

    for (index, original) in lines.iter().enumerate() {
        let line = original.trim();
        if line.is_empty() {
            continue;
        }

        if let Some((postal, range)) = postal_in(line) {
            if out.postal_code.is_none() {
                out.postal_code = Some(postal);
            }
            add_raw(&mut out.raw_lines, line);

            let without_postal = format!("{} {}", &line[..range.start], &line[range.end..]);
            let (same_street, same_city, same_region) = address_in(&without_postal);
            if out.street_address.is_none() {
                out.street_address = same_street;
            }
            // The postal line is the strongest city/region context and may
            // replace an earlier, provisional neighbouring-line candidate.
            if same_city.is_some() {
                out.city = same_city;
            }
            if same_region.is_some() {
                out.region = same_region;
            }

            if out.city.is_none() || out.region.is_none() {
                let start = index.saturating_sub(2);
                let end = (index + 2).min(lines.len().saturating_sub(1));
                for nearby in &lines[start..=end] {
                    let nearby = nearby.trim();
                    let (city, region) = city_region_in(nearby);
                    if region.is_none() {
                        continue;
                    }
                    if out.city.is_none() {
                        out.city = city;
                    }
                    if out.region.is_none() {
                        out.region = region;
                    }
                    add_raw(&mut out.raw_lines, nearby);
                    break;
                }
            }

            if out.street_address.is_none() {
                let start = index.saturating_sub(3);
                let end = (index + 2).min(lines.len().saturating_sub(1));
                for nearby in lines[start..=end].iter().rev() {
                    let nearby = nearby.trim();
                    if let Some((street, _)) = street_in(nearby) {
                        out.street_address = Some(street);
                        add_raw(&mut out.raw_lines, nearby);
                        break;
                    }
                }
            }
        }

        let wrapped_street = index.checked_sub(1).and_then(|i| {
            let wrapped = format!("{} {}", line, lines[i].trim());
            wrapped_saint_street_re()
                .find(&wrapped)
                .map(|found| found.as_str().trim().to_string())
        });
        let has_wrapped_street = wrapped_street.is_some();
        let (street, same_city, same_region) =
            wrapped_street.map_or_else(|| address_in(line), |street| (Some(street), None, None));
        let Some(street) = street else {
            continue;
        };
        if out.street_address.is_none() {
            out.street_address = Some(street);
        }
        if out.city.is_none() {
            out.city = same_city;
        }
        if out.region.is_none() {
            out.region = same_region;
        }
        add_raw(&mut out.raw_lines, line);
        if has_wrapped_street {
            add_raw(&mut out.raw_lines, lines[index - 1].trim());
        }

        // Split layouts put `City, Region[, Postal]` or `City + phone` just
        // below the street. Prefer an explicit region; keep a plain city only
        // as a fallback until a later postal line supplies stronger context.
        let mut city_fallback = None;
        for nearby in lines
            .iter()
            .take((index + 2).min(lines.len()))
            .skip(index + 1)
        {
            let nearby = nearby.trim();
            if nearby.is_empty() || street_in(nearby).is_some() {
                continue;
            }
            let without_postal = postal_in(nearby).map_or_else(
                || nearby.to_string(),
                |(_, range)| format!("{} {}", &nearby[..range.start], &nearby[range.end..]),
            );
            let (city, region) = city_region_in(&without_postal);
            if region.is_some() {
                if out.city.is_none() {
                    out.city = city;
                }
                if out.region.is_none() {
                    out.region = region;
                }
                add_raw(&mut out.raw_lines, nearby);
                city_fallback = None;
                break;
            }
            if city_fallback.is_none() && city.is_some() && phone_in(nearby).is_some() {
                city_fallback = Some((city, nearby));
            }
        }
        if out.city.is_none() {
            if let Some((city, evidence)) = city_fallback {
                out.city = city;
                add_raw(&mut out.raw_lines, evidence);
            }
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

    #[test]
    fn extracts_numbered_highway_and_split_city_line() {
        let found = extract_merchant_details(&lines(
            "FOODY MART\n5221 Highway 7\nMarkham, Ontario, L3R 1N3\n(905)305-9866",
        ));
        assert_eq!(found.street_address.as_deref(), Some("5221 Highway 7"));
        assert_eq!(found.city.as_deref(), Some("Markham"));
        assert_eq!(found.region.as_deref(), Some("Ontario"));
        assert_eq!(found.postal_code.as_deref(), Some("L3R 1N3"));
        assert_eq!(found.phone_number.as_deref(), Some("(905)305-9866"));
    }

    #[test]
    fn extracts_reversed_city_postal_street_line() {
        let found = extract_merchant_details(&lines(
            "Scarborough, Ontario, M1S 3M7 175 Commander Blvd.\n(416)293-8882",
        ));
        assert_eq!(found.street_address.as_deref(), Some("175 Commander Blvd."));
        assert_eq!(found.city.as_deref(), Some("Scarborough"));
        assert_eq!(found.region.as_deref(), Some("Ontario"));
        assert_eq!(found.postal_code.as_deref(), Some("M1S 3M7"));
    }

    #[test]
    fn extracts_costco_header_branch() {
        let found = extract_merchant_details(&lines(
            "N Oshawa #1591\n100 Windfields Farm Drive East\nOshawa, ON L1L 0R8",
        ));
        assert_eq!(found.store_number.as_deref(), Some("1591"));
        assert_eq!(
            found.street_address.as_deref(),
            Some("100 Windfields Farm Drive East")
        );
    }

    #[test]
    fn falls_back_to_costco_warehouse_footer() {
        let found = extract_merchant_details(&lines(
            "COSTCO\n65 Kirkham Drive\nMarkham, ON L3S 0A9\nWhse:545 Trm:8",
        ));
        assert_eq!(found.store_number.as_deref(), Some("545"));
    }

    #[test]
    fn extracts_lcbo_terminal_store_and_spaced_phone() {
        let found = extract_merchant_details(&lines(
            "1571 Sandhurst Circle\nTORONTO-SCARBOROUGH, ON M1V-1V2\n( 416)291-1638\nST:0584",
        ));
        assert_eq!(found.store_number.as_deref(), Some("0584"));
        assert_eq!(found.phone_number.as_deref(), Some("( 416)291-1638"));
        assert_eq!(
            found.street_address.as_deref(),
            Some("1571 Sandhurst Circle")
        );
    }

    #[test]
    fn extracts_freshco_footer_store_and_ignores_terminal_id_as_phone() {
        let found = extract_merchant_details(&lines(
            "9580 Mccowan Road\nMarkham (905) 887-4366\n0000008000\n2026706720\nTerm Store Oper\nTran 20:15:16\n3993 3875 111",
        ));
        assert_eq!(found.city.as_deref(), Some("Markham"));
        assert_eq!(found.phone_number.as_deref(), Some("(905) 887-4366"));
        assert_eq!(found.store_number.as_deref(), Some("3875"));
    }
}
