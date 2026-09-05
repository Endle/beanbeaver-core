//! Post-OCR detection normalization: the passes, and the runner that orders them.
//!
//! Each pass is a pure `(detections) -> instructions` function — kept indices,
//! corrected coordinates, a sort order — mirroring the Python pipeline at the
//! bbox layer. [`normalize_detections`] applies them to a [`DetectionPage`] in
//! the fixed order `filter_low_quality -> filter_bob_markers -> deskew ->
//! sort_reading_order`, which [`NormalizationOptions`] can skip but not
//! rearrange.
//!
//! Everything here works in **de-padded, original-image pixels**.
//! [`crate::ocr_transform`] owns both boundaries: de-padding on the way in, and
//! lowering to the `[0,1]` [`crate::ocr_document::OcrDocument`] on the way out.

use regex::Regex;
use std::cmp::Ordering;
use std::sync::OnceLock;

pub const MIN_CONFIDENCE: f64 = 0.7;
pub const MIN_TEXT_LENGTH: usize = 2;

// Detection-level deskew over same-row item<->price slopes.
pub const DESKEW_MIN_CONFIDENCE: f64 = 0.95;
pub const DESKEW_MIN_ITEM_WIDTH: f64 = 0.08; // x image_width
pub const DESKEW_MIN_PRICE_WIDTH: f64 = 0.03;
pub const DESKEW_MIN_X_DISTANCE: f64 = 0.50;
pub const DESKEW_ITEM_X_MAX_FRAC: f64 = 0.40;
pub const DESKEW_PRICE_X_MIN_FRAC: f64 = 0.60;
pub const DESKEW_Y_WINDOW_PX: f64 = 200.0;
pub const DESKEW_ANGLE_CAP_DEG: f64 = 5.0;
/// Below this the shear is not worth the disturbance it causes.
///
/// Not a "too small to bother" nicety — a floor on risk/reward. The drift this
/// pass exists to remove is on the order of a whole row pitch; at 1.3 deg over a
/// typical item-to-price span it is barely a third of one, far too little to
/// re-seat a misgrouped row but quite enough to jostle borderline ones. Measured:
/// a Costco receipt estimated at 1.34 deg lost a line item when sheared, while
/// the receipts this pass actually rescues sit at 2.5-4 deg.
pub const DESKEW_MIN_ANGLE_DEG: f64 = 1.5;
pub const DESKEW_INLIER_TOL_DEG: f64 = 0.2;
pub const DESKEW_MIN_INLIERS: usize = 5;

/// Fraction of the *item rows that could pair at all* whose slope must agree
/// with the winning angle.
///
/// This is deliberately not a fraction of the raw candidate pairs. Candidates
/// are the item x price cross-product inside [`DESKEW_Y_WINDOW_PX`], and that
/// window spans several rows on a real receipt (~30px pitch vs a 200px window),
/// so only ~1 pair in 10 can ever be same-row. Measured over the 124-fixture
/// corpus, a pair-fraction gate of 0.60 was unreachable on *every* receipt —
/// the deskew pass had never once fired in production. Counting distinct item
/// rows instead makes the denominator the thing we actually care about.
///
/// 0.25 rather than something higher because the band is genuinely narrow: two
/// scans of the *same physical receipt* minutes apart measured 0.316 and 0.273,
/// one deskewing correctly and the other declining and reverting to a shifted
/// summary block. Consensus, row tightening and the runner-up margin were all
/// checked as ways to separate good from bad in the 0.25-0.32 band and none of
/// them does; what keeps the band safe is [`DESKEW_MIN_ANGLE_DEG`], which
/// excludes the sub-row corrections that have nothing to gain.
pub const DESKEW_MIN_ROW_CONSENSUS: f64 = 0.25;

/// The estimated shear must also tighten rows by this fraction, measured over
/// *all* detections, before it is applied.
///
/// The pair estimator alone cannot break one-row aliasing: an angle off by
/// exactly one row pitch re-labels which price belongs to which item and scores
/// nearly as well. Row tightness is an independent signal — aliasing smears
/// rows, a true deskew compacts them — so it corroborates, never searches.
///
/// Calibrated, not derived. With the estimator ungated, 11 corpus receipts
/// cleared every other check. Ranked by this score the outcomes separate:
/// everything at or below 0.018 either regressed (a subtotal/tax swap at 0.000,
/// a date read as 2030 at 0.000, a subtotal lost to 0.00 at 0.012, a dropped
/// T&T line item at 0.018) and everything at or above 0.029 improved or was
/// inert. A shear that makes rows no tighter is one the pair evidence
/// hallucinated.
///
/// Treat this as a fitted threshold, not a law — it rests on ~11 receipts. The
/// safe direction is up: declining only falls back to the un-deskewed geometry
/// that shipped before, while accepting a bad angle actively corrupts rows.
pub const DESKEW_MIN_ROW_TIGHTENING: f64 = 0.025;

// --- Pairing-free fallback estimator -----------------------------------------
//
// The pair estimator above measures the tilt from same-row item<->price slopes.
// That presupposes each price is already recognisable as belonging to its item's
// row — which is exactly the assumption a large skew destroys, and it needs
// [`DESKEW_MIN_INLIERS`] rows to say anything at all. A short receipt simply
// cannot clear that bar: a 4-item No Frills receipt tilted 3.8 deg produced 3
// inliers, declined as `too_few_inliers`, and every price on it was claimed by
// the row below its own.
//
// So when the pair estimator declines, fall back to searching [`row_partition_cost`]
// directly. It needs no pairing at all — it asks only "which shear makes these
// detections fall into the tightest rows?" — and it works on any receipt with
// enough text to form rows.
//
// The reason this is safe to *search*, when the module docs say row cost
// "corroborates, never searches", is the `+ rows * link` fragmentation term: an
// extreme angle that shatters every row into singletons buys one `link` per
// singleton and scores badly. Without that term this would rediscover the
// failure that retired the pixel-level projection-profile deskew.

/// Coarse sweep step. Finer than the 0.2 deg pair-inlier tolerance, so the
/// coarse pass cannot land in a different cluster than the refinement.
const DESKEW_SWEEP_STEP_DEG: f64 = 0.05;
/// Refinement step around the coarse minimum.
const DESKEW_SWEEP_REFINE_STEP_DEG: f64 = 0.01;
/// How far from the winner a sample must be to count as an independent minimum
/// for the margin check. One degree matches the pair estimator's runner-up rule.
const DESKEW_SWEEP_ALIAS_SEPARATION_DEG: f64 = 1.0;

/// Detections needed before the sweep will render an opinion. Row tightness is a
/// population statistic; on a handful of boxes it is noise.
pub const DESKEW_SWEEP_MIN_DETECTIONS: usize = 25;

/// Row tightening the sweep angle must achieve — stricter than
/// [`DESKEW_MIN_ROW_TIGHTENING`], because the sweep is proposing an angle rather
/// than corroborating one.
pub const DESKEW_SWEEP_MIN_ROW_TIGHTENING: f64 = 0.08;

/// How much better the winning angle must be than the best angle at least
/// [`DESKEW_SWEEP_ALIAS_SEPARATION_DEG`] away, as a fraction of the unsheared
/// cost. This is the anti-aliasing guard: a one-row-aliased angle re-labels
/// which price belongs to which item and scores nearly as well, so a shallow
/// winner is not evidence of anything.
pub const DESKEW_SWEEP_MIN_MARGIN: f64 = 0.02;

