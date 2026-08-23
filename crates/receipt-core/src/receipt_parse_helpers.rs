use regex::Regex;
use std::sync::OnceLock;

use crate::merchant_match::{self, MerchantFamily, MerchantMatch};
use crate::ocr_document::{OcrDocument, OcrLine};

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

/// A line adjacent to the banner joins it when it is at least this fraction of
/// the banner's height — i.e. it is another line of the same stacked logo rather
/// than body text. Costco's "WHOLESALE" is 0.67× its "COSTCO"; the receipt's
/// first address line is 0.53×, which is what sets the gap this sits in.
const STACKED_BANNER_MIN_RATIO: f64 = 0.6;

fn clean_merchant_candidate(value: &str) -> String {
    re_clean_merchant()
        .replace_all(value, "")
        .trim()
        .to_string()
}

/// Loose plausibility used by both the size-prior and the first-line fallback:
/// a confident, non-empty, non-date line that cleans to a usable candidate.
/// Returns the cleaned candidate, or `None` if the line can't be a merchant.
fn line_merchant_candidate(line: &OcrLine) -> Option<String> {
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
fn banner_by_size(lines: &[OcrLine]) -> Option<String> {
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

    let mut best: Option<usize> = None;
    for (index, line) in lines.iter().enumerate() {
        if line.center_y > cut {
            continue;
        }
        if line.height < MIN_BANNER_HEIGHT_RATIO * median_height {
            continue;
        }
        if re_banner_reject().is_match(&line.text) {
            continue;
        }
        if line_merchant_candidate(line).is_none() {
            continue;
        }
        if best.map_or(true, |b| line.height > lines[b].height) {
            best = Some(index);
        }
    }
    let best = best?;

    // A stacked display logo splits the name across two banner-sized lines —
    // Costco prints "COSTCO" over "WHOLESALE" — and the tallest line alone is
    // only half the name. Absorb vertically adjacent display lines outward from
    // the banner so the matcher sees "OSTC WHOLESALE" rather than "OSTC": the
    // former clears the fuzzy bar against the "COSTCO WHOLESALE" alias, the
    // latter does not.
    let mut first = best;
    let mut last = best;
    while first > 0 && joins_banner(&lines[first - 1], &lines[first], lines[best].height, cut) {
        first -= 1;
    }
    while last + 1 < lines.len()
        && joins_banner(&lines[last + 1], &lines[last], lines[best].height, cut)
    {
        last += 1;
    }

    let joined = (first..=last)
        .filter_map(|index| line_merchant_candidate(&lines[index]))
        .collect::<Vec<_>>()
        .join(" ");
    (!joined.is_empty()).then_some(joined)
}

/// Whether `cand` is another line of the same stacked logo as `neighbor`, given
/// the height of the banner line the walk started from.
///
/// Body text fails the height ratio; a date/address/price row fails
/// [`re_banner_reject`]; a line printed far below the logo fails the adjacency
/// check, whose budget is the two lines' mean height — roughly "no more than one
/// display line of blank space between them".
fn joins_banner(cand: &OcrLine, neighbor: &OcrLine, banner_height: f64, cut: f64) -> bool {
    cand.center_y <= cut
        && cand.height >= STACKED_BANNER_MIN_RATIO * banner_height
        && !re_banner_reject().is_match(&cand.text)
        && line_merchant_candidate(cand).is_some()
        && (cand.center_y - neighbor.center_y).abs() <= (cand.height + neighbor.height) / 2.0
}

pub fn extract_merchant_with_confidence(doc: &OcrDocument) -> Option<String> {
    // Prefer a dominant large-font banner when one exists...
    if let Some(banner) = banner_by_size(&doc.lines) {
        return Some(banner);
    }

    // ...otherwise fall back to the first plausible header line (historical
    // behavior), bounded to the first 10 lines of the header region.
    doc.lines.iter().take(10).find_map(line_merchant_candidate)
}

/// Best-effort OCR'd merchant name straight from the receipt header, *before*
/// any canonicalization — prefer the highest-confidence early line, then fall
/// back to the first plausible non-date line. This is the `raw` the matcher
/// preserves; `"UNKNOWN_MERCHANT"` is the last-resort sentinel (kept for parity
/// with the prior behavior).
fn ocr_header_candidate(lines: &[String], doc: &OcrDocument) -> String {
    if let Some(confident) = extract_merchant_with_confidence(doc) {
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
    doc: &OcrDocument,
    known_merchants: &[String],
    families: &[MerchantFamily],
) -> MerchantMatch {
    let raw = ocr_header_candidate(lines, doc);
    let full_text_upper = full_text.to_ascii_uppercase();
    merchant_match::resolve(&raw, &full_text_upper, known_merchants, families)
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
    use super::{extract_merchant_match, extract_merchant_with_confidence};
    use crate::merchant_match::MerchantFamily;
    use crate::ocr_document::{Bbox, OcrDocument, OcrLine, OcrWord};

    /// One header line with a single word at the given confidence, box `height`,
    /// and line `center_y` — the geometry the size-prior reads. `height` and
    /// `center_y` are ratios of image height, and only ever compared with each
    /// other, so these fixtures keep the pixel-ish magnitudes they were written
    /// with rather than restating them as fractions.
    fn mline(text: &str, confidence: f64, height: f64, center_y: f64) -> OcrLine {
        OcrLine {
            text: text.to_string(),
            words: vec![OcrWord {
                text: text.to_string(),
                bbox: Bbox {
                    left: 0.0,
                    top: 0.0,
                    right: 1.0,
                    bottom: 1.0,
                },
                confidence,
            }],
            height,
            center_y,
        }
    }

    fn one_page(lines: Vec<OcrLine>) -> OcrDocument {
        OcrDocument { lines }
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
        extract_merchant_match(
            &lines,
            full_text,
            &OcrDocument::default(),
            &["COSTCO".to_string()],
            &families(),
        )
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
    fn stacked_banner_halves_join_into_one_candidate() {
        // costco/2026-07-22_costco_67_82, real line geometry: the logo stacks
        // "COSTCO" (h=208) over "WHOLESALE" (h=140). Only the taller half clears
        // the 1.8x-median bar, but the half alone ("OSTC", the C dropped by OCR)
        // is too far from "COSTCO" to correct. Joined, it clears the fuzzy bar
        // against the "COSTCO WHOLESALE" alias.
        let pages = one_page(vec![
            mline("OSTC", 0.96, 208.0, 374.0),
            mline("WHOLESALE", 0.99, 140.0, 502.0),
            mline("Markham #545", 0.99, 111.0, 628.0),
            mline("65 Kirkham Drive", 1.0, 94.0, 718.0),
            mline("1424970 CASHMERE TP 26.99 H", 0.99, 88.0, 1235.0),
            mline("430 XL EGGS 9.69", 0.99, 82.0, 1389.0),
        ]);
        assert_eq!(
            extract_merchant_with_confidence(&pages).as_deref(),
            Some("OSTC WHOLESALE")
        );
    }

    #[test]
    fn body_text_under_the_banner_does_not_join_it() {
        // The join must stop at the logo: the address line below Loblaws is
        // nowhere near banner height, so the candidate stays the banner alone.
        let pages = one_page(vec![
            mline("Loblaws", 0.95, 75.0, 65.0),
            mline("123 Main Street", 0.95, 28.0, 110.0),
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
