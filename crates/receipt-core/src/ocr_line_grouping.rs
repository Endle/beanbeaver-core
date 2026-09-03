//! Group post-OCR detections into reading-order lines.
//!
//! Item-first matching that mirrors the receipt layout: SKU/summary tokens on
//! the left are paired with the first vertically-overlapping price on the
//! right, then middle-column descriptions attach to the nearest line. Pure
//! geometry; the Python wrapper marshals detection dicts and builds the OCR
//! schema. Operates on the shared [`Detection`] view and returns groups of
//! source indices so the caller keeps the original dicts intact.

use std::cmp::Ordering;
use std::sync::OnceLock;

use regex::Regex;

use crate::detection_normalization::{boxes_overlap_y, Detection};

fn summary_label_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^\s*(?:SUB\s*T[OCQDG0]TAL|SUBTOTAL|TOTAL|HST|GST|PST|TAX|MASTER(?:CARD)?|VISA|DEBIT|CREDIT|POINTS|CASH|CHANGE|BALANCE|APPROVED|CARD|TERMINAL|MEMBER|AMOUNT|REFERENCE|AUTH)\b",
        )
        .unwrap()
    })
}

/// Exact receipt-summary labels are row anchors on self-checkout layouts, not
/// descriptions that should be absorbed into a neighbouring item.
///
/// Most receipts print these inside the LEFT transition band, where
/// [`belongs_in_left_column`] already catches them. Shoppers 2026-06-30 is much
/// wider: `SUBTOTAL` starts at x=0.35 and both amount columns at x=0.68, so the
/// fixed RIGHT cut routes the entire block to MIDDLE. With no summary anchor,
/// progressive middle attachment grows one line containing the item, its two
/// prices and `SUBTOTAL`, and the summary filter then correctly drops it. Keep
/// this exact and narrow so prose such as `TOTAL POINTS EARNED TODAY` remains a
/// normal middle token. The caller requires the document-level `SCO CheckOut`
/// banner: promoting centered summary labels globally changes valid LCBO and
/// Pharmasave live layouts. It also limits anchors to the left of
/// [`RIGHT_COLUMN_CUT`]: a far-right `Total` is an item-table column heading,
/// as on Home Hardware, and must stay on the `SKU Qty Price Total` row.
fn is_summary_anchor_label(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^\s*(?:SUB\s*T[OCQDG0]TAL|SUBTOTAL|TOTAL|HST|GST|PST|TAX)\s*:?\s*$")
            .unwrap()
    })
    .is_match(text)
}

/// Decide whether a detection sits in the LEFT (SKU/summary-label) column.
///
/// `x_norm < 0.2` is unambiguously LEFT. In the 0.2-0.3 transition band the
/// answer depends on content: numeric SKU-style tokens (digit-led) and summary
/// labels (TOTAL, TAX, …) belong on the LEFT; alpha-led short tokens like
/// Costco's `CRAISINS 1.8` are descriptions and belong in MIDDLE.
fn belongs_in_left_column(text: &str, x_norm: f64) -> bool {
    if x_norm < 0.2 {
        return true;
    }
    if x_norm >= 0.3 {
        return false;
    }
    let stripped = text.trim_start();
    let Some(first) = stripped.chars().next() else {
        return false;
    };
    if first.is_ascii_digit() {
        return true;
    }
    summary_label_re().is_match(stripped)
}

/// Adaptive Y-threshold for middle-column line merges. Larger text/blur ->
/// larger tolerance, clamped to avoid cross-row merges.
fn adaptive_middle_y_threshold(dets: &[Detection]) -> f64 {
    let mut heights: Vec<f64> = dets
        .iter()
        .map(|det| det.y_max - det.y_min)
        .filter(|height| *height > 0.0)
        .collect();
    if heights.is_empty() {
        return 24.0;
    }
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let median_height = heights[heights.len() / 2];
    (median_height * 0.8).clamp(12.0, 30.0)
}

/// What kind of RIGHT-column amount a LEFT-column label is allowed to claim.
///
/// The right column leans up on skewed/curled receipts — by two-thirds of a row
/// on the worst corpus cases — so a sub-line printed *above* an item can overlap
/// that item's price and claim it first. The item is then dropped for having no
/// price, and because prices are consumed one-to-one in reading order, every
/// following row inherits its neighbour's amount until something breaks the
/// chain. Typing the label is what breaks it: a row that cannot legally hold the
/// amount in front of it declines, and the whole downstream run re-aligns on its
/// own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AmountClaim {
    /// Ordinary item or summary row: takes the first overlapping amount.
    Any,
    /// Row that never carries an amount of its own — code stubs, POS headers,
    /// quantity breakdowns, and already-priced-in savings notices.
    Never,
    /// Reduction row: the amount that belongs to it is *negative*, so a positive
    /// (tax-coded) price overlapping it belongs to a neighbour instead.
    NegativeOnly,
    /// Loyalty row: what belongs to it is a points figure (`125 PTS`), never
    /// money, so a price overlapping it is the neighbouring item's.
    PointsOnly,
}

impl AmountClaim {
    /// Whether `text` — a RIGHT-column amount — is one this label may take.
    fn accepts(self, text: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Never => false,
            Self::NegativeOnly => is_negative_amount(text),
            Self::PointsOnly => is_points_amount(text),
        }
    }
}

/// True for a RIGHT-column token that could be an amount at all — i.e. one that
/// contains a digit.
///
/// The RIGHT partition is defined geometrically (`x_norm > 0.7`), so it holds
/// more than the price column: several chains print a narrow per-line tax-code
/// column further right still — Costco's `H` / `HH` / `E`, the Loblaw chains'
/// `MRJ` / `HMRJ` / `RQ`. A bare code is not an amount in any convention, so no
/// label may satisfy its claim with one.
///
/// This decides *partitioning*, not claim typing: a code is not an amount that
/// some rows may not have, it is not an amount at all, so it never enters the
/// price column to be claimed. Letting one stand in for a price is the same
/// cascade [`AmountClaim`] exists to stop, entered from the other side — the row
/// looks paid-for, its real amount falls through to the row below, and every
/// following row inherits its neighbour's. costco/2026-03-05_costco_245_87 loses
/// eight items to it, starting where `3966510 FO TANK S` claims the `HH` printed
/// level with it and hands its own 19.99 to `TPD TANK TOP`.
///
/// Routing to MIDDLE rather than refusing the token inside the pairing loop is
/// what keeps this from costing anything: the code still attaches to the row it
/// was printed on, it just cannot be mistaken for that row's money. Refusing it
/// in the loop instead leaves it an *orphan line*, which pulls MIDDLE text onto
/// itself and away from the row that needed it — no_frills/2026-03-06 reads its
/// date as 2006-03-26 that way, the `DateTime:` label having lost the timestamp
/// it was labelling.
fn is_amount_shaped(text: &str) -> bool {
    text.contains(|c: char| c.is_ascii_digit())
}

/// True for a loyalty-points figure rather than money — `125 PTS`, `1300 PTS`.
/// A currency marker disqualifies it, so a tax-coded price never reads as points.
fn is_points_amount(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    !text.contains('$')
        && RE
            .get_or_init(|| Regex::new(r"(?i)\d\s*(?:PTS|POINTS)\b").unwrap())
            .is_match(text)
}

/// True for a negative amount, in either of the two conventions receipts use:
/// a leading sign (`-$6.00`, `$-6.00`) or a trailing one (`6.00-`, common on
/// older POS printers).
fn is_negative_amount(text: &str) -> bool {
    let trimmed = text.trim();
    // A run of dashes is a rule separator, not an amount, so require a digit
    // before reading either sign convention.
    if !trimmed.contains(|c: char| c.is_ascii_digit()) {
        return false;
    }
    let leading = trimmed.trim_start_matches(['$', '(', ' ']).starts_with('-');
    let trailing = trimmed
        .trim_end_matches([' ', ')'])
        .strip_suffix('-')
        .is_some_and(|head| head.trim_end().ends_with(|c: char| c.is_ascii_digit()));
    leading || trailing
}

/// The claim a LEFT-column label may make on the right column. Everything not
/// recognised here is an ordinary row and takes the next amount, so a wrong
/// answer costs at most the row it names — but see the cascade note on
/// [`AmountClaim`]: a label typed `Never` by mistake hands its price to the row
/// below and shifts the rest of the receipt, so these patterns are deliberately
/// narrow and anchored.
///
/// The vocabulary here is Sobeys/FreshCo's (`INSTANT SAVINGS`, `YOU SAVED`);
/// other chains word it differently (Costco prints `TPD/<sku>`), which is why
/// this wants to move into the merchant rules data rather than grow inline.
fn amount_claim(text: &str) -> AmountClaim {
    if is_code_stub_label(text)
        || is_transaction_id_label(text)
        || is_self_checkout_header_label(text)
        || is_membership_label(text)
        || is_department_header_label(text)
        || is_priced_in_savings_label(text)
    {
        return AmountClaim::Never;
    }
    if is_reduction_label(text) {
        return AmountClaim::NegativeOnly;
    }
    if is_points_label(text) {
        return AmountClaim::PointsOnly;
    }
    AmountClaim::Any
}