/// Distinct item rows whose item<->price slope must agree with the sweep angle
/// before the shear is applied.
///
/// **The sweep proposes; the pairs dispose.** Row tightness answers "does this
/// shear make the page tidier?", which is not the same question as "does it put
/// each price on its item's row" — and only the second one matters for grouping.
/// A FreshCo receipt tilted 0.9 deg (measured on four label<->price pairs) had
/// its cost minimised at 1.63 deg; applying that over-sheared by a quarter of a
/// row and handed every label the price below it.
///
/// So the sweep is not trusted to be *right*, only to be a good place to look:
/// the pair evidence still has the final say, but now it merely has to confirm
/// an angle rather than discover one. That is a far weaker demand — three rows
/// instead of [`DESKEW_MIN_INLIERS`] — which is what lets a short receipt clear
/// it. The 4-item No Frills receipt that motivated this has exactly four
/// item<->price pairs, all agreeing within 0.4 deg, and could never have reached
/// five.
pub const DESKEW_SWEEP_MIN_CORROBORATING_ROWS: usize = 3;

/// Agreement band for corroboration. Wider than [`DESKEW_INLIER_TOL_DEG`]
/// because the two estimators measure different things — a whole-page cost
/// minimum and a single row's slope — so demanding they agree to the same
/// tolerance a cluster of pairs agrees among themselves would be spurious
/// precision.
pub const DESKEW_SWEEP_CORROBORATION_TOL_DEG: f64 = 0.5;

/// How far prices must already be from their labels, in median text heights,
/// before the sweep is allowed to shear anything.
///
/// The precondition the other gates all miss: a receipt whose prices already sit
/// beside their items cannot be helped by a shear, only harmed. FreshCo's
/// 2026-06-17 receipt is the case in point — its labels are within a quarter of
/// a row of their prices, so it was never broken, and the 1.63 deg the sweep
/// liked was enough to walk every price up one row.
pub const DESKEW_SWEEP_MIN_MISALIGNMENT: f64 = 0.5;

/// Numeric view of a detection. Field names mirror the Python detection dict.
#[derive(Clone, Debug, Default)]
pub struct Detection {
    pub confidence: f64,
    pub text: String,
    pub center_y: f64,
    pub y_min: f64,
    pub y_max: f64,
    pub min_x: f64,
    pub bbox: Vec<(f64, f64)>,
}

fn price_text_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*[-$]?\d+\.\d{2}[A-Z]?\s*$").unwrap())
}

/// Vertical overlap ratio test between two detections.
pub fn boxes_overlap_y(a: &Detection, b: &Detection, min_overlap_ratio: f64) -> bool {
    let overlap_start = a.y_min.max(b.y_min);
    let overlap_end = a.y_max.min(b.y_max);
    if overlap_start >= overlap_end {
        return false;
    }
    let overlap = overlap_end - overlap_start;
    let smaller_height = (a.y_max - a.y_min).min(b.y_max - b.y_min);
    if smaller_height <= 0.0 {
        return false;
    }
    overlap / smaller_height >= min_overlap_ratio
}

/// True for Costco Bottom-Of-Basket marker rows.
fn is_bob_marker_text(text: &str) -> bool {
    let upper = text.to_uppercase();
    let has_bottom_banner = upper.contains("BOTTOM OF BAS");
    let has_bob_count_marker = upper.contains("BOB COUNT") && has_xstar_run(&upper, 4);
    has_bottom_banner || has_bob_count_marker
}

