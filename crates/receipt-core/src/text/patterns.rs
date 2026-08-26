//! Compiled regexes and price/tax pattern constants for text extraction.

use regex::Regex;
use std::sync::OnceLock;

use crate::common::{WEIGHT_UNIT_AT_SEP, WEIGHT_UNIT_CLASS};

// Canadian grocery receipts mark items with one or more trailing single-letter
// tax flags right after the price: H (HST), G (GST), P (PST), T (TAX),
// C (combined / container deposit eligible), F (food / non-taxable),
// J (sometimes used for joint promos). Receipts routinely combine them, e.g.
// "$1.79 HC" or "$8.99 HCF". The price-detection regexes below allow 0-3 of
// these letters in any case. Some merchants (e.g. Sunny Foodmart) suffix a
// category digit — "$3.88 Tx1" — so an optional 'x' and up to two trailing
// digits are tolerated, but only after at least one tax letter (so a bare
// trailing number like "9.99 5" is not silently swallowed as a flag).
// Some merchants print a '*' immediately before the tax letter(s) — e.g.
// FreshCo's "$25.96*HC" — so an optional leading asterisk is tolerated; without
// it the whole price token fails to parse and the line is silently dropped.
// T&T prints its flags as separate space-separated letters — "W $12.81 G F"
// arrives as ONE OCR token — so the group repeats over whitespace rather than
// matching a single contiguous run. `*` (not `+`) keeps the whole class
// optional, which is what every call site assumed when this was `?`.
pub(crate) const TAX_FLAG_CLASS: &str = r"(?:\*?[BbCcFfGgHhJjPpTtXx]{1,3}\d{0,2}\s*)*";

// When the parser sees a bare standalone-price line (e.g. `$8.95` on its own)
// it walks back up to 5 lines looking for the description that goes with it.
// On some receipts the YOU SAVED amount escapes the skip patterns and lands
// as a bare price line right after a previously-emitted complete item (e.g.
// the Freshco "Cherries Red $6.69 C" / "YOU SAVED" / "$8.95" cluster). The
// walk then re-grabs the already-paired "Cherries Red $6.69 C" description
// and produces a ghost duplicate item at the wrong (savings) price.
//
// With this guard on, the backward walk skips candidates that already end in
// a trailing price — such a line is a fully-formed item, not a dangling
// description. The guard fires ONLY when the line being processed is a bare
// price (no description, no quantity expression). Quantity-expression
// triggers like "1 @ $9.99 3.99" (where the receipt's column layout merged
// the next item's price onto the qty row) must keep their access to
// trailing-price prev-lines: the only description for those is the OCR-
// merged "ITEM NAME 9.99" line above. See e2e fixture
// `unknown-date_foody_martmccowan_121_99` for that shape.
//
// REVERT: flip to `false` if a future regression shows real items going
// missing because their description happened to end in a price-like token.
pub(crate) const SKIP_PRICED_LINES_IN_BACKWARD_DESC_SEARCH: bool = true;

/// Minimum count of drift witnesses before receipt-level price drift is
/// assumed. Three keeps one or two OCR flukes on a straight receipt from
/// flipping the pairing direction.
pub(crate) const PRICE_DRIFT_EVIDENCE_MIN: usize = 3;

pub(crate) fn re_skip_patterns() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)TOTAL|SUBTOTAL|SUB\s+TOTAL|TOTALS?\s+ON|^TAX$|^HST|^GST|^PST|AFTER\s+TAX|^\s*\d+\s*%$|^CASH\b|^CREDIT\b|^DEBIT\b|^CHANGE\b|^BALANCE|^VISA\b|^MASTERCARD\b|^AMEX\b|^APPROVED\b|^ACTIVATED\b|^PC\s+\d|^ACCT:|^ACCOUNT:|^REFERENCE|THANK YOU|WELCOME|RECEIPT|TRANSACTION|^POINTS\b|^REWARDS\b|^EARNED\b|^SAVED$|^YOU SAVED|PRICE\s+MATCH|^CARD|AUTH|REF\s*#|SLIP\s*#|^TILL|CASHIER|\bSTORE\b|^PHONE|ADDRESS|SIGNATURE|Merchant|^QTY$|^UNIT$|^SAV$|ITEM\s+COUNT|NUMBER\s+OF\s+ITEMS|XXXX+|^CAD|VERIFIED|^PIN$|CUSTOMER\s+COPY|COPY$|Optimum|Redeemed",
        )
        .unwrap()
    })
}

pub(crate) fn re_total_word() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\bTOTAL\b").unwrap())
}