/// True for the loyalty rows that report points rather than money — FreshCo
/// prints `POINTS EARNED` between two item lines, where it is well placed to
/// swallow the second one's price.
fn is_points_label(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\s*(?:TOTAL\s+)?POINTS?\s+EARNED\b").unwrap())
        .is_match(text)
}

/// True for gift-card activation code rows: a "PC" prefix followed by nothing
/// but a long digit run (e.g. Costco's "PC 339919953764897" printed between a
/// gift-card label and its amount). The prefix is required: a bare digit run
/// can be a UPC row that legitimately carries its multi-line item's price
/// (Loblaw prints beer as "COORS LIGHT..." over "05716323055  13.99").
fn is_code_stub_label(text: &str) -> bool {
    let Some(rest) = text.trim().strip_prefix("PC") else {
        return false;
    };
    let rest = rest.trim();
    rest.len() >= 9 && rest.chars().all(|ch| ch.is_ascii_digit())
}

/// True for POS transaction-id header rows: the word "Transaction" followed by
/// nothing but a digit run (Clover prints "Transaction 037972" directly above
/// the first item). Like code stubs, the row never carries an amount, so it
/// must not steal the first item's price when the right column leans up
/// (Jin Lian unknown-date_jin_lian_food_39_99: the header overlapped
/// FESHRIMP PASTE's $11.92 at ~0.32 and claimed it, dropping the item).
fn is_transaction_id_label(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.len() < 11 || !trimmed[..11].eq_ignore_ascii_case("TRANSACTION") {
        return false;
    }
    let rest = trimmed[11..].trim_start_matches([' ', '#', ':']).trim();
    rest.len() >= 4 && rest.chars().all(|ch| ch.is_ascii_digit())
}

/// True for the self-checkout banner some POS systems print directly above the
/// first item — `SCO CheckOut` (self-checkout).
///
/// Like a transaction-id header, this row never carries money. On Shoppers'
/// 2026-03-08 receipt the right column leans upward just far enough that the
/// banner overlaps CREST's tax-coded `9.99 5` before CREST does. First-fit then
/// hands every item the following row's price and drops the last one. Keep the
/// shape narrow and anchored: `SCO` plus `CHECKOUT`/`CHECK OUT` is structural;
/// a product description that merely contains either word is not.
fn is_self_checkout_header_label(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\s*SCO\s+CHECK\s*OUT\s*$").unwrap())
        .is_match(text)
}

/// True for a warehouse-club membership header — Costco's
/// `Member 111942685019`, printed directly above the first item.
///
/// Same family as the code stubs and transaction ids above, and the same failure:
/// the row never carries an amount, so when the right column leans up it is
/// perfectly placed to swallow the first item's price and shift the rest of the
/// receipt. costco/2026-03-10_costco_16_38 loses *both* its items that way — the
/// header takes 6.69, `2% FINE-FILT` inherits `XL EGGS`'s 9.69, and the drift
/// runs on through SUBTOTAL and TAX until TOTAL is left with nothing.
///
/// The trailing digit run is required, which is what separates this from the
/// rows that only *mention* membership and do carry amounts — Real Canadian's
/// `Member Pricing` reduction line, FreshCo's masked `Member card number:
/// **x****062`. A leading OCR-noise token is tolerated because Costco's own
/// scans produce one ("00 Member 111942685019").
fn is_membership_label(text: &str) -> bool {
    let trimmed = text.trim();
    // Skip a leading noise token so "00 Member <digits>" reads the same as
    // "Member <digits>"; anything longer than that is not this header.
    let rest = match trimmed.split_once(char::is_whitespace) {
        Some((head, tail)) if !head.eq_ignore_ascii_case("MEMBER") => tail.trim_start(),
        _ => trimmed,
    };
    let Some(digits) = rest
        .get(..6)
        .filter(|head| head.eq_ignore_ascii_case("MEMBER"))
        .map(|_| rest[6..].trim_start_matches([' ', '#', ':', '.']).trim())
    else {
        return false;
    };
    digits.len() >= 6 && digits.chars().all(|ch| ch.is_ascii_digit())
}

/// True for the section header the Loblaw chains print above each group of
/// items — `21-GROCERY`, `22-DAIRY`, `27-PRODUCE`.
///
/// Same family as the code stubs and membership headers above: a row that never
/// carries money, sitting where it can swallow a neighbour's price. It is worse
/// placed than those, because it appears *between* every pair of item groups
/// rather than once at the top, and it prints at the left margin — shallower
/// than the item column, so [`yields_to_price_column`] reads it as a banner and
/// lets it keep what it takes. real_canadian/rcss_20260130 loses two amounts to
/// it directly (`22-DAIRY` takes the 9.00 belonging to the `6 @ $1.50` above it,
/// `23-FROZEN` takes 2.49) and the rows below inherit the shift.
///
/// Shape only, no vocabulary: the corpus's own OCR renders these as `31-NEATS`,
/// `41-HONE`, `26-L.IQUOR` and `42-ENTERTAINNENT`, so matching department names
/// would miss exactly the noisy rows that cause trouble. Two digits and a dash
/// is not a shape an item row can take — SKUs run 8-11 digits, and a quantity
/// breakdown carries an `@`. Requiring letters after the dash is what keeps
/// dates and masked card numbers out.
fn is_department_header_label(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\s*\d{2}\s*[-–]\s*[A-Z][A-Z .&/]*$").unwrap())
        .is_match(text.trim())
}

/// True for the quantity/unit-price breakdown a multi-buy item prints under its
/// description — `2 @ 1/ $8.99`, `6 @ 1/$8.99`, `1.280 kg @ $1.52 / kg`.
///
/// Whether such a row carries an amount is a **per-chain layout choice, and the
/// corpus contains both**: FreshCo prints the extended price on the description
/// row and leaves the breakdown bare, while Loblaw chains print it on the
/// breakdown itself (No Frills: `2 @ $9.54 ea` … `19.08`). So this predicate
/// only identifies the shape — the caller decides from the surrounding receipt,
/// not from the merchant.
///
/// Anchored on a leading quantity so it can't match a description that merely
/// mentions "@".
fn is_quantity_breakdown_label(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\s*\d+(?:[.,]\d+)?\s*(?:kg|lbs?|g|ea)?\s*@").unwrap())
        .is_match(text)
}

/// True for savings notices whose amount is *already reflected* in the item
/// price above them and is printed inline in the label itself — FreshCo's
/// `YOU SAVED $2.00`. These are informational: they are not part of the
/// subtotal, so they must neither claim a right-column price nor become a line
/// item. (On the corpus's FreshCo receipts the `YOU SAVED` amounts sum with the
/// `INSTANT SAVINGS` ones to the printed "Your Total Savings", which is what
/// confirms the two are different kinds of thing.)
fn is_priced_in_savings_label(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\s*YOU\s+SAVED\b").unwrap())
        .is_match(text)
}

/// True for a real line-item reduction — a row whose negative amount *is* part
/// of the subtotal (FreshCo's `INSTANT SAVINGS`), as opposed to the priced-in
/// notices above. Anchored at the start so the summary rows that merely contain
/// the word ("Your Total Savings", "Discounts & Specials", both of which carry a
/// positive figure) keep claiming their own amounts.
fn is_reduction_label(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\s*INSTANT\s+SAVINGS\b").unwrap())
        .is_match(text)
}

/// How far a following row may be and still be offered a yielded amount.
///
/// Two rows, which is one intervening sub-line. The amount is being handed
/// across a row boundary, not searched for: a longer reach starts crossing whole
/// items, and the receipts this exists for print the annotation directly above
/// the row that should have had the amount.
const INDENT_YIELD_LOOKAHEAD: usize = 2;

/// Detections closer than half a character cell share a print-grid column.
const INDENT_COLUMN_LINK: f64 = 0.5;

/// The long and short sides of a detection's quad — its width and height, both
/// invariant under the tilt that inflates an axis-aligned extent.
fn quad_extents(det: &Detection) -> (f64, f64) {
    if det.bbox.len() < 4 {
        return (0.0, det.y_max - det.y_min);
    }
    let side = |a: (f64, f64), b: (f64, f64)| ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    let (a, b) = (
        side(det.bbox[0], det.bbox[1]),
        side(det.bbox[1], det.bbox[2]),
    );
    (a.max(b), a.min(b))
}

/// Per-character advance of the receipt's body font, in pixels.
///
/// Receipt printers are monospace, so box width over character count recovers
/// the print grid's cell size. Restricted to rows of at least six characters (a
/// two-character token is mostly box padding) and to the modal height band (a
/// double-width SUBTOTAL or a display banner prints on a different grid).
///
/// This is the yardstick indentation has to be measured in, and a fraction of
/// image width is not it. Over the 123-receipt corpus a cell is 0.0146-0.0255 of
/// image width, median 0.0209 — so the 0.05-of-width bar an earlier attempt used
/// was about 2.4 cells wide, and a one-space indent could never have cleared it.
fn glyph_pitch(dets: &[Detection]) -> Option<f64> {
    let mut heights: Vec<f64> = dets
        .iter()
        .map(|det| quad_extents(det).1)
        .filter(|height| *height > 0.0)
        .collect();
    if heights.is_empty() {
        return None;
    }
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let median_height = heights[heights.len() / 2];

    let mut pitches: Vec<f64> = Vec::new();
    for det in dets {
        let chars = det.text.trim().chars().count();
        let (width, height) = quad_extents(det);
        if chars < 6 || height <= 0.0 || width <= 0.0 {
            continue;
        }
        if height < median_height * 0.75 || height > median_height * 1.25 {
            continue;
        }
        pitches.push(width / chars as f64);
    }
    if pitches.len() < 5 {
        return None;
    }
    pitches.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    Some(pitches[pitches.len() / 2])
}