/// Matches the `[X*]{4,}` clause: a run of `min_len`+ consecutive `X`/`*`.
fn has_xstar_run(text: &str, min_len: usize) -> bool {
    let mut run = 0usize;
    for ch in text.chars() {
        if ch == 'X' || ch == '*' {
            run += 1;
            if run >= min_len {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Drop detections below the confidence floor or with too-short text.
/// Returns the kept indices in original order.
pub fn filter_low_quality(detections: &[Detection]) -> Vec<usize> {
    detections
        .iter()
        .enumerate()
        .filter(|(index, det)| {
            det.text.trim().chars().count() >= MIN_TEXT_LENGTH
                && (det.confidence >= MIN_CONFIDENCE
                    || is_only_description_of_a_priced_row(detections, *index))
        })
        .map(|(index, _)| index)
        .collect()
}

/// The one shape where dropping a sub-floor detection costs an entire item: it
/// is the **description** of a row that also carries an item code and a price.
///
/// [`MIN_CONFIDENCE`] is at its optimum and must not move — swept live over the
/// 130-receipt corpus it gives 933 critical items at 0.70 against 932 at 0.65,
/// 930 at 0.60 and 924 at 0.50, and the 0.65 step is not even a real gain but a
/// churn of three receipts down and two up. So the floor stays, and this carves
/// out the single case it gets wrong.
///
/// What makes that case different is that the *rest of the row survives*. On
/// costco/2026-08-26 a pen stroke crosses the receipt beside `1789729 ZIPLOC M
/// 18.99` and pushes the description to 0.66; the code and the price are read
/// perfectly at 1.00. Losing the description leaves a row of digits, whose alpha
/// ratio is 0, so nothing downstream will accept it as an item — the row is
/// dropped, its $18.99 is stranded as a `PossibleMissedItem`, and the receipt
/// silently comes up one item short. Keeping a slightly-doubted description is a
/// far smaller risk than that: it is at worst a misspelling in a line comment,
/// and the price it lets through is exact.
///
/// Every clause is there to keep this from becoming a general loosening, and the
/// measurement says they work: replayed over all 131 corpus receipts, this
/// rescues **exactly one** detection — the ZIPLOC description it was built for.
/// The nearest miss is a `TP` fragment on costco/2026-03-05, which a description
/// of two letters cannot pass.
fn is_only_description_of_a_priced_row(detections: &[Detection], index: usize) -> bool {
    let candidate = &detections[index];
    // A description, not a smudge: mostly letters, and the engine's own floor
    // still applies underneath (it emits nothing below 0.5).
    if !text_is_wordlike(&candidate.text) {
        return false;
    }
    let row = detections
        .iter()
        .enumerate()
        .filter(|(other, det)| *other != index && boxes_overlap_y(candidate, det, 0.3));
    let (mut has_code, mut has_price, mut has_other_description) = (false, false, false);
    for (_, det) in row {
        let text = det.text.trim();
        // Only confident row-mates count as evidence. Two sub-floor detections
        // vouching for each other is not corroboration.
        if det.confidence < MIN_CONFIDENCE {
            continue;
        }
        if price_text_re().is_match(text) {
            has_price = true;
        } else if text.len() >= 3 && text.chars().all(|c| c.is_ascii_digit()) {
            has_code = true;
        } else if text_is_wordlike(text) {
            has_other_description = true;
        }
    }
    has_code && has_price && !has_other_description
}

/// Mostly letters — the shape of a product name rather than a code, a price, or
/// detector noise.
fn text_is_wordlike(text: &str) -> bool {
    let trimmed = text.trim();
    let letters = trimmed.chars().filter(|c| c.is_alphabetic()).count();
    letters >= 3 && letters * 2 >= trimmed.chars().count()
}

/// Drop Costco BOB markers that overlap real item rows. Returns kept indices.
pub fn filter_bob_markers(detections: &[Detection]) -> Vec<usize> {
    if detections.is_empty() {
        return Vec::new();
    }
    let mut kept: Vec<usize> = Vec::new();
    for (index, det) in detections.iter().enumerate() {
        if !is_bob_marker_text(&det.text) {
            kept.push(index);
            continue;
        }
        let overlaps_non_marker = detections.iter().enumerate().any(|(other_index, other)| {
            other_index != index
                && !is_bob_marker_text(&other.text)
                && boxes_overlap_y(det, other, 0.25)
        });
        if !overlaps_non_marker {
            kept.push(index);
        }
    }
    kept
}

fn bbox_x_extent(bbox: &[(f64, f64)]) -> (f64, f64, f64) {
    let mut x_min = f64::INFINITY;
    let mut x_max = f64::NEG_INFINITY;
    let mut sum = 0.0;
    for &(x, _) in bbox {
        x_min = x_min.min(x);
        x_max = x_max.max(x);
        sum += x;
    }
    (x_min, x_max, sum / bbox.len() as f64)
}

/// One item<->price pairing: its implied tilt angle and which item row it came
/// from. The owning row is what the consensus gate counts, so a single item
/// pairing with six prices cannot vote six times.
#[derive(Clone, Copy)]
struct PairCandidate {
    angle_deg: f64,
    item_index: usize,
}

/// A detection reduced to the only two numbers the deskew needs from it:
/// `(x_center, center_y)`.
type ColumnPoint = (f64, f64);

/// The left-column labels and right-column prices the deskew reasons over.
fn item_price_columns(
    detections: &[Detection],
    image_width: f64,
) -> (Vec<ColumnPoint>, Vec<ColumnPoint>) {
    let item_x_max_cap = image_width * DESKEW_ITEM_X_MAX_FRAC;
    let price_x_min_floor = image_width * DESKEW_PRICE_X_MIN_FRAC;
    let min_item_width = image_width * DESKEW_MIN_ITEM_WIDTH;
    let min_price_width = image_width * DESKEW_MIN_PRICE_WIDTH;

    let mut items: Vec<ColumnPoint> = Vec::new();
    let mut prices: Vec<ColumnPoint> = Vec::new();

    for det in detections {
        if det.confidence < DESKEW_MIN_CONFIDENCE {
            continue;
        }
        if det.bbox.len() < 4 {
            continue;
        }
        let (x_min, x_max, x_center) = bbox_x_extent(&det.bbox);
        let width = x_max - x_min;
        if width <= 0.0 {
            continue;
        }
        let cy = det.center_y;
        let text = det.text.trim();

        if x_max < item_x_max_cap && width >= min_item_width {
            items.push((x_center, cy));
        }
        if x_min > price_x_min_floor && width >= min_price_width && price_text_re().is_match(text) {
            prices.push((x_center, cy));
        }
    }
    (items, prices)
}

/// How far each label sits from the nearest price that could be its own, in
/// units of `scale`, taken as the median over labels.
///
/// This is the question the whole pass exists to answer: *are prices currently
/// landing on their item's row or not?* A receipt whose labels already sit
/// beside their prices has nothing to gain from a shear and everything to lose,
/// because any correction can only push an aligned pair apart. Measured at zero
/// shear, so it describes the input rather than the proposed fix.
///
/// Returns `None` when there is not enough of a left/right column structure to
/// judge — treated as "no evidence of a problem", i.e. decline.
fn misalignment_ratio(detections: &[Detection], image_width: f64, scale: f64) -> Option<f64> {
    if scale <= 0.0 {
        return None;
    }
    let (items, prices) = item_price_columns(detections, image_width);
    let min_x_distance = image_width * DESKEW_MIN_X_DISTANCE;

    let mut gaps: Vec<f64> = Vec::new();
    for &(icx, icy) in &items {
        let nearest = prices
            .iter()
            .filter(|(pcx, _)| pcx - icx >= min_x_distance)
            .map(|(_, pcy)| (pcy - icy).abs())
            .filter(|dy| *dy <= DESKEW_Y_WINDOW_PX)
            .fold(f64::INFINITY, f64::min);
        if nearest.is_finite() {
            gaps.push(nearest);
        }
    }
    if gaps.len() < DESKEW_SWEEP_MIN_CORROBORATING_ROWS {
        return None;
    }
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    Some(gaps[gaps.len() / 2] / scale)
}

/// Cross-product item/price candidates, filtered by column/width/proximity.
///
/// Mispairings are expected to fall out when the angle is chosen rather than
/// being filtered upfront.
fn build_pair_candidates(detections: &[Detection], image_width: f64) -> Vec<PairCandidate> {
    let min_x_distance = image_width * DESKEW_MIN_X_DISTANCE;
    let (items, prices) = item_price_columns(detections, image_width);

    let mut candidates: Vec<PairCandidate> = Vec::new();
    for (item_index, &(icx, icy)) in items.iter().enumerate() {
        for &(pcx, pcy) in &prices {
            let dx = pcx - icx;
            if dx < min_x_distance {
                continue;
            }
            if (pcy - icy).abs() > DESKEW_Y_WINDOW_PX {
                continue;
            }
            candidates.push(PairCandidate {
                angle_deg: (pcy - icy).atan2(dx).to_degrees(),
                item_index,
            });
        }
    }
    candidates
}

/// Distinct item rows represented in a candidate set.
fn distinct_rows(candidates: &[PairCandidate]) -> usize {
    let mut rows: Vec<usize> = candidates.iter().map(|c| c.item_index).collect();
    rows.sort_unstable();
    rows.dedup();
    rows.len()
}

/// Best-supported tilt angle over the candidate pairs.
///
/// Every candidate angle inside the cap is tried as a cluster centre and scored
/// by how many *distinct item rows* fall within [`DESKEW_INLIER_TOL_DEG`] of it;
/// the winner's inlier angles are averaged. Returns
/// `(angle_deg, inlier_pairs, inlier_rows, runner_up_rows)`, where the runner-up
/// is the best cluster more than a degree away — the one-row-aliasing decoy.
///
/// This replaced a 50-iteration seeded RANSAC. On a dense receipt only ~1 pair
/// in 10 is genuinely same-row, so three-sample trials found the true cluster
/// only by luck; on the receipt that prompted this it locked onto a 7-pair
/// cluster at -0.12 deg and missed the real 10-pair cluster at -3.47 deg. The
/// candidate pool is ~100 angles, so scanning it exhaustively is both cheap and
/// deterministic — no seed, no iteration budget, no luck.
fn best_supported_angle(candidates: &[PairCandidate]) -> (f64, usize, usize, usize) {
    if candidates.len() < 3 {
        return (0.0, 0, 0, 0);
    }
    let cluster_at = |trial: f64| -> (f64, usize, usize) {
        let inliers: Vec<PairCandidate> = candidates
            .iter()
            .copied()
            .filter(|c| (c.angle_deg - trial).abs() <= DESKEW_INLIER_TOL_DEG)
            .collect();
        if inliers.is_empty() {
            return (0.0, 0, 0);
        }
        let mean = inliers.iter().map(|c| c.angle_deg).sum::<f64>() / inliers.len() as f64;
        (mean, inliers.len(), distinct_rows(&inliers))
    };

    let mut best = (0.0f64, 0usize, 0usize);
    for candidate in candidates {
        if candidate.angle_deg.abs() > DESKEW_ANGLE_CAP_DEG {
            continue;
        }
        let scored = cluster_at(candidate.angle_deg);
        // Rows first, pairs as the tie-break: two angles explaining the same
        // number of rows are separated by how much evidence backs them.
        if (scored.2, scored.1) > (best.2, best.1) {
            best = scored;
        }
    }

    let mut runner_up_rows = 0usize;
    for candidate in candidates {
        if candidate.angle_deg.abs() > DESKEW_ANGLE_CAP_DEG
            || (candidate.angle_deg - best.0).abs() <= 1.0
        {
            continue;
        }
        runner_up_rows = runner_up_rows.max(cluster_at(candidate.angle_deg).2);
    }

    (best.0, best.1, best.2, runner_up_rows)
}

/// Cost of the row partition induced by shearing all detections by `angle_deg`:
/// total within-row vertical spread, plus one `link` per row.
///
/// Rows are single-linkage clusters of sheared `center_y`. The per-row term is
/// what makes the measure fragmentation-proof: splitting a row into two
/// singletons drops its spread to zero but buys another `link`, so it only pays
/// off when the row was genuinely more than `link` tall — i.e. when it was not
/// one row to begin with. Without that term an extreme angle shatters every row
/// into singletons and scores a perfect zero, which is precisely the failure
/// that retired the pixel-level projection-profile deskew.
///
/// This is the corroborating signal, never the search: it is evaluated at the
/// estimated angle and at zero, and nowhere else.
fn row_partition_cost(
    detections: &[Detection],
    angle_deg: f64,
    image_width: f64,
    link: f64,
) -> f64 {
    let tan_angle = angle_deg.to_radians().tan();
    let x_ref = image_width / 2.0;
    let mut ys: Vec<f64> = detections
        .iter()
        .map(|det| {
            let count = det.bbox.len().max(1) as f64;
            let x_center = det.bbox.iter().map(|&(x, _)| x).sum::<f64>() / count;
            det.center_y - (x_center - x_ref) * tan_angle
        })
        .collect();
    ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));

    let mut total = 0.0;
    let mut rows = 0usize;
    let mut start = 0usize;
    for index in 1..=ys.len() {
        if index == ys.len() || ys[index] - ys[index - 1] > link {
            total += ys[index - 1] - ys[start];
            rows += 1;
            start = index;
        }
    }
    total + rows as f64 * link
}

/// Median text height, the receipt's natural vertical scale.
///
/// Taken from the quad's short side rather than `y_max - y_min`, because the
/// axis-aligned extent of a *tilted* box grows with its width: a 600px-wide
/// footer line at 3.5 deg reads ~37px taller than the glyphs actually are. On
/// the receipt that prompted this change that inflated the row-link distance to
/// 43px against a 30px row pitch — larger than the rows it was meant to
/// separate. The short side is invariant under exactly the rotation being
/// measured.
fn median_text_height(detections: &[Detection]) -> f64 {
    let mut heights: Vec<f64> = detections
        .iter()
        .map(|det| {
            if det.bbox.len() < 4 {
                return det.y_max - det.y_min;
            }
            let side =
                |a: (f64, f64), b: (f64, f64)| ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
            side(det.bbox[0], det.bbox[1]).min(side(det.bbox[1], det.bbox[2]))
        })
        .filter(|height| *height > 0.0)
        .collect();
    if heights.is_empty() {
        return 0.0;
    }
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    heights[heights.len() / 2]
}

/// Outcome of the pairing-free sweep: the cost-minimising angle, its cost, and
/// the best cost found at least [`DESKEW_SWEEP_ALIAS_SEPARATION_DEG`] away.
struct SweepResult {
    angle_deg: f64,
    cost: f64,
    far_cost: f64,
}

/// Shear angle minimising [`row_partition_cost`], found by exhaustive sweep.
///
/// Coarse pass over the whole legal band, then a refinement around the winner.
/// Exhaustive rather than iterative for the same reason [`best_supported_angle`]
/// is: the search space is one bounded dimension, so scanning it is cheap,
/// deterministic, and cannot get stuck in a local minimum.
fn sweep_best_angle(detections: &[Detection], image_width: f64, link: f64) -> Option<SweepResult> {
    if detections.len() < DESKEW_SWEEP_MIN_DETECTIONS || link <= 0.0 {
        return None;
    }

    let steps = (2.0 * DESKEW_ANGLE_CAP_DEG / DESKEW_SWEEP_STEP_DEG).round() as i64;
    let mut samples: Vec<(f64, f64)> = Vec::with_capacity(steps as usize + 1);
    let mut best = (0.0f64, f64::INFINITY);
    for step in 0..=steps {
        let angle = -DESKEW_ANGLE_CAP_DEG + step as f64 * DESKEW_SWEEP_STEP_DEG;
        let cost = row_partition_cost(detections, angle, image_width, link);
        samples.push((angle, cost));
        if cost < best.1 {
            best = (angle, cost);
        }
    }
    if !best.1.is_finite() {
        return None;
    }

    let refine_steps = (2.0 * DESKEW_SWEEP_STEP_DEG / DESKEW_SWEEP_REFINE_STEP_DEG).round() as i64;
    let mut refined = best;
    for step in 0..=refine_steps {
        let angle = best.0 - DESKEW_SWEEP_STEP_DEG + step as f64 * DESKEW_SWEEP_REFINE_STEP_DEG;
        if angle.abs() > DESKEW_ANGLE_CAP_DEG {
            continue;
        }
        let cost = row_partition_cost(detections, angle, image_width, link);
        if cost < refined.1 {
            refined = (angle, cost);
        }
    }

    let far_cost = samples
        .iter()
        .filter(|(angle, _)| (angle - refined.0).abs() >= DESKEW_SWEEP_ALIAS_SEPARATION_DEG)
        .map(|&(_, cost)| cost)
        .fold(f64::INFINITY, f64::min);

    Some(SweepResult {
        angle_deg: refined.0,
        cost: refined.1,
        far_cost,
    })
}

/// New `(center_y, y_min, y_max)` per detection after vertical shear correction.
fn apply_shear(detections: &[Detection], angle_deg: f64, image_width: f64) -> Vec<(f64, f64, f64)> {
    let tan_angle = angle_deg.to_radians().tan();
    let x_ref = image_width / 2.0;
    detections
        .iter()
        .map(|det| {
            let count = det.bbox.len().max(1) as f64;
            let x_center = det.bbox.iter().map(|&(x, _)| x).sum::<f64>() / count;
            let delta = (x_center - x_ref) * tan_angle;
            (det.center_y - delta, det.y_min - delta, det.y_max - delta)
        })
        .collect()
}

/// Which estimator produced the angle in a [`DeskewOutcome`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeskewEstimator {
    /// Same-row item<->price slope consensus. Tried first, and the only
    /// estimator that can fire on a receipt it can measure.
    PairConsensus,
    /// Direct minimisation of [`row_partition_cost`] over the angle band. Used
    /// only where pair consensus declined — see [`DESKEW_SWEEP_MIN_ROW_TIGHTENING`].
    RowSweep,
}

/// Result of the deskew pass. `new_y` is `Some` only when the shear is applied.
///
/// Everything other than `new_y` is a **diagnostic record of why the gate
/// decided what it did**: `ocr_transform` reads `new_y` and nothing else, and
/// the rest exists to be read by a human debugging a gate constant. Most of
/// those fields are asserted by this module's own tests, which is what keeps
/// them honest; `row_tightening`, `sweep_margin` and `sweep_misalignment` are
/// not read by anything at all today, including tests. They are kept, not
/// deleted, because each one is the measured quantity behind a named constant
/// (`DESKEW_MIN_ROW_TIGHTENING`, `DESKEW_SWEEP_ALIAS_SEPARATION_DEG`,
/// `DESKEW_SWEEP_MIN_MISALIGNMENT`) and dropping them would mean recomputing
/// them by hand the next time one of those is swept.
#[allow(
    dead_code,
    reason = "diagnostic fields for gate constants; see the doc comment above"
)]
pub struct DeskewOutcome {
    pub angle_deg: f64,
    pub applied: bool,
    pub gate_reason: Option<&'static str>,
    /// Which estimator `angle_deg` came from. On a decline this names the
    /// estimator whose gate rejected it — the sweep if it was reached at all.
    pub estimator: DeskewEstimator,
    pub candidate_count: usize,
    pub inlier_count: usize,
    /// Distinct item rows agreeing with `angle_deg`, over the item rows that
    /// produced any candidate at all. Not a fraction of candidate *pairs* — see
    /// [`DESKEW_MIN_ROW_CONSENSUS`].
    pub consensus_ratio: f64,
    /// How much `angle_deg` tightens rows relative to no shear, as a fraction
    /// of the unsheared spread. See [`DESKEW_MIN_ROW_TIGHTENING`].
    pub row_tightening: f64,
    /// Sweep only: how much the winning angle beats the best angle at least
    /// [`DESKEW_SWEEP_ALIAS_SEPARATION_DEG`] away, as a fraction of the
    /// unsheared cost. Zero when the sweep was not reached.
    pub sweep_margin: f64,
    /// Sweep only: distinct item rows whose own slope agrees with the swept
    /// angle. See [`DESKEW_SWEEP_MIN_CORROBORATING_ROWS`].
    pub sweep_corroborating_rows: usize,
    /// Sweep only: median label-to-nearest-price gap in median text heights,
    /// measured at zero shear. See [`DESKEW_SWEEP_MIN_MISALIGNMENT`].
    pub sweep_misalignment: f64,
    pub new_y: Option<Vec<(f64, f64, f64)>>,
}