pub(crate) fn re_tender_label() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^\s*(VISA|MASTERCARD|MASTER\s*CARD|AMEX|DEBIT|INTERAC)\b").unwrap()
    })
}

pub(crate) fn re_digits_only() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d+$").unwrap())
}

pub(crate) fn re_parenthetical_only() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\([^)]*\)?$").unwrap())
}

pub(crate) fn re_trailing_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!(r"(\d+\.\d{{2}})(-?)\s*{TAX_FLAG_CLASS}\s*$")).unwrap())
}

// Deliberately requires whitespace — not `\$?` — before the amount. A
// `$`-prefixed tail here would make every "6 @ $0.98" unit-price row look
// like it carries a total, and each would emit a phantom "6 @ $" item.
// A qty row that genuinely owns its trailing amount proves it by
// arithmetic instead; see `qty_row_owns_trailing_total`.
pub(crate) fn re_trailing_total_presence() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!(r"\s+\d+\.\d{{2}}\s*{TAX_FLAG_CLASS}\s*$")).unwrap())
}

// "<desc> <unit-price> <flags> <ext-price>" rows (e.g. Shoppers'
// "VICKS SINUS CO 20.99 GP 20.99") leave the unit price and tax flags
// dangling at the end of desc_part once the trailing extended price is
// consumed. The flag letters are mandatory here: a single-price line with
// flags is already fully consumed by re_trailing_price, so a bare trailing
// number never matches this and stays untouched.
pub(crate) fn re_embedded_unit_price_suffix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\s+\$?\d+\.\d{2}\s*\*?[CcFfGgHhJjPpTtXx]{1,3}\d{0,2}\s*$").unwrap()
    })
}

pub(crate) fn re_tail_token() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"([0-9A-Za-z]\.[0-9A-Za-z]{{2,3}}{TAX_FLAG_CLASS})\s*$"
        ))
        .unwrap()
    })
}

pub(crate) fn re_compact_space() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+").unwrap())
}

pub(crate) fn re_reg_price_marker() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|[^A-Z0-9])[0-9OI]?REG\$?\d+\.\d{2}").unwrap())
}

pub(crate) fn re_find_prices() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(\d+\.\d{2})").unwrap())
}

pub(crate) fn re_compact_promo_ghost() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(r"^[A-Z]{{1,5}}\$?\d+\.\d{{2}}{TAX_FLAG_CLASS}$")).unwrap()
    })
}

pub(crate) fn re_standalone_price_line() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!(r"^\$?\d+\.\d{{2}}\s*{TAX_FLAG_CLASS}\s*$")).unwrap())
}

pub(crate) fn re_long_digits_line() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d{8,}\s*$").unwrap())
}

pub(crate) fn re_weak_parenthetical() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\([^)]{1,12}\)$").unwrap())
}

pub(crate) fn re_weak_measure() -> &'static Regex {
    // Matches size-only fragments like "320g", "500ml", "1.5kg" — and the
    // OCR-mangled "+400g" form where the opening paren got transcribed as
    // `+`. These appear as the desc_part on Foody Mart Frozen-section rows
    // (`<size> <price>` with a trailing price the parser must hand to the
    // item description on the line BELOW, not the standalone size token).
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^[+]?\d+(?:\.\d+)?\s*(?:KG|G|LB|L|ML|OZ)$").unwrap())
}

pub(crate) fn re_malformed_price_marker() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d+\s*@\s*$").unwrap())
}

pub(crate) fn re_onsale_parenthetical() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\([#\w]*\)\s*<?\s*ON\s*SALE").unwrap())
}

pub(crate) fn re_price_info_line() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\$\d+\.\d{2}").unwrap())
}

pub(crate) fn re_parenthetical_closed() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\([^)]*\)$").unwrap())
}

pub(crate) fn re_parenthetical_multibuy() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\(\d+\s*/\s*for\s+\$[\d.]+\)").unwrap())
}

pub(crate) fn re_malformed_ocr_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"(\d+[Il]\.\d{{2}}|\d+\.[Il]\d|\d+\.\d[Il])\s*{TAX_FLAG_CLASS}\s*$"
        ))
        .unwrap()
    })
}

pub(crate) fn re_trailing_noisy_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!(r"(\d+)\.(\d{{3}})\s*{TAX_FLAG_CLASS}\s*$")).unwrap())
}