/// The receipt's print grid: which column each LEFT row starts in, and the
/// column its rows normally pair from.
struct IndentGrid {
    /// Column index per position in the LEFT list, counted from the left margin.
    level: Vec<usize>,
    /// The column the most amount-claiming rows stand in.
    modal: usize,
}

/// Cluster LEFT rows' left edges into print-grid columns by single linkage at
/// [`INDENT_COLUMN_LINK`].
///
/// Chaining is the safe failure. A genuinely ragged left column — Costco prints
/// SKU rows, `PC` activation stubs and `***TOTAL` at three unrelated x — collapses
/// into one level, and a receipt with one level has every row on the modal
/// column, which switches the yield off entirely.
fn indent_levels(dets: &[Detection], left: &[usize], pitch: f64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..left.len()).collect();
    order.sort_by(|&a, &b| {
        dets[left[a]]
            .min_x
            .partial_cmp(&dets[left[b]].min_x)
            .unwrap_or(Ordering::Equal)
    });
    let mut level = vec![0usize; left.len()];
    let mut current = 0usize;
    for window in 0..order.len() {
        if window > 0 {
            let gap = dets[left[order[window]]].min_x - dets[left[order[window - 1]]].min_x;
            if gap > INDENT_COLUMN_LINK * pitch {
                current += 1;
            }
        }
        level[order[window]] = current;
    }
    level
}

/// The grid, built from a first pass's pairings. `None` when the receipt has no
/// measurable pitch or nothing claimed an amount, both of which disable the yield.
fn indent_grid(dets: &[Detection], left: &[usize], claims: &[Option<usize>]) -> Option<IndentGrid> {
    let pitch = glyph_pitch(dets)?;
    let level = indent_levels(dets, left, pitch);
    let mut counts: Vec<usize> = vec![0; level.iter().copied().max().map_or(0, |max| max + 1)];
    for (position, claim) in claims.iter().enumerate() {
        if claim.is_some() {
            counts[level[position]] += 1;
        }
    }
    let best = *counts.iter().max()?;
    if best == 0 {
        return None;
    }
    // Ties go to the shallowest column so the choice is deterministic.
    let modal = counts.iter().position(|&count| count == best)?;
    Some(IndentGrid { level, modal })
}

fn line_y_span(dets: &[Detection], line: &[usize]) -> (f64, f64) {
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for &index in line {
        min_y = min_y.min(dets[index].y_min);
        max_y = max_y.max(dets[index].y_max);
    }
    (min_y, max_y)
}

fn line_center_y(dets: &[Detection], line: &[usize]) -> f64 {
    let sum: f64 = line.iter().map(|&index| dets[index].center_y).sum();
    sum / line.len() as f64
}

fn line_overlap_ratio(dets: &[Detection], det_index: usize, line: &[usize]) -> f64 {
    let det = &dets[det_index];
    let (line_min, line_max) = line_y_span(dets, line);
    let overlap_start = det.y_min.max(line_min);
    let overlap_end = det.y_max.min(line_max);
    if overlap_start >= overlap_end {
        return 0.0;
    }
    let overlap = overlap_end - overlap_start;
    let det_height = (det.y_max - det.y_min).max(1e-6);
    let line_height = (line_max - line_min).max(1e-6);
    overlap / det_height.min(line_height)
}

/// Whether `det` and `line` plausibly belong to the *same* row of text, judged by
/// their centers rather than by raw overlap.
///
/// Overlap alone is not enough when the two boxes have very different heights: a
/// stacked display logo (Costco prints "COSTCO" over "WHOLESALE", each several
/// times the body height) puts one glyph's descender region inside the other's
/// ascender region, which is a healthy-looking overlap *ratio* — 0.33 on
/// costco/2026-07-22_costco_67_82 — even though the two are unmistakably separate
/// lines. Merged, they render in x-order as "WHOLESALE OSTC": the banner reversed,
/// which no merchant matcher can recover.
///
/// Two boxes genuinely sharing a row always have at least one center inside the
/// other's vertical span, so require that. Boxes that merely graze each other's
/// extremes — which is all a stacked logo does — fail it from both directions.
fn centers_agree(dets: &[Detection], det_index: usize, line: &[usize]) -> bool {
    let det = &dets[det_index];
    let (line_min, line_max) = line_y_span(dets, line);
    let line_cy = line_center_y(dets, line);
    (line_min <= det.center_y && det.center_y <= line_max)
        || (det.y_min <= line_cy && line_cy <= det.y_max)
}

fn distance_to_line_span(dets: &[Detection], det_index: usize, line: &[usize]) -> f64 {
    let center_y = dets[det_index].center_y;
    let (line_min, line_max) = line_y_span(dets, line);
    if line_min <= center_y && center_y <= line_max {
        0.0
    } else if center_y < line_min {
        line_min - center_y
    } else {
        center_y - line_max
    }
}

/// Stable sort of `indices` by each detection's `center_y`.
fn sort_by_center_y(dets: &[Detection], indices: &mut [usize]) {
    indices.sort_by(|&a, &b| {
        dets[a]
            .center_y
            .partial_cmp(&dets[b].center_y)
            .unwrap_or(Ordering::Equal)
    });
}

/// Lexicographic comparison of the middle-column placement score
/// `(overlap_rank, distance_to_span, center_distance)`.
fn score_less(a: (u8, f64, f64), b: (u8, f64, f64)) -> bool {
    match a.0.cmp(&b.0) {
        Ordering::Less => true,
        Ordering::Greater => false,
        Ordering::Equal => match a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal) {
            Ordering::Less => true,
            Ordering::Greater => false,
            Ordering::Equal => a.2 < b.2,
        },
    }
}

/// How much of a row's height must overlap an amount for the row to claim it.
///
/// Measured, not chosen. Sweeping it over the 123-receipt corpus falls away
/// monotonically as it loosens — 0.25→881, 0.20→872, 0.15→868, 0.10→855,
/// 0.05→839 critical items, against 885 at 0.3. The temptation is real, because
/// loosening *does* fix receipts whose two columns sit on different baselines:
/// on real_canadian/rcss_20260130 the amount column runs ~26px low against a
/// ~30px row pitch, so `06700000874` overlaps its own 1.25 by 0.177, declines
/// it, and every row below inherits its neighbour's amount. But the same
/// loosening lets the structural row next to an amount claim it everywhere else,
/// and that costs more than it earns. Those receipts need the structural rows
/// typed (see [`AmountClaim`]), not a wider window.
///
/// **rcss_20260130 has since been fixed, and not by moving this gate.** The
/// window is the wrong instrument because it is asked between the two tokens on
/// a row that are furthest apart, and so have accumulated the most drift; the
/// tokens in between have not. [`row_reach`] walks the row through them at this
/// same 0.3, which takes that receipt from 12/17 items to 14/17 — the corpus
/// from 1063 to 1065 critical items — with no other receipt in the 133-fixture
/// corpus changing at all. The gate stays at 0.3.
const PAIR_OVERLAP_GATE: f64 = 0.3;

/// Where the RIGHT (amount) column starts, as a fraction of image width.
///
/// A fixed fraction cannot be right for every receipt, and moving it does not
/// help. Swept over the 123-receipt corpus (critical items / totals, against
/// 893 / 121 here): 0.74→888, 0.68→891, 0.66→888. The two values that score
/// *higher* on items, 0.62→894 and 0.58→894, both do it by **losing a total**
/// (121/123 → 120/123), and a wrong total is the more severe defect. Lowering
/// the bar for every receipt admits, on every other receipt, the middle-column
/// token that was correctly excluded.
///
/// **Widening it per-receipt was tried too, and is also a net loss** — the
/// obvious next move, since a photographed receipt's price column is not a
/// straight vertical line and so straddles this cut. Measuring RIGHT's own left
/// edge and re-admitting price-shaped MIDDLE tokens within ~1.5 character cells
/// of it promotes exactly the right tokens (on lcbo/unknown-date_lcbo_74_35, the
/// 11.95 and 16.40 at 0.694/0.696 that belong with their eight column-mates at
/// 0.705-0.725) and still scores **888**, because promotion moves a token from
/// one pipeline to another rather than from nowhere to somewhere. In MIDDLE it
/// attaches to the *best-aligned* line; in RIGHT it must be claimed one-to-one
/// in reading order. LCBO prints the item name on one line and `SKU … price` on
/// the next, so the amount pairs with the SKU row and orphans the description
/// that owns it — that receipt goes 2/7 → **0/7**, and a clean costco 13/13
/// → 10/13. MIDDLE attachment is load-bearing for multi-line item layouts;
/// do not "fix" the partition without moving that too.
const RIGHT_COLUMN_CUT: f64 = 0.7;