/// Vertical shear correction, from same-row item<->price slopes where the
/// receipt supports that measurement and from a direct row-tightness sweep
/// where it does not.
///
/// Bias is "miss safely": a wrong correction can push borderline rows out of
/// the matcher's y-band, so each estimator only fires when the angle is in band,
/// large enough to matter, and corroborated — pair consensus by enough agreeing
/// item rows and a beaten runner-up, the sweep by a decisive cost minimum.
///
/// The sweep is strictly a fallback: it is consulted only where pair consensus
/// declined, so every receipt the pair estimator already handled keeps exactly
/// the geometry it had. The blast radius of the fallback is the set of receipts
/// that were previously left un-deskewed.
pub fn deskew(detections: &[Detection], image_width: f64) -> DeskewOutcome {
    let candidates = build_pair_candidates(detections, image_width);
    let candidate_count = candidates.len();
    let (angle, inliers, inlier_rows, runner_up_rows) = best_supported_angle(&candidates);
    let pairable_rows = distinct_rows(&candidates);
    let consensus_ratio = if pairable_rows > 0 {
        inlier_rows as f64 / pairable_rows as f64
    } else {
        0.0
    };

    let link = median_text_height(detections) * 0.5;
    let cost_unsheared = if link > 0.0 {
        row_partition_cost(detections, 0.0, image_width, link)
    } else {
        0.0
    };
    let row_tightening = if cost_unsheared > 0.0 {
        let sheared = row_partition_cost(detections, angle, image_width, link);
        ((cost_unsheared - sheared) / cost_unsheared).max(0.0)
    } else {
        0.0
    };

    let gate_reason = if candidate_count == 0 {
        Some("no_candidates")
    } else if inliers < DESKEW_MIN_INLIERS {
        Some("too_few_inliers")
    } else if angle.abs() > DESKEW_ANGLE_CAP_DEG {
        Some("angle_too_large")
    } else if consensus_ratio < DESKEW_MIN_ROW_CONSENSUS {
        Some("weak_consensus")
    } else if inlier_rows <= runner_up_rows {
        // A one-row-aliased angle explains a different but equally large set of
        // rows. Ties are not evidence; decline rather than guess.
        Some("ambiguous_angle")
    } else if angle.abs() < DESKEW_MIN_ANGLE_DEG {
        Some("angle_too_small")
    } else if row_tightening < DESKEW_MIN_ROW_TIGHTENING {
        Some("rows_not_tightened")
    } else {
        None
    };

    if let Some(reason) = gate_reason {
        return sweep_fallback(
            detections,
            image_width,
            link,
            cost_unsheared,
            &candidates,
            DeskewOutcome {
                angle_deg: angle,
                applied: false,
                gate_reason: Some(reason),
                estimator: DeskewEstimator::PairConsensus,
                candidate_count,
                inlier_count: inliers,
                consensus_ratio,
                row_tightening,
                sweep_margin: 0.0,
                sweep_corroborating_rows: 0,
                sweep_misalignment: 0.0,
                new_y: None,
            },
        );
    }

    let new_y = apply_shear(detections, angle, image_width);
    DeskewOutcome {
        angle_deg: angle,
        applied: true,
        gate_reason: None,
        estimator: DeskewEstimator::PairConsensus,
        candidate_count,
        inlier_count: inliers,
        consensus_ratio,
        row_tightening,
        sweep_margin: 0.0,
        sweep_corroborating_rows: 0,
        sweep_misalignment: 0.0,
        new_y: Some(new_y),
    }
}