// A two-digit fraction where one digit was OCR'd as a letter (I/l for 1).
// Routes these through malformed-price reconciliation (which maps the letter
// back to a digit via levenshtein) instead of the warning-only path, e.g.
// "0.9I" -> 0.91 when the subtotal corroborates it.
pub(crate) fn re_trailing_letter_fraction_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"(\d+)\.([0-9][Il]|[Il][0-9]|[Il]{{2}})\s*{TAX_FLAG_CLASS}\s*$"
        ))
        .unwrap()
    })
}

pub(crate) fn re_count_at_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\d+)\s*@\s*\$?(-?\d+\.\d{2})").unwrap())
}

pub(crate) fn re_weight_at_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // The per-unit rate is captured (when readable) so the row's own total
    // is computable: "3.37 lb @ $2.98/lb" costs 10.04, so a trailing 7.45 on
    // that row can be recognized as another item's drifted price.
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"(?i)^(\d+\.?\d*)\s*{WEIGHT_UNIT_CLASS}{WEIGHT_UNIT_AT_SEP}@(?:\s*\$?(\d+\.\d{{2}}))?"
        ))
        .unwrap()
    })
}

pub(crate) fn re_weight_rate_no_at() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // OCR-dropped `@` variant: "1.86 lb  $2.49/lb". The `/unit` tail is
    // required so a bare "weight + total" line can't masquerade as a rate.
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"(?i)^(\d+\.?\d*)\s*{WEIGHT_UNIT_CLASS}\s+\$?(\d+\.\d{{2}})\s*/"
        ))
        .unwrap()
    })
}

pub(crate) fn re_multi_for_price() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\(?(\d+)\s*/\s*for\s+\$?(\d+\.\d{2})\)?").unwrap())
}

pub(crate) fn re_negative_unit_qty() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\d+\s*@\s*\$?-?\d+\.\d{2}\s*$").unwrap())
}

pub(crate) fn re_compact_offer_fragment() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\d+\s*@\s*\d+\s*/\s*\$?\d+\.\d{2}\b").unwrap())
}

// Compact "<qty>/$<price>" deal notation with no "@" and no "for", optionally
// prefixed by a bundle quantity -- e.g. FreshCo's "1/ $1.99" (the unit price
// printed under a multi-buy item) and "2  1/$6.44". These are unit-price
// detail rows, not items; without recognizing them they leak through as
// phantom items that inflate the total. The pattern is digits/punctuation only
// (no alphabetic content), so real product names can never match it.
pub(crate) fn re_compact_slash_deal() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"^(?:\d+\s+)?\d+\s*/\s*\$?\d+\.\d{{2}}\s*{TAX_FLAG_CLASS}\s*$"
        ))
        .unwrap()
    })
}

pub(crate) fn re_parenthetical_offer_prefix() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\([^)]+\)\s+\d+\s*/\s*for\b").unwrap())
}

pub(crate) fn re_section_header_with_aisle() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[^A-Z0-9]*\d{1,2}\s*[-:]\s*[A-Z]{3,}$").unwrap())
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

pub(crate) fn re_ascii_words() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[A-Z]+").unwrap())
}

pub(crate) fn re_summary_patterns() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^(?:SUB\s*TOTAL|SUBTOTAL|TOTAL|HST|GST|PST|TAX|MASTER(?:CARD)?|VISA|DEBIT|CREDIT|POINTS|CASH|CHANGE|BALANCE|APPROVED|CARD|TERMINAL|MEMBER)\b",
        )
        .unwrap()
    })
}

pub(crate) fn re_tax_tokens() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b(HST|GST|PST|TAX)\b").unwrap())
}

pub(crate) fn re_sale_price_subtext() -> &'static Regex {
    // OCR-merged sale-price subtext on Asian-grocery receipts:
    // "<size>)@<unit>(<qty>/$<deal>)" appended after the real description
    // because the opening paren before the size was lost and the closing
    // paren glued straight to the `@`. The discriminator is `)@<digit>`,
    // which never appears in legitimate item descriptions.
    //
    // Matches: " 6*60g)@5.99(1/$4.98)" → stripped. Does NOT match LCBO's
    // "(1 @ 19.75)" form (no `)@` and there's a space around the `@`).
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+\S*\)@\d+\.\d{2}.*$").unwrap())
}

pub(crate) fn re_size_paren_residue() -> &'static Regex {
    // Bare "<size>)" tail left when OCR drops the CJK text of a parenthetical
    // size line and its remainder merges into the description above
    // ("Shirakiku - Frozen Imitat 500g)"). The mandatory `)` keeps legitimate
    // size-bearing names like "POTATO 10LB" intact.
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\s+\d+(?:\.\d+)?\s*(?:KG|G|LB|L|ML|OZ)\)\s*$").unwrap())
}