/// The steepest a single printed row's baseline may run, in degrees, for
/// [`row_reach`] to follow it across the page.
///
/// A photographed receipt is not flat, and the deformation that matters here is
/// not the one [`crate::detection_normalization::deskew`] corrects. That pass
/// removes a *global* shear; a receipt held with a curl in it has a baseline
/// slope that varies down the page, and no single angle straightens it. The
/// no_frills receipt this was measured on runs 2.74 deg at its first item row,
/// 2.44, 1.52 and 1.13 over the next three, then settles at 0.3-0.6 deg for the
/// rest — so the sweep's whole-page minimum was 0.37 deg, correctly declined as
/// too small to matter, while the top of the item block was drifting most of a
/// row across the width of the page.
///
/// Swept over the 133-receipt corpus (critical items / totals, of 1176 / 133):
///
/// | deg | 1.5 | 2.0 | 3.0 | 4.0 | 5.0 | 6.0 | 8.0 |
/// |---|---|---|---|---|---|---|---|
/// | items | 1063 | 1063 | 1065 | 1065 | 1065 | 1065 | 1065 |
/// | totals | 131 | 131 | 131 | 131 | 131 | 131 | **130** |
///
/// Below 3 deg the walk cannot reach across the receipt that motivated it, which
/// tilts 2.74 deg. Above 6 deg it starts crossing rows: at 8 deg the item count
/// is unchanged but it is a *trade* — lcbo/unknown-date_lcbo_74_35 gains one and
/// costco/2026-04-26_costco_173_15 loses one — and a **total** goes with it,
/// which is the more severe defect. 4.0 is the middle of the flat 3-6 band.
const ROW_CHAIN_MAX_TILT_DEG: f64 = 4.0;

fn row_chain_max_tilt() -> f64 {
    static TAN: OnceLock<f64> = OnceLock::new();
    *TAN.get_or_init(|| ROW_CHAIN_MAX_TILT_DEG.to_radians().tan())
}

/// Every detection that sits on the same printed row as `anchor_index`, to its
/// right — the row followed hop by hop rather than assumed straight.
///
/// [`PAIR_OVERLAP_GATE`] asks whether a label's box and an amount's box overlap
/// *each other*, which on a tilted row is the hardest question on the receipt:
/// the two are the furthest-apart tokens on it, so they accumulate the most
/// drift. The tokens between them do not have that problem — neighbours on a row
/// are close enough in x that the drift between them is a fraction of a row — so
/// the row can be walked instead of jumped. Each hop is the same overlap test at
/// the same gate; what changes is that it is asked between adjacent tokens.
///
/// This is what separates a tilted row from the row below it, and the separation
/// is not marginal. On the no_frills receipt in [`ROW_CHAIN_MAX_TILT_DEG`],
/// `03120044526` overlaps its own `4.50` by **0.277** against the 0.3 gate and
/// declines it, while the `RH FLOUR ALL` row below overlaps that same amount by
/// **0.917** and takes it — and because amounts are consumed one-to-one in
/// reading order, every row under it inherits its neighbour's price and the last
/// one falls off the receipt as an orphan. Walking the row instead reaches the
/// amount through `COCKTAIL JCE` and `MRJ`, both of which overlap their
/// neighbours by more than 0.8.
///
/// The tilt cap is what stops the walk from wandering onto another row: a chain
/// is only followed while it stays within [`ROW_CHAIN_MAX_TILT_DEG`] of the
/// anchor's own baseline, measured from the anchor rather than hop to hop so the
/// error cannot accumulate.
fn row_reach(dets: &[Detection], anchor_index: usize, x_order: &[usize]) -> Vec<usize> {
    let anchor = &dets[anchor_index];
    let max_tilt = row_chain_max_tilt();
    let mut reached: Vec<usize> = Vec::new();
    for &index in x_order {
        if index == anchor_index {
            continue;
        }
        let det = &dets[index];
        let run = det.min_x - anchor.min_x;
        if run <= 0.0 || (det.center_y - anchor.center_y).abs() > run * max_tilt {
            continue;
        }
        let linked = boxes_overlap_y(anchor, det, PAIR_OVERLAP_GATE)
            || reached.iter().any(|&hop| {
                dets[hop].min_x < det.min_x && boxes_overlap_y(&dets[hop], det, PAIR_OVERLAP_GATE)
            });
        if linked {
            reached.push(index);
        }
    }
    reached
}

/// Detection indices ordered by `min_x`, the order [`row_reach`] walks a row in.
fn x_order(dets: &[Detection]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..dets.len()).collect();
    order.sort_by(|&a, &b| {
        dets[a]
            .min_x
            .partial_cmp(&dets[b].min_x)
            .unwrap_or(Ordering::Equal)
    });
    order
}

/// Pair LEFT labels to RIGHT amounts, returning the RIGHT slot each LEFT
/// position claimed.
///
/// Each LEFT row claims the first unassigned RIGHT amount that overlaps it *and
/// that its label may legally hold* (see [`AmountClaim`]). This sequential
/// first-fit keeps the two columns monotonically aligned, which is the strongest
/// signal on receipts (overlap-quality ranking was tried and mis-pairs receipts
/// whose amounts lean a half-row down); the type check is what keeps a sub-line
/// from consuming the slot its item needs.
///
/// With a `grid`, a row standing off the receipt's main price column also yields
/// a contested amount to a following row that stands on it — see
/// [`yields_to_price_column`].
fn pair_columns(
    dets: &[Detection],
    left: &[usize],
    right: &[usize],
    grid: Option<&IndentGrid>,
) -> Vec<Option<usize>> {
    let mut assigned_prices = vec![false; right.len()];
    let mut claims: Vec<Option<usize>> = vec![None; left.len()];

    // Whether the last row that was *allowed* to take an amount actually took
    // one — the context a quantity breakdown needs (see below). Rows typed
    // `Never` are skipped rather than recorded, so a breakdown separated from
    // its item by a savings notice still sees the item's outcome.
    let mut last_eligible_claimed = false;
    let x_order = x_order(dets);
    for (position, &left_index) in left.iter().enumerate() {
        let mut claim = amount_claim(&dets[left_index].text);
        // A breakdown row holds the extended price on some chains and nothing on
        // others. Rather than key that off the merchant, read it off the
        // receipt: if the row this breakdown belongs to already took a price,
        // the extended price is up there and the breakdown must not take the
        // *next* item's — which is exactly how FreshCo produce lines lose theirs.
        let breakdown = is_quantity_breakdown_label(&dets[left_index].text);
        if claim == AmountClaim::Any && last_eligible_claimed && breakdown {
            claim = AmountClaim::Never;
        }
        if claim == AmountClaim::Never {
            continue;
        }
        let mut matched: Option<usize> = None;
        let row = row_reach(dets, left_index, &x_order);
        for (slot, &right_index) in right.iter().enumerate() {
            if assigned_prices[slot] {
                continue;
            }
            let same_row =
                boxes_overlap_y(&dets[left_index], &dets[right_index], PAIR_OVERLAP_GATE)
                    || row.contains(&right_index);
            if !same_row || !claim.accepts(&dets[right_index].text) {
                continue;
            }
            // Second guard for breakdowns, for the chains that print the item
            // name *below* its quantity line (Foody Mart): first-fit hands the
            // amount to whichever row comes first in reading order, so a
            // breakdown that merely brushes the amount outranks the item row
            // that squarely lines up with it. Yield when the next row is the
            // better fit — items are the price carriers, breakdowns only
            // sometimes are.
            if breakdown {
                let mine = line_overlap_ratio(dets, right_index, &[left_index]);
                let next_is_better = left
                    .get(position + 1)
                    .is_some_and(|&next| line_overlap_ratio(dets, right_index, &[next]) > mine);
                if next_is_better {
                    continue;
                }
            }
            if grid
                .is_some_and(|grid| yields_to_price_column(dets, left, right_index, position, grid))
            {
                continue;
            }
            matched = Some(slot);
            break;
        }
        last_eligible_claimed = matched.is_some();
        if let Some(slot) = matched {
            assigned_prices[slot] = true;
            claims[position] = Some(slot);
        }
    }
    claims
}