/// Second opinion for receipts the pair estimator could not measure.
///
/// `declined` is the outcome pair consensus produced; it is returned unchanged
/// if the sweep has nothing better to offer, so a decline here is never worse
/// than a decline before this fallback existed.
fn sweep_fallback(
    detections: &[Detection],
    image_width: f64,
    link: f64,
    cost_unsheared: f64,
    candidates: &[PairCandidate],
    declined: DeskewOutcome,
) -> DeskewOutcome {
    if cost_unsheared <= 0.0 {
        return declined;
    }
    let Some(sweep) = sweep_best_angle(detections, image_width, link) else {
        return declined;
    };

    let tightening = ((cost_unsheared - sweep.cost) / cost_unsheared).max(0.0);
    let margin = if sweep.far_cost.is_finite() {
        ((sweep.far_cost - sweep.cost) / cost_unsheared).max(0.0)
    } else {
        // No sample far enough away to compare against: the band is too narrow
        // for the alias check to mean anything, so treat it as unproven.
        0.0
    };
    let corroborating: Vec<PairCandidate> = candidates
        .iter()
        .copied()
        .filter(|c| (c.angle_deg - sweep.angle_deg).abs() <= DESKEW_SWEEP_CORROBORATION_TOL_DEG)
        .collect();
    let corroborating_rows = distinct_rows(&corroborating);
    let misalignment = misalignment_ratio(detections, image_width, link * 2.0).unwrap_or(0.0);

    let reason = if sweep.angle_deg.abs() < DESKEW_MIN_ANGLE_DEG {
        Some("sweep_angle_too_small")
    } else if tightening < DESKEW_SWEEP_MIN_ROW_TIGHTENING {
        Some("sweep_rows_not_tightened")
    } else if margin < DESKEW_SWEEP_MIN_MARGIN {
        Some("sweep_ambiguous_angle")
    } else if corroborating_rows < DESKEW_SWEEP_MIN_CORROBORATING_ROWS {
        Some("sweep_uncorroborated")
    } else if misalignment < DESKEW_SWEEP_MIN_MISALIGNMENT {
        Some("sweep_rows_already_aligned")
    } else {
        None
    };

    if let Some(reason) = reason {
        return DeskewOutcome {
            angle_deg: sweep.angle_deg,
            gate_reason: Some(reason),
            estimator: DeskewEstimator::RowSweep,
            row_tightening: tightening,
            sweep_margin: margin,
            sweep_corroborating_rows: corroborating_rows,
            sweep_misalignment: misalignment,
            ..declined
        };
    }

    DeskewOutcome {
        angle_deg: sweep.angle_deg,
        applied: true,
        gate_reason: None,
        estimator: DeskewEstimator::RowSweep,
        row_tightening: tightening,
        sweep_margin: margin,
        sweep_corroborating_rows: corroborating_rows,
        sweep_misalignment: misalignment,
        new_y: Some(apply_shear(detections, sweep.angle_deg, image_width)),
        ..declined
    }
}

/// Stable sort by (center_y, min_x) for top-to-bottom, left-to-right reading
/// order. Returns the source indices in sorted order.
pub fn sort_reading_order(detections: &[Detection]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..detections.len()).collect();
    order.sort_by(|&a, &b| {
        detections[a]
            .center_y
            .partial_cmp(&detections[b].center_y)
            .unwrap_or(Ordering::Equal)
            .then(
                detections[a]
                    .min_x
                    .partial_cmp(&detections[b].min_x)
                    .unwrap_or(Ordering::Equal),
            )
    });
    order
}

/// Detections in **de-padded, original-image pixels**, carried together with the
/// image they were measured against.
///
/// The dimensions and the boxes used to travel as separate arguments, which is
/// what let a pass be handed one image's geometry and another's `image_width`.
/// Bundling them is the point: a pass takes the page, so the two cannot drift.
#[derive(Clone, Debug)]
pub(crate) struct DetectionPage {
    pub detections: Vec<Detection>,
    pub image_width: f64,
    pub image_height: f64,
}

/// Which normalization passes [`normalize_detections`] runs.
///
/// **Order is fixed and not configurable.** The passes depend on each other —
/// deskew measures slopes over whatever survived filtering, and reading order is
/// what line grouping assumes — so a reordered pipeline would be a second
/// pipeline, not a profile of this one. Options can only skip a pass.
///
/// [`SHIPPING`](Self::SHIPPING) is the one profile the apps run. Anything else
/// is a diagnostic: build it with struct-update syntax so a new pass added later
/// is enabled by default rather than silently omitted from an old literal.
///
/// ```ignore
/// NormalizationOptions { deskew: false, ..NormalizationOptions::SHIPPING }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NormalizationOptions {
    pub filter_low_quality: bool,
    pub filter_bob_markers: bool,
    pub deskew: bool,
    pub sort_reading_order: bool,
}

impl NormalizationOptions {
    /// The profile the apps run, and the only one reachable through
    /// [`crate::ocr_transform::transform`].
    pub const SHIPPING: Self = Self {
        filter_low_quality: true,
        filter_bob_markers: true,
        deskew: true,
        sort_reading_order: true,
    };
}

impl Default for NormalizationOptions {
    fn default() -> Self {
        Self::SHIPPING
    }
}

/// Reorder/subset `detections` by source index. `keep` is a permutation for the
/// ordering pass and a subsequence for the filters; both are the same operation.
fn take_indices(detections: &[Detection], keep: &[usize]) -> Vec<Detection> {
    keep.iter().map(|&i| detections[i].clone()).collect()
}