/// Whether the row at `position` should hand `right_index` to a following row.
///
/// Indentation says a row stands *off* the column its neighbours pair from, and
/// that much is measurable: receipts print on a character grid, and clustering
/// left edges at half a cell recovers it. What indentation does **not** say is
/// what an off-column row means, because the corpus disagrees chain by chain.
/// Food Basics sets savings notices one cell in and they never carry a price;
/// Foody Mart prints the item name *below* its quantity line, so its deepest
/// column carries the amount 65 times out of 69; Costco's `PC` activation stubs
/// and `***TOTAL` sit *shallower* than its items, inverting the ladder
/// altogether; Bestco Fresh indents every item one cell past its department
/// headers, so "deeper than the row above" describes every item on the receipt.
///
/// So an off-column row is never refused outright. Measured over the
/// 123-receipt corpus, vetoing them strips 449 of the 1843 rows that currently
/// claim an amount — a quarter of the receipt's money.
///
/// Being off-column is not on its own a reason to give an amount up either.
/// Requiring only that *some* following row on the price column overlaps the
/// amount cost more than it earned (net −2 on the corpus's items-sum/subtotal
/// count): Loblaw prints beer as `COORS LIGHT…` over a bare UPC row that
/// carries the price, and the UPC row is off-column but squarely on its
/// amount's row, so it handed 13.99 away to a worse-aligned neighbour. The
/// column only breaks the tie — the successor must also be the *better*
/// vertical fit, which is the same bar the quantity-breakdown guard uses.
fn yields_to_price_column(
    dets: &[Detection],
    left: &[usize],
    right_index: usize,
    position: usize,
    grid: &IndentGrid,
) -> bool {
    // Deeper than the price column, not merely off it. An annotation is indented
    // *in* under the item it belongs to; a row starting to the *left* of the item
    // column is a banner spanning the page, and those own their amounts. Costco
    // 2026-03-18 is the case that separates the two: its `xxxBottom of
    // _Basketxxx` marker starts at x=39 against an item column at x=170, and it
    // legitimately holds the 17.99 belonging to the row it has overlapped.
    if grid.level[position] <= grid.modal {
        return false;
    }
    let mine = line_overlap_ratio(dets, right_index, &[left[position]]);
    left.iter()
        .enumerate()
        .skip(position + 1)
        .take(INDENT_YIELD_LOOKAHEAD)
        .any(|(next_position, &next_index)| {
            grid.level[next_position] == grid.modal
                && boxes_overlap_y(&dets[next_index], &dets[right_index], 0.3)
                && line_overlap_ratio(dets, right_index, &[next_index]) > mine
        })
}