/// Run the detection-preserving passes over `page`, in the fixed production
/// order: filter_low_quality -> filter_bob_markers -> deskew ->
/// sort_reading_order.
///
/// Mirrors the Python `normalize_detections` with `default_detection_pipeline()`
/// (debug-dump I/O omitted — irrelevant on device).
pub(crate) fn normalize_detections(page: &mut DetectionPage, options: NormalizationOptions) {
    if options.filter_low_quality {
        let keep = filter_low_quality(&page.detections);
        page.detections = take_indices(&page.detections, &keep);
    }

    if options.filter_bob_markers {
        let keep = filter_bob_markers(&page.detections);
        page.detections = take_indices(&page.detections, &keep);
    }

    if options.deskew {
        let outcome = deskew(&page.detections, page.image_width);
        if let Some(new_y) = outcome.new_y {
            for (det, (center_y, y_min, y_max)) in page.detections.iter_mut().zip(new_y) {
                // The corner points have to move with the summary fields. Line
                // grouping reads y_min/y_max/center_y, but the document's word
                // boxes are built straight off `bbox`, so leaving the
                // corners behind hands the two paths different geometry for the
                // same detection. Re-derive the shift from center_y rather than
                // recomputing the shear, so the two can never drift apart.
                let delta = det.center_y - center_y;
                for point in &mut det.bbox {
                    point.1 -= delta;
                }
                det.center_y = center_y;
                det.y_min = y_min;
                det.y_max = y_max;
            }
        }
    }

    if options.sort_reading_order {
        let order = sort_reading_order(&page.detections);
        page.detections = take_indices(&page.detections, &order);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(text: &str, cx: f64, cy: f64, width: f64) -> Detection {
        let x_min = cx - width / 2.0;
        let x_max = cx + width / 2.0;
        let half_h = 15.0;
        Detection {
            confidence: 0.99,
            text: text.to_string(),
            center_y: cy,
            y_min: cy - half_h,
            y_max: cy + half_h,
            min_x: x_min,
            bbox: vec![
                (x_min, cy - half_h),
                (x_max, cy - half_h),
                (x_max, cy + half_h),
                (x_min, cy + half_h),
            ],
        }
    }

    fn straight_rows(n: usize) -> Vec<Detection> {
        let mut rows = Vec::new();
        for i in 0..n {
            let cy = 400.0 + i as f64 * 250.0;
            rows.push(det(&format!("ITEM {i}"), 200.0, cy, 200.0));
            rows.push(det(
                &format!("{:.2}", (i + 1) as f64 * 1.99),
                850.0,
                cy,
                80.0,
            ));
        }
        rows
    }

    fn tilt(d: &Detection, angle_deg: f64, image_width: f64) -> Detection {
        let tan_a = angle_deg.to_radians().tan();
        let x_ref = image_width / 2.0;
        let x_center = d.bbox.iter().map(|&(x, _)| x).sum::<f64>() / d.bbox.len() as f64;
        let delta = (x_center - x_ref) * tan_a;
        let mut new = d.clone();
        new.bbox = d.bbox.iter().map(|&(x, y)| (x, y + delta)).collect();
        new.center_y = d.center_y + delta;
        new.y_min = d.y_min + delta;
        new.y_max = d.y_max + delta;
        new
    }

    #[test]
    fn filter_low_quality_drops_low_confidence_and_short_text() {
        let mut dets = straight_rows(1);
        dets.push(det("LOW", 200.0, 9000.0, 200.0));
        dets.last_mut().unwrap().confidence = 0.5;
        dets.push(det("x", 200.0, 9500.0, 200.0)); // single char -> dropped
        let kept = filter_low_quality(&dets);
        assert_eq!(kept, vec![0, 1]);
    }

    #[test]
    fn a_doubted_description_survives_when_its_row_has_a_code_and_a_price() {
        // costco/2026-08-26: a pen stroke pushes `ZIploc m` to 0.66 while the
        // code and price beside it read at 1.00. Dropping it leaves a row of
        // digits and strands the 18.99.
        let mut dets = vec![
            det("1789729", 250.0, 1270.0, 130.0),
            det("ZIploc m", 380.0, 1274.0, 170.0),
            det("18.99", 660.0, 1278.0, 90.0),
        ];
        dets[1].confidence = 0.66;
        assert_eq!(filter_low_quality(&dets), vec![0, 1, 2]);
    }

    #[test]
    fn a_doubted_description_is_still_dropped_when_the_row_already_has_one() {
        // The rescue exists to stop a price being stranded. A row that can name
        // itself without this detection is not at risk, so the floor applies.
        let mut dets = vec![
            det("1789729", 250.0, 1270.0, 130.0),
            det("ZIPLOC BAGS", 380.0, 1272.0, 170.0),
            det("smudge", 520.0, 1274.0, 90.0),
            det("18.99", 660.0, 1278.0, 90.0),
        ];
        dets[2].confidence = 0.66;
        assert_eq!(filter_low_quality(&dets), vec![0, 1, 3]);
    }

    #[test]
    fn a_doubted_description_is_still_dropped_without_a_price_to_strand() {
        let mut dets = vec![
            det("1789729", 250.0, 1270.0, 130.0),
            det("ZIploc m", 380.0, 1274.0, 170.0),
        ];
        dets[1].confidence = 0.66;
        assert_eq!(filter_low_quality(&dets), vec![0]);
    }

    #[test]
    fn a_doubted_row_mate_cannot_vouch_for_a_doubted_description() {
        // Two sub-floor detections corroborating each other is not evidence.
        let mut dets = vec![
            det("1789729", 250.0, 1270.0, 130.0),
            det("ZIploc m", 380.0, 1274.0, 170.0),
            det("18.99", 660.0, 1278.0, 90.0),
        ];
        dets[1].confidence = 0.66;
        dets[2].confidence = 0.60;
        assert_eq!(filter_low_quality(&dets), vec![0]);
    }

    #[test]
    fn deskew_no_candidates_when_empty() {
        let outcome = deskew(&[], 1000.0);
        assert!(!outcome.applied);
        assert_eq!(outcome.gate_reason, Some("no_candidates"));
        assert_eq!(outcome.candidate_count, 0);
    }

    #[test]
    fn deskew_no_op_on_straight_receipt() {
        let outcome = deskew(&straight_rows(8), 1000.0);
        assert!(!outcome.applied);
        assert_eq!(outcome.gate_reason, Some("angle_too_small"));
        assert!(outcome.angle_deg.abs() < DESKEW_MIN_ANGLE_DEG);
    }

    #[test]
    fn deskew_recovers_known_tilt() {
        let true_angle = 1.5;
        let straight = straight_rows(8);
        let tilted: Vec<Detection> = straight
            .iter()
            .map(|d| tilt(d, true_angle, 1000.0))
            .collect();
        let outcome = deskew(&tilted, 1000.0);
        assert!(outcome.applied);
        assert!(outcome.inlier_count >= 5);
        assert!(outcome.consensus_ratio >= 0.60);
        assert!((outcome.angle_deg - true_angle).abs() < 0.05);
        let new_y = outcome.new_y.unwrap();
        for i in 0..8 {
            assert!((new_y[2 * i].0 - new_y[2 * i + 1].0).abs() < 1.0);
        }
    }

    #[test]
    fn deskew_rejects_large_angle() {
        let huge = DESKEW_ANGLE_CAP_DEG + 2.0;
        let tilted: Vec<Detection> = straight_rows(8)
            .iter()
            .map(|d| tilt(d, huge, 1000.0))
            .collect();
        let outcome = deskew(&tilted, 1000.0);
        assert!(!outcome.applied);
    }

    #[test]
    fn deskew_requires_price_text_shape() {
        let straight = straight_rows(8);
        let mut tilted: Vec<Detection> = straight.iter().map(|d| tilt(d, 1.5, 1000.0)).collect();
        for i in (1..tilted.len()).step_by(2) {
            tilted[i].text = "TAX".to_string();
        }
        let outcome = deskew(&tilted, 1000.0);
        assert_eq!(outcome.gate_reason, Some("no_candidates"));
    }

    /// A dense receipt: many rows inside `DESKEW_Y_WINDOW_PX`, so most
    /// item x price pairs are cross-row noise. This is the regime the pass used
    /// to be blind in — every real receipt looks like this, and the pair-count
    /// consensus gate was mathematically unreachable on all of them.
    fn dense_rows(n: usize, pitch: f64) -> Vec<Detection> {
        let mut rows = Vec::new();
        for i in 0..n {
            let cy = 400.0 + i as f64 * pitch;
            rows.push(det(&format!("0512300{i:04}"), 200.0, cy, 200.0));
            rows.push(det(
                &format!("{:.2}", (i + 1) as f64 * 1.99),
                850.0,
                cy,
                80.0,
            ));
        }
        rows
    }

    #[test]
    fn deskew_fires_on_a_dense_receipt_where_pairs_are_mostly_cross_row() {
        let true_angle = -2.5;
        let tilted: Vec<Detection> = dense_rows(14, 30.0)
            .iter()
            .map(|d| tilt(d, true_angle, 1000.0))
            .collect();
        let outcome = deskew(&tilted, 1000.0);

        // Most pairs are cross-row: a 200px window over a 30px pitch admits ~6
        // rows either side, so the pair fraction stays far below the 0.60 the
        // old gate demanded.
        let pair_fraction = outcome.inlier_count as f64 / outcome.candidate_count as f64;
        assert!(
            pair_fraction < 0.60,
            "pair fraction {pair_fraction} — fixture is not dense enough to be the regression it guards"
        );
        assert!(outcome.applied, "gate said {:?}", outcome.gate_reason);
        assert!((outcome.angle_deg - true_angle).abs() < 0.05);
    }

    /// A receipt too short for the pair estimator: only `items` item rows, so at
    /// most `items` same-row pairs — below `DESKEW_MIN_INLIERS`. Padded with
    /// left-column-only text (headers, footers) so the sweep has a population to
    /// measure row tightness over, exactly like a real receipt's preamble.
    fn short_receipt(items: usize, pitch: f64) -> Vec<Detection> {
        // Deliberately irregular: section gaps and ragged x positions. A
        // perfectly periodic page aliases exactly one row off and the margin
        // gate rightly refuses to choose, which is a property of graph paper
        // rather than of receipts.
        let mut rows = Vec::new();
        let mut cy = 200.0;
        for i in 0..6 {
            rows.push(det(
                &format!("HEADER LINE {i}"),
                180.0 + (i % 3) as f64 * 40.0,
                cy,
                240.0 + (i % 2) as f64 * 90.0,
            ));
            if i % 2 == 0 {
                rows.push(det(&format!("H{i}"), 800.0, cy, 70.0));
            }
            cy += pitch;
        }
        cy += pitch * 0.6;
        for i in 0..items {
            rows.push(det(&format!("0512300{i:04}"), 200.0, cy, 200.0));
            rows.push(det(
                &format!("{:.2}", (i + 1) as f64 * 3.11),
                850.0,
                cy,
                80.0,
            ));
            cy += pitch;
            // The quantity sub-line each of these receipts carries.
            rows.push(det(
                &format!("2 @ ${:.2}", (i + 1) as f64 * 1.55),
                230.0,
                cy,
                150.0,
            ));
            cy += pitch * 1.4;
        }
        cy += pitch * 0.7;
        for i in 0..20 {
            rows.push(det(
                &format!("FOOTER TEXT {i}"),
                220.0 + (i % 4) as f64 * 35.0,
                cy,
                300.0 + (i % 3) as f64 * 70.0,
            ));
            // Real footers carry a right-hand column too (tender amounts, card
            // digits, points). Without it almost every row is a single token and
            // no shear can measurably tighten the page.
            rows.push(det(&format!("F{i:02}"), 810.0, cy, 75.0));
            cy += pitch * if i % 5 == 0 { 1.3 } else { 1.0 };
        }
        rows
    }

    #[test]
    fn sweep_recovers_a_tilt_the_pair_estimator_is_too_short_to_find() {
        // The No Frills case: four item rows at 3.8 deg. Pair consensus needs
        // five and declines; the sweep measures the whole page instead.
        let true_angle = 3.8;
        let tilted: Vec<Detection> = short_receipt(4, 32.0)
            .iter()
            .map(|d| tilt(d, true_angle, 950.0))
            .collect();
        let outcome = deskew(&tilted, 950.0);

        assert!(
            outcome.inlier_count < DESKEW_MIN_INLIERS,
            "fixture must starve the pair estimator, got {} inliers",
            outcome.inlier_count
        );
        assert!(outcome.applied, "gate said {:?}", outcome.gate_reason);
        assert_eq!(outcome.estimator, DeskewEstimator::RowSweep);
        assert!(
            (outcome.angle_deg - true_angle).abs() < 0.2,
            "recovered {:.3} for a true {true_angle}",
            outcome.angle_deg
        );
    }

    #[test]
    fn sweep_declines_a_receipt_whose_prices_already_sit_on_their_rows() {
        // The FreshCo regression: an untilted short receipt has nothing to gain,
        // so whatever angle minimises row cost must not be applied.
        let straight = short_receipt(4, 32.0);
        let outcome = deskew(&straight, 950.0);
        assert!(
            !outcome.applied,
            "sheared an aligned receipt by {:.3}",
            outcome.angle_deg
        );
    }

    #[test]
    fn sweep_declines_when_no_item_row_agrees_with_it() {
        // Corroboration is what stops the sweep acting on its own opinion. Strip
        // the right column and the same tilted page has nothing to confirm it.
        let true_angle = 3.8;
        let tilted: Vec<Detection> = short_receipt(4, 32.0)
            .iter()
            .filter(|d| !d.text.contains('.'))
            .map(|d| tilt(d, true_angle, 950.0))
            .collect();
        let outcome = deskew(&tilted, 950.0);
        assert!(
            !outcome.applied,
            "fired with {} corroborating rows",
            outcome.sweep_corroborating_rows
        );
    }

    #[test]
    fn pair_consensus_still_wins_where_it_can_measure() {
        // The sweep is a fallback only: a receipt the pair estimator can handle
        // must keep exactly the angle and provenance it had before.
        let true_angle = -2.5;
        let tilted: Vec<Detection> = dense_rows(14, 30.0)
            .iter()
            .map(|d| tilt(d, true_angle, 1000.0))
            .collect();
        let outcome = deskew(&tilted, 1000.0);
        assert!(outcome.applied);
        assert_eq!(outcome.estimator, DeskewEstimator::PairConsensus);
    }

    #[test]
    fn deskew_consensus_counts_rows_not_pair_votes() {
        // One item row paired against many prices must not out-vote the rest of
        // the receipt just by appearing in more pairs.
        let candidates: Vec<PairCandidate> = (0..9)
            .map(|_| PairCandidate {
                angle_deg: 2.0,
                item_index: 0,
            })
            .chain((1..4).map(|row| PairCandidate {
                angle_deg: -1.0,
                item_index: row,
            }))
            .collect();
        let (angle, pairs, rows, _) = best_supported_angle(&candidates);
        assert_eq!(rows, 3, "three distinct rows agree on -1.0");
        assert_eq!(pairs, 3);
        assert!((angle + 1.0).abs() < 1e-9);
    }

    #[test]
    fn deskew_declines_when_the_aliased_runner_up_ties() {
        // Two angles a full row apart, each explaining the same number of rows:
        // that is one-row aliasing, not evidence.
        let candidates: Vec<PairCandidate> = (0..5)
            .map(|row| PairCandidate {
                angle_deg: 2.0,
                item_index: row,
            })
            .chain((5..10).map(|row| PairCandidate {
                angle_deg: -2.0,
                item_index: row,
            }))
            .collect();
        let (_, _, rows, runner_up) = best_supported_angle(&candidates);
        assert_eq!(rows, runner_up, "the decoy must tie the winner");
    }

    #[test]
    fn row_partition_cost_penalizes_shattering_rows() {
        // The guard that keeps this from becoming the projection-profile trap:
        // an absurd angle scatters every detection into its own row, which must
        // cost more than the honest partition, not less.
        let dets = dense_rows(10, 30.0);
        let link = median_text_height(&dets) * 0.5;
        let honest = row_partition_cost(&dets, 0.0, 1000.0, link);
        let shattered = row_partition_cost(&dets, 45.0, 1000.0, link);
        assert!(
            shattered > honest,
            "shattered {shattered} should cost more than honest {honest}"
        );
    }

    #[test]
    fn median_text_height_is_not_inflated_by_tilt() {
        // y_max - y_min grows with width once a box is rotated; the quad's short
        // side does not. A wide line must not drag the row-link scale up.
        let mut wide = det("A VERY WIDE FOOTER LINE", 500.0, 1000.0, 600.0);
        let half_h = (wide.y_max - wide.y_min) / 2.0;
        let angle: f64 = 4.0_f64.to_radians();
        let (sin, cos) = angle.sin_cos();
        wide.bbox = wide
            .bbox
            .iter()
            .map(|&(x, y)| {
                let (dx, dy) = (x - 500.0, y - 1000.0);
                (500.0 + dx * cos - dy * sin, 1000.0 + dx * sin + dy * cos)
            })
            .collect();
        wide.y_min = wide.bbox.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
        wide.y_max = wide
            .bbox
            .iter()
            .map(|p| p.1)
            .fold(f64::NEG_INFINITY, f64::max);

        let axis_aligned_extent = wide.y_max - wide.y_min;
        let measured = median_text_height(std::slice::from_ref(&wide));
        assert!(
            axis_aligned_extent > 2.0 * half_h * 1.5,
            "fixture should be badly inflated, got {axis_aligned_extent}"
        );
        assert!(
            (measured - 2.0 * half_h).abs() < 1.0,
            "expected the true glyph height {}, got {measured}",
            2.0 * half_h
        );
    }

    #[test]
    fn bob_marker_detection() {
        assert!(is_bob_marker_text("***Bottom of Basket"));
        assert!(is_bob_marker_text("*xBOB Count XXXX"));
        assert!(!is_bob_marker_text("BOB Count 3"));
        assert!(!is_bob_marker_text("MILK"));
    }

    #[test]
    fn sort_reading_order_is_stable_top_to_bottom() {
        let dets = vec![
            det("B", 200.0, 100.0, 50.0),
            det("A", 100.0, 100.0, 50.0),
            det("C", 150.0, 50.0, 50.0),
        ];
        // center_y 50 first (index 2), then row at 100 ordered by min_x: A before B
        assert_eq!(sort_reading_order(&dets), vec![2, 1, 0]);
    }
}