/// Group detections into lines using item-first matching. Each returned line is
/// a list of source indices: within a line sorted left-to-right by `min_x`, and
/// lines ordered top-to-bottom by average `center_y`.
pub fn group_detections_into_lines(dets: &[Detection], image_width: f64) -> Vec<Vec<usize>> {
    if dets.is_empty() {
        return Vec::new();
    }

    // Partition into LEFT / MIDDLE / RIGHT, preserving detection order so the
    // subsequent stable center_y sorts match the Python list semantics.
    let mut left: Vec<usize> = Vec::new();
    let mut middle: Vec<usize> = Vec::new();
    let mut right: Vec<usize> = Vec::new();
    let has_self_checkout_header = dets
        .iter()
        .any(|det| is_self_checkout_header_label(&det.text));
    for (index, det) in dets.iter().enumerate() {
        let x_norm = det.min_x / image_width;
        if x_norm > RIGHT_COLUMN_CUT && is_amount_shaped(&det.text) {
            right.push(index);
        } else if belongs_in_left_column(&det.text, x_norm)
            || (has_self_checkout_header
                && x_norm <= RIGHT_COLUMN_CUT
                && is_summary_anchor_label(&det.text))
        {
            left.push(index);
        } else {
            middle.push(index);
        }
    }

    sort_by_center_y(dets, &mut left);
    sort_by_center_y(dets, &mut right);

    // Pair once to find out which rows claim anything, use that to locate the
    // receipt's main price column, then pair again with the yield enabled. The
    // grid cannot be measured before the first pass because the column is
    // *defined* as the one the claims come from.
    let baseline = pair_columns(dets, &left, &right, None);
    let claims = match indent_grid(dets, &left, &baseline) {
        Some(grid) => pair_columns(dets, &left, &right, Some(&grid)),
        None => baseline,
    };

    let mut assigned_prices = vec![false; right.len()];
    let mut lines: Vec<Vec<usize>> = Vec::new();
    for (position, &left_index) in left.iter().enumerate() {
        match claims[position] {
            Some(slot) => {
                lines.push(vec![left_index, right[slot]]);
                assigned_prices[slot] = true;
            }
            None => lines.push(vec![left_index]),
        }
    }

    // Orphan prices stand as their own lines.
    for (slot, &right_index) in right.iter().enumerate() {
        if !assigned_prices[slot] {
            lines.push(vec![right_index]);
        }
    }

    // MIDDLE descriptions attach to the best-aligned existing line.
    let y_threshold = adaptive_middle_y_threshold(dets);
    let overlap_threshold = 0.25;
    for &mid_index in &middle {
        let mut best_line: Option<usize> = None;
        let mut best_score: Option<(u8, f64, f64)> = None;
        for (line_idx, line) in lines.iter().enumerate() {
            let overlap_ratio = line_overlap_ratio(dets, mid_index, line);
            let center_distance = (dets[mid_index].center_y - line_center_y(dets, line)).abs();
            if overlap_ratio < overlap_threshold && center_distance > y_threshold {
                continue;
            }
            if !centers_agree(dets, mid_index, line) {
                continue;
            }
            let score = (
                if overlap_ratio >= overlap_threshold {
                    0
                } else {
                    1
                },
                distance_to_line_span(dets, mid_index, line),
                center_distance,
            );
            if best_score.is_none() || score_less(score, best_score.unwrap()) {
                best_score = Some(score);
                best_line = Some(line_idx);
            }
        }
        match best_line {
            Some(line_idx) => lines[line_idx].push(mid_index),
            None => lines.push(vec![mid_index]),
        }
    }

    // Within-line left-to-right, then lines top-to-bottom (both stable).
    for line in &mut lines {
        line.sort_by(|&a, &b| {
            dets[a]
                .min_x
                .partial_cmp(&dets[b].min_x)
                .unwrap_or(Ordering::Equal)
        });
    }
    lines.sort_by(|a, b| {
        line_center_y(dets, a)
            .partial_cmp(&line_center_y(dets, b))
            .unwrap_or(Ordering::Equal)
    });

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(text: &str, min_x: f64, cy: f64) -> Detection {
        Detection {
            confidence: 0.99,
            text: text.to_string(),
            center_y: cy,
            y_min: cy - 20.0,
            y_max: cy + 20.0,
            min_x,
            bbox: Vec::new(),
        }
    }

    #[test]
    fn left_column_routing() {
        assert!(belongs_in_left_column("anything", 0.1));
        assert!(!belongs_in_left_column("anything", 0.35));
        assert!(belongs_in_left_column("232952 COKE", 0.25)); // digit-led SKU
        assert!(belongs_in_left_column("TOTAL", 0.25)); // summary label
        assert!(!belongs_in_left_column("CRAISINS 1.8", 0.25)); // alpha description
    }

    #[test]
    fn pairs_left_item_with_overlapping_right_price() {
        let dets = vec![
            det("232952 COKE", 120.0, 220.0), // left
            det("17.19", 760.0, 220.0),       // right, same row
            det("305882 IBU", 120.0, 340.0),  // left
            det("16.99", 760.0, 340.0),       // right, same row
        ];
        let lines = group_detections_into_lines(&dets, 1000.0);
        assert_eq!(lines.len(), 2);
        // top row first, item before price
        assert_eq!(lines[0], vec![0, 1]);
        assert_eq!(lines[1], vec![2, 3]);
    }

    #[test]
    fn a_tilted_row_keeps_its_own_price() {
        // Real geometry, no_frills 2026-08-30 (the top two item rows), where the
        // baseline runs 2.7 deg and the page is 1531 wide: `03120044526`
        // overlaps its own `4.50` by 0.277 against the 0.3 gate, while the row
        // below it overlaps that same amount by 0.917. Direct pairing hands 4.50
        // to RH FLOUR and every row under it inherits its neighbour's price;
        // walking the row through COCKTAIL JCE and MRJ does not.
        let dets = vec![
            det_span("03120044526", 83.0, 583.7, 652.7),
            det_span("COCKTAIL JCE", 445.0, 601.1, 669.3),
            det_span("MRJ", 929.0, 628.5, 684.3),
            det_span("4.50", 1100.0, 635.1, 698.5),
            det_span("05900001652", 86.0, 640.3, 706.0),
            det_span("RH FLOUR ALL", 445.0, 654.3, 720.4),
            det_span("MRJ", 931.0, 674.9, 730.7),
            det_span("11.97", 1091.0, 687.7, 744.1),
        ];
        let lines = group_detections_into_lines(&dets, 1531.0);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], vec![0, 1, 2, 3]);
        assert_eq!(lines[1], vec![4, 5, 6, 7]);
    }

    #[test]
    fn a_row_does_not_reach_the_next_rows_price() {
        // The same walk must not cross rows on a receipt that is not tilted: the
        // first row has no amount of its own, and the second row's price is a
        // full row below it.
        let dets = vec![
            det_span("232952 COKE", 120.0, 200.0, 240.0),
            det_span("DEPOSIT INCLUDED", 400.0, 200.0, 240.0),
            det_span("305882 IBU", 120.0, 320.0, 360.0),
            det_span("16.99", 760.0, 320.0, 360.0),
        ];
        let lines = group_detections_into_lines(&dets, 1000.0);
        assert!(lines.iter().any(|line| line == &vec![2, 3]));
        assert!(!lines
            .iter()
            .any(|line| line.contains(&0) && line.contains(&3)));
    }

    fn det_span(text: &str, min_x: f64, y_min: f64, y_max: f64) -> Detection {
        Detection {
            confidence: 0.99,
            text: text.to_string(),
            center_y: (y_min + y_max) / 2.0,
            y_min,
            y_max,
            min_x,
            bbox: Vec::new(),
        }
    }

    /// Like `det_span` but with a real quad, which the glyph-pitch estimate
    /// needs — a detection with no bbox has no measurable width.
    fn det_box(text: &str, x_min: f64, x_max: f64, y_min: f64, y_max: f64) -> Detection {
        Detection {
            confidence: 0.99,
            text: text.to_string(),
            center_y: (y_min + y_max) / 2.0,
            y_min,
            y_max,
            min_x: x_min,
            bbox: vec![
                (x_min, y_min),
                (x_max, y_min),
                (x_max, y_max),
                (x_min, y_max),
            ],
        }
    }

    fn render(dets: &[Detection], lines: &[Vec<usize>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.iter()
                    .map(|&i| dets[i].text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    /// Food Basics 2026-07-31, real de-padded geometry, image width 1392.
    ///
    /// The chain prints `Saving <amount>` one grid cell in from its item column.
    /// The right column leans up ~14px, so `Saving 2.01` overlapped SUBTOTAL's
    /// 6.96 first and took it: the receipt reported a 6.96 line item and a
    /// SUBTOTAL with no amount. Nothing in the label is recognisable — the
    /// vocabulary here is `INSTANT SAVINGS`/`YOU SAVED`, not a singular
    /// "Saving" — so only the column separates them.
    fn food_basics_item_block() -> Vec<Detection> {
        vec![
            det_box("(10)CORN BICOLOR LOC", 210.0, 766.0, 573.0, 647.0),
            det_box("1.98", 1203.0, 1330.0, 610.0, 679.0),
            det_box("10 @ 10/$1.98", 261.0, 627.0, 638.0, 699.0),
            det_box("Saving 4.72", 232.0, 546.0, 692.0, 757.0),
            det_box("PRODUCE", 153.0, 354.0, 755.0, 807.0),
            det_box("4.98", 1198.0, 1325.0, 780.0, 851.0),
            det_box("YELLOW PLUM", 206.0, 519.0, 804.0, 863.0),
            det_box("Saving 2.01", 230.0, 548.0, 858.0, 918.0),
            det_box("6.96", 1086.0, 1311.0, 894.0, 961.0),
            det_box("SUBTOTAL", 152.0, 585.0, 912.0, 970.0),
            det_box("6.96", 1090.0, 1309.0, 954.0, 1010.0),
            det_box("TOTAL", 155.0, 421.0, 968.0, 1026.0),
            det_box("6.96", 1195.0, 1321.0, 1059.0, 1121.0),
            det_box("CREDIT CR", 261.0, 519.0, 1078.0, 1131.0),
        ]
    }

    #[test]
    fn off_column_row_yields_the_summary_amount() {
        let dets = food_basics_item_block();
        let rendered = render(&dets, &group_detections_into_lines(&dets, 1392.0));
        assert!(
            rendered.contains(&"SUBTOTAL 6.96".to_string()),
            "{rendered:?}"
        );
        assert!(
            rendered.contains(&"Saving 2.01".to_string()),
            "the savings row must end up with no amount: {rendered:?}"
        );
        // The rows that were already right must stay right.
        assert!(
            rendered.contains(&"(10)CORN BICOLOR LOC 1.98".to_string()),
            "{rendered:?}"
        );
        assert!(rendered.contains(&"TOTAL 6.96".to_string()), "{rendered:?}");
        assert!(
            rendered.contains(&"CREDIT CR 6.96".to_string()),
            "the deepest column still claims when nothing follows it: {rendered:?}"
        );
    }

    #[test]
    fn a_row_shallower_than_the_price_column_keeps_its_amount() {
        // Costco 2026-03-18_costco_78_54, real geometry, image width 946: the
        // `Bottom of Basket` banner starts at x=39 against an item column at
        // x=170 and squarely overlaps the 17.99 belonging to the row it covers.
        // It is off-column but *shallower*, so it is a banner, not an
        // annotation — yielding here handed COKE ZERO an amount that is not its
        // own and shifted every following row.
        let dets = vec![
            det_box(
                "xxxxxxxxxxxBottom of _Basketxxxxxxxxxxx",
                39.0,
                881.0,
                480.0,
                573.0,
            ),
            det_box("232952 COKE ZERO", 170.0, 524.0, 536.0, 612.0),
            det_box("17.99", 694.0, 813.0, 542.0, 582.0),
            det_box("6.69", 710.0, 810.0, 585.0, 626.0),
            det_box("232893 2% FINE-FILT", 170.0, 540.0, 600.0, 668.0),
            det_box("6.69", 712.0, 813.0, 626.0, 675.0),
        ];
        let rendered = render(&dets, &group_detections_into_lines(&dets, 946.0));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("Bottom of") && line.contains("17.99")),
            "the banner must keep the amount it overlaps: {rendered:?}"
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("COKE ZERO") && line.contains("6.69")),
            "{rendered:?}"
        );
    }

    #[test]
    fn department_headers_never_claim_an_amount() {
        // The shapes the corpus actually produces, OCR noise included.
        for header in [
            "21-GROCERY",
            "22-DAIRY",
            "27-PRODUCE",
            "31-NEATS",
            "41-HONE",
            "26-L.IQUOR",
            "42-ENTERTAINNENT",
            "25-NATURAL FOODS",
            "33-BAKERY INSTORE",
        ] {
            assert_eq!(
                amount_claim(header),
                AmountClaim::Never,
                "{header} is a department header"
            );
        }
        // Rows that merely start with digits and must keep claiming.
        for row in [
            "05600001066 CRUSH ZERO ORANG",
            "6 @ $1.50 MRJ",
            "2045120 TPD TANK TOP",
            "10-15-2026",
            "20$0.10",
        ] {
            assert_eq!(amount_claim(row), AmountClaim::Any, "{row} carries a price");
        }
    }

    #[test]
    fn a_department_header_does_not_claim_a_neighbours_amount() {
        // real_canadian/rcss_20260130, real live-OCR geometry, image width 895.
        // `22-DAIRY` prints at the left margin — shallower than the item column,
        // so the indent yield reads it as a banner and lets it keep whatever it
        // takes — and its box sits 5px from the 9.00 that belongs to the
        // `6 @ $1.50` breakdown above it, while that breakdown's own box is a
        // full row away. Nothing geometric separates them; only the typing does.
        let dets = vec![
            det_box("(6)06780000235", 80.4, 300.0, 523.4, 557.2),
            det_box("2.99", 650.7, 720.0, 520.2, 554.4),
            det_box("6 @ $1.50", 102.6, 300.0, 550.7, 590.6),
            det_box("22-DAIRY", 48.1, 260.0, 580.9, 621.2),
            det_box("9.00", 732.2, 800.0, 578.7, 613.0),
            det_box("06038309397", 78.0, 300.0, 617.7, 646.6),
            det_box("9.52", 649.6, 720.0, 634.2, 672.6),
        ];
        let rendered = render(&dets, &group_detections_into_lines(&dets, 895.0));
        assert!(
            !rendered
                .iter()
                .any(|line| line.contains("22-DAIRY") && line.contains("9.00")),
            "the department header must not claim an amount: {rendered:?}"
        );
        // What the header held back does *not* reach the breakdown here: the SKU
        // row above it wrongly took 2.99 (the amount column on this receipt runs
        // ~26px low, so every row is claiming its neighbour's — see
        // [`PAIR_OVERLAP_GATE`]), which suppresses the breakdown in turn. So 9.00
        // stands as an orphan line rather than becoming a wrong item price, and
        // the rows past the header re-align on their own.
        assert!(
            rendered.contains(&"9.00".to_string()),
            "it should stand alone rather than land on the wrong row: {rendered:?}"
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("06038309397") && line.contains("9.52")),
            "{rendered:?}"
        );
    }

    #[test]
    fn glyph_pitch_recovers_the_print_grid() {
        let dets = food_basics_item_block();
        let pitch = glyph_pitch(&dets).expect("a monospace receipt has a pitch");
        // ~28px at image width 1392 — 2.1% of width, the corpus median.
        assert!(
            (pitch - 28.7).abs() < 1.5,
            "expected ~28.7px per character cell, got {pitch}"
        );
    }

    #[test]
    fn a_ragged_left_column_collapses_to_one_level_and_disables_the_yield() {
        // Costco's left column is genuinely ragged (SKU rows, `PC` stubs and
        // `***TOTAL` at three unrelated x), so single linkage must chain them
        // rather than invent a ladder — one level means every row is on the
        // modal column and nothing can yield.
        let dets = vec![
            det_box("399 DOORDASH2X50", 131.0, 380.0, 844.0, 890.0),
            det_box("PC 339919953764897", 125.0, 400.0, 865.0, 915.0),
            det_box("810 LCBO CARD", 130.0, 360.0, 902.0, 945.0),
            det_box("SUBTOTAL 1", 128.0, 330.0, 958.0, 1000.0),
            det_box("TAXABLE 2", 121.0, 320.0, 992.0, 1030.0),
            det_box("***TOTAL", 134.0, 340.0, 1013.0, 1054.0),
        ];
        let pitch = glyph_pitch(&dets).expect("pitch");
        let left: Vec<usize> = (0..dets.len()).collect();
        let levels = indent_levels(&dets, &left, pitch);
        assert_eq!(
            levels.iter().copied().max(),
            Some(0),
            "left edges within half a cell must chain into one column: {levels:?}"
        );
    }

    #[test]
    fn code_stub_rows_do_not_claim_next_rows_amount() {
        // Costco 2026-07-02_costco_578_44 summary block (de-padded pixel
        // geometry, image width 502): the "PC <code>" gift-card activation
        // stubs interleave the LCBO/SUBTOTAL/TAX/TOTAL rows and overlap the
        // next row's amount at ~0.4. When stubs may claim prices, each one
        // stole the amount of the row below it and every summary line
        // shifted by one.
        let dets = vec![
            det_span("399 DOORDASH2X50", 131.0, 844.0, 890.0),
            det_span("79.99", 380.0, 837.0, 882.0),
            det_span("PC 339919953764897", 25.0, 865.0, 915.0),
            det_span("400.00", 361.0, 897.0, 938.0),
            det_span("810 LCBO CARD", 130.0, 902.0, 945.0),
            det_span("PC 381019522753105", 22.0, 921.0, 974.0),
            det_span("573.31", 366.0, 952.0, 996.0),
            det_span("SUBTOTAL", 117.0, 958.0, 1000.0),
            det_span("5.13", 389.0, 982.0, 1024.0),
            det_span("TAX", 121.0, 992.0, 1024.0),
            det_span("578.44", 363.0, 1011.0, 1053.0),
            det_span("***TOTAL", 60.0, 1013.0, 1054.0),
        ];
        let lines = group_detections_into_lines(&dets, 502.0);
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| {
                line.iter()
                    .map(|&i| dets[i].text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        assert!(
            rendered.contains(&"399 DOORDASH2X50 79.99".to_string()),
            "{rendered:?}"
        );
        assert!(
            rendered.contains(&"810 LCBO CARD 400.00".to_string()),
            "{rendered:?}"
        );
        assert!(
            rendered.contains(&"SUBTOTAL 573.31".to_string()),
            "{rendered:?}"
        );
        assert!(rendered.contains(&"TAX 5.13".to_string()), "{rendered:?}");
        assert!(
            rendered.contains(&"***TOTAL 578.44".to_string()),
            "{rendered:?}"
        );
    }

    #[test]
    fn transaction_id_header_does_not_claim_first_items_price() {
        // Jin Lian (Clover POS) unknown-date_jin_lian_food_39_99: the right
        // column leans up, so the first item's $11.92 overlaps the
        // "Transaction 037972" header row above it (~0.32) before it overlaps
        // its own item row (~0.85). First-fit let the header claim the price
        // and FESHRIMP PASTE was dropped. Real pixel geometry, width 1600.
        let dets = vec![
            det_span("Transaction 037972", 53.0, 720.0, 808.0),
            det_span("$11.92", 1291.0, 783.0, 862.0),
            det_span("FESHRIMP PASTE150g", 174.0, 795.0, 886.0),
        ];
        let lines = group_detections_into_lines(&dets, 1600.0);
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| {
                line.iter()
                    .map(|&i| dets[i].text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        assert!(
            rendered.contains(&"Transaction 037972".to_string()),
            "{rendered:?}"
        );
        assert!(
            rendered.contains(&"FESHRIMP PASTE150g $11.92".to_string()),
            "{rendered:?}"
        );
    }

    #[test]
    fn self_checkout_header_does_not_claim_first_items_tax_coded_price() {
        // Shoppers 2026-03-08, real cached-OCR geometry, padded width 981.
        // The banner overlaps CREST's 9.99 by 18px and appears first, while the
        // tax suffix is joined to the amount in the same OCR detection.
        let dets = vec![
            det_box("SCO CheckOut", 30.0, 283.0, 413.0, 458.0),
            det_box("9.99 5", 720.0, 857.0, 440.0, 487.0),
            det_box("CREST 3DW TTHP", 32.0, 323.0, 453.0, 494.0),
            det_box("2.00", 721.0, 813.0, 480.0, 525.0),
            det_box("2 X CARNABY, SWEET", 32.0, 383.0, 489.0, 532.0),
        ];

        let rendered = render(&dets, &group_detections_into_lines(&dets, 981.0));
        assert!(
            rendered.contains(&"SCO CheckOut".to_string()),
            "{rendered:?}"
        );
        assert!(
            rendered.contains(&"CREST 3DW TTHP 9.99 5".to_string()),
            "{rendered:?}"
        );
        assert!(
            rendered.contains(&"2 X CARNABY, SWEET 2.00".to_string()),
            "{rendered:?}"
        );
    }

    #[test]
    fn centered_summary_label_anchors_a_middle_only_amount_block() {
        // Shoppers 2026-06-30, real de-padded cached-OCR geometry. The 1598px
        // image puts both price columns at x=0.68, below RIGHT_COLUMN_CUT, and
        // SUBTOTAL itself at x=0.35. Every token here therefore routes through
        // MIDDLE unless the exact summary label becomes its own anchor.
        let dets = vec![
            det_box("SCO CheckOut", 8.0, 424.0, 872.0, 936.0),
            det_box("DRIXORAL NASAL", 7.0, 472.0, 929.0, 1004.0),
            det_box("13.29 GP", 664.0, 936.0, 908.0, 987.0),
            det_box("13.29", 1080.0, 1257.0, 898.0, 976.0),
            det_box("SUBTOTAL :", 563.0, 859.0, 977.0, 1053.0),
            det_box("13.29", 1081.0, 1258.0, 959.0, 1041.0),
        ];

        let rendered = render(&dets, &group_detections_into_lines(&dets, 1598.0));
        assert!(
            rendered.contains(&"DRIXORAL NASAL 13.29 GP 13.29".to_string()),
            "{rendered:?}"
        );
        assert!(
            rendered.contains(&"SUBTOTAL : 13.29".to_string()),
            "{rendered:?}"
        );
        assert!(
            !is_summary_anchor_label("TOTAL POINTS EARNED TODAY:"),
            "loyalty prose must not become a structural summary row"
        );
    }

    #[test]
    fn far_right_total_stays_on_the_item_table_header_row() {
        // Home Hardware 2026-08-09 prints `Total` as the rightmost heading of
        // `SKU Qty Price Total`. It starts at x=0.716, beyond the normal amount
        // cut, so treating it as a summary anchor splits the header in two and
        // prevents the spatial parser from recognising any item rows.
        let dets = vec![
            det_box("SKU", 45.0, 125.0, 500.0, 550.0),
            det_box("Qty", 280.0, 350.0, 500.0, 550.0),
            det_box("Price", 460.0, 560.0, 500.0, 550.0),
            det_box("Total", 684.0, 795.0, 500.0, 550.0),
        ];

        let rendered = render(&dets, &group_detections_into_lines(&dets, 955.0));
        assert_eq!(rendered, vec!["SKU Qty Price Total"]);
    }

    /// Render lines as text, the way the summary-block tests above do.
    #[test]
    fn tax_code_column_is_not_a_price_candidate() {
        // costco/2026-03-05_costco_245_87 (image width 846): Costco prints a
        // per-line tax code in a narrow column to the *right* of the price
        // column, and on this scan `HH` sits level with `FO TANK S` while that
        // row's own 19.99 leans a half-row up. Treated as an amount, the bare
        // code satisfied the row's claim, and the 19.99 fell through to
        // `TPD TANK TOP` — taking the next eight rows' amounts with it.
        let dets = vec![
            det_span("3966510 FO TANK S", 149.0, 1050.0, 1106.0),
            det_span("HH", 647.0, 1036.0, 1085.0),
            det_span("2045120 TPD TANK TOP", 151.0, 1087.0, 1141.0),
            det_span("19.99", 552.0, 1071.0, 1111.0),
            det_span("599010 LAVAZZA 1KG", 162.0, 1123.0, 1176.0),
            det_span("5.00-", 560.0, 1106.0, 1146.0),
        ];
        let lines = rendered(&dets, 846.0);
        // The code still lands on the row it was printed on — it just is not
        // that row's money.
        assert!(
            lines.contains(&"3966510 FO TANK S 19.99 HH".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"2045120 TPD TANK TOP 5.00-".to_string()),
            "{lines:?}"
        );
    }

    #[test]
    fn membership_header_does_not_claim_the_first_item_price() {
        // costco/2026-03-10_costco_16_38 (image width 918): the membership
        // header prints directly above the first item, and the price column
        // leans up ~15px against a 48px row pitch — enough for the header to
        // overlap 6.69 and claim it, dropping `2% FINE-FILT` onto the next
        // row's 9.69 and shifting the receipt through to TOTAL.
        let dets = vec![
            det_span("Member 111942685019", 119.0, 694.0, 759.0),
            det_span("435259 2% FINE-FILT", 187.0, 737.0, 803.0),
            det_span("6.69", 696.0, 728.0, 787.0),
            det_span("430 XL EGGS", 246.0, 787.0, 849.0),
            det_span("9.69", 696.0, 774.0, 832.0),
        ];
        let lines = rendered(&dets, 918.0);
        assert!(
            lines.contains(&"435259 2% FINE-FILT 6.69".to_string()),
            "{lines:?}"
        );
        assert!(lines.contains(&"430 XL EGGS 9.69".to_string()), "{lines:?}");
    }

    #[test]
    fn membership_typing_spares_the_rows_that_only_mention_membership() {
        // The digit run is the whole discriminator: Real Canadian's
        // `Member Pricing` is a reduction row that does carry an amount, and
        // FreshCo masks its card number so the tail is not digits either.
        assert!(is_membership_label("Member 111942685019"));
        assert!(is_membership_label("00 Member 111942685019"));
        assert!(!is_membership_label("Member Pricing"));
        assert!(!is_membership_label("MEMBER PRICING"));
        assert!(!is_membership_label("Member number:"));
        assert!(!is_membership_label("MEMBER:"));
        assert!(!is_membership_label("Member card number: **x****062"));
    }

    fn rendered(dets: &[Detection], width: f64) -> Vec<String> {
        group_detections_into_lines(dets, width)
            .iter()
            .map(|line| {
                line.iter()
                    .map(|&i| dets[i].text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    #[test]
    fn savings_rows_do_not_claim_the_item_price() {
        // FreshCo unknown-date_freshco_157_38 (image width 913): the price
        // column leans up by ~15px against a 34px row pitch, so each savings
        // sub-line overlaps the price of the item *below* it. Untyped, the
        // savings rows took those prices and Soft Drink Orange / Sprite Zero
        // were dropped for having none.
        let dets = vec![
            det_span("INSTANT SAVINGS", 103.0, 597.0, 644.0),
            det_span("YOU SAVED $2.00", 116.0, 630.0, 678.0),
            det_span("$17.98 HC", 640.0, 655.0, 701.0),
            det_span("Soft Drink Orange", 103.0, 668.0, 713.0),
            det_span("INSTANT SAVINGS", 102.0, 736.0, 787.0),
            det_span("$53.94 HC", 640.0, 757.0, 805.0),
            det_span("Sprite Zero", 103.0, 778.0, 822.0),
        ];
        let lines = rendered(&dets, 913.0);
        assert!(
            lines.contains(&"Soft Drink Orange $17.98 HC".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"Sprite Zero $53.94 HC".to_string()),
            "{lines:?}"
        );
    }

    #[test]
    fn quantity_breakdown_yields_when_its_item_already_has_a_price() {
        // Same receipt: "1.280 kg @ $1.52 / kg" sits under a Bananas row that
        // already took $1.95, so the $2.99 it overlaps is the next item's.
        let dets = vec![
            det_span("$1.95 C", 655.0, 1067.0, 1118.0),
            det_span("Bananas", 105.0, 1099.0, 1132.0),
            det_span("1.280 kg @ $1.52 / kg", 114.0, 1116.0, 1171.0),
            det_span("$2.99 C", 654.0, 1135.0, 1188.0),
            det_span("Cocomax Coconut Wtr", 105.0, 1156.0, 1205.0),
        ];
        let lines = rendered(&dets, 913.0);
        assert!(lines.contains(&"Bananas $1.95 C".to_string()), "{lines:?}");
        assert!(
            lines.contains(&"Cocomax Coconut Wtr $2.99 C".to_string()),
            "{lines:?}"
        );
    }

    #[test]
    fn quantity_breakdown_keeps_the_price_when_it_is_the_carrier() {
        // The opposite layout, which the same rule must not break: No Frills
        // 2026-04-30_nofrills_34_94 (image width 1723) prints the extended
        // price *on* the breakdown row, and the description row above it has
        // none of its own.
        let dets = vec![
            det_span("$7.99 lmt 2,", 223.0, 964.0, 1032.0),
            det_span("19.08", 1478.0, 1024.0, 1089.0),
            det_span("2 @ $9.54 ea", 225.0, 1031.0, 1094.0),
            det_span("NO NAME EGGS", 226.0, 1094.0, 1153.0),
        ];
        let lines = rendered(&dets, 1723.0);
        assert!(
            lines.contains(&"2 @ $9.54 ea 19.08".to_string()),
            "{lines:?}"
        );
    }

    #[test]
    fn points_row_does_not_claim_a_currency_amount() {
        // FreshCo prints POINTS EARNED between two Eggs Large rows; it takes
        // the points figure, never the second egg carton's price.
        let dets = vec![
            det_span("$9.18 C", 652.0, 1446.0, 1494.0),
            det_span("Eggs Large", 107.0, 1471.0, 1514.0),
            det_span("125 PTS", 572.0, 1485.0, 1530.0),
            det_span("POINTS EARNED", 106.0, 1501.0, 1548.0),
            det_span("$9.18 C", 653.0, 1516.0, 1565.0),
            det_span("Eggs Large", 106.0, 1539.0, 1586.0),
        ];
        let lines = rendered(&dets, 913.0);
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.contains("Eggs Large $9.18 C"))
                .count(),
            2,
            "{lines:?}"
        );
    }

    #[test]
    fn amount_claim_typing() {
        assert!(matches!(
            amount_claim("Natrel 2% Milk 4L"),
            AmountClaim::Any
        ));
        assert!(matches!(
            amount_claim("YOU SAVED $1.00"),
            AmountClaim::Never
        ));
        assert!(matches!(
            amount_claim("INSTANT SAVINGS"),
            AmountClaim::NegativeOnly
        ));
        assert!(matches!(
            amount_claim("POINTS EARNED"),
            AmountClaim::PointsOnly
        ));
        // Summary rows that merely contain the word still take their own
        // (positive) figures.
        assert!(matches!(
            amount_claim("Your Total Savings"),
            AmountClaim::Any
        ));
        assert!(matches!(
            amount_claim("Discounts & Specials"),
            AmountClaim::Any
        ));

        assert!(AmountClaim::NegativeOnly.accepts("-$6.00"));
        assert!(AmountClaim::NegativeOnly.accepts("6.00-"));
        assert!(!AmountClaim::NegativeOnly.accepts("$53.94 HC"));
        assert!(AmountClaim::PointsOnly.accepts("125 PTS"));
        assert!(!AmountClaim::PointsOnly.accepts("$9.18 C"));
        // A dash separator is not an amount.
        assert!(!AmountClaim::NegativeOnly.accepts("--------"));
    }

    #[test]
    fn quantity_breakdown_shapes() {
        assert!(is_quantity_breakdown_label("2 @ 1/ $8.99"));
        assert!(is_quantity_breakdown_label("1.280 kg @ $1.52 / kg"));
        assert!(is_quantity_breakdown_label("6 @ $1.99"));
        // Not a breakdown: a description that happens to contain "@".
        assert!(!is_quantity_breakdown_label("(7125H 800g)@13.99(1/$9.98)"));
        assert!(!is_quantity_breakdown_label("EMAIL@STORE.CA"));
    }

    #[test]
    fn stacked_logo_halves_do_not_merge_into_one_line() {
        // costco/2026-07-22_costco_67_82 (image width 1485): the banner stacks
        // "COSTCO" over "WHOLESALE", each several times the body height, so the
        // two boxes overlap by 46px — 0.33 of the shorter one, comfortably past
        // the 0.25 merge bar — despite being unmistakably separate lines. Merged,
        // they sort by x into "WHOLESALE OSTC": the name reversed, which drops
        // the merchant to Unknown. Neither box's center lies inside the other's
        // span, which is what tells them apart from real same-row text.
        let dets = vec![
            det_span("OSTC", 475.0, 320.0, 528.0),
            det_span("WHOLESALE", 383.0, 482.0, 622.0),
            det_span("Markham #545", 490.0, 623.0, 734.0),
        ];
        let lines = rendered(&dets, 1485.0);
        assert_eq!(
            lines,
            vec!["OSTC", "WHOLESALE", "Markham #545"],
            "{lines:?}"
        );
    }

    #[test]
    fn same_row_text_still_merges_when_boxes_differ_in_height() {
        // The guard above must not split a genuine row: a short price box next to
        // a taller description still has its center inside the other's span.
        let dets = vec![
            det_span("MILK 2%", 105.0, 500.0, 560.0),
            det_span("6.09", 760.0, 512.0, 548.0),
        ];
        let lines = rendered(&dets, 1000.0);
        assert_eq!(lines, vec!["MILK 2% 6.09"], "{lines:?}");
    }

    #[test]
    fn orphan_price_becomes_its_own_line() {
        let dets = vec![det("MILK", 120.0, 220.0), det("4.99", 760.0, 900.0)];
        let lines = group_detections_into_lines(&dets, 1000.0);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn empty_input() {
        assert!(group_detections_into_lines(&[], 1000.0).is_empty());
    }
}
