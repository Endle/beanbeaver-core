//! Post-OCR detection normalization logic.
//!
//! Pure `(detections) -> instructions` passes mirroring the Python pipeline at
//! the bbox layer. The Python wrapper owns orchestration, dict marshaling, and
//! the optional debug-dump filesystem I/O; everything numeric lives here.
//!
//! Default ordering: filter_low_quality -> filter_bob_markers ->
//! deskew_detections -> sort_reading_order.

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
pub const DESKEW_MIN_ANGLE_DEG: f64 = 0.3;
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
pub const DESKEW_MIN_ROW_CONSENSUS: f64 = 0.30;

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
        .filter(|(_, det)| {
            det.confidence >= MIN_CONFIDENCE && det.text.trim().chars().count() >= MIN_TEXT_LENGTH
        })
        .map(|(index, _)| index)
        .collect()
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

/// Cross-product item/price candidates, filtered by column/width/proximity.
///
/// Mispairings are expected to fall out when the angle is chosen rather than
/// being filtered upfront.
fn build_pair_candidates(detections: &[Detection], image_width: f64) -> Vec<PairCandidate> {
    let item_x_max_cap = image_width * DESKEW_ITEM_X_MAX_FRAC;
    let price_x_min_floor = image_width * DESKEW_PRICE_X_MIN_FRAC;
    let min_item_width = image_width * DESKEW_MIN_ITEM_WIDTH;
    let min_price_width = image_width * DESKEW_MIN_PRICE_WIDTH;
    let min_x_distance = image_width * DESKEW_MIN_X_DISTANCE;

    let mut items: Vec<(f64, f64)> = Vec::new(); // (x_center, center_y)
    let mut prices: Vec<(f64, f64)> = Vec::new(); // (x_center, center_y)

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

/// Result of the deskew pass. `new_y` is `Some` only when the shear is applied.
pub struct DeskewOutcome {
    pub angle_deg: f64,
    pub applied: bool,
    pub gate_reason: Option<&'static str>,
    pub candidate_count: usize,
    pub inlier_count: usize,
    /// Distinct item rows agreeing with `angle_deg`, over the item rows that
    /// produced any candidate at all. Not a fraction of candidate *pairs* — see
    /// [`DESKEW_MIN_ROW_CONSENSUS`].
    pub consensus_ratio: f64,
    /// How much `angle_deg` tightens rows relative to no shear, as a fraction
    /// of the unsheared spread. See [`DESKEW_MIN_ROW_TIGHTENING`].
    pub row_tightening: f64,
    pub new_y: Option<Vec<(f64, f64, f64)>>,
}

/// Vertical shear correction driven by same-row item<->price slopes.
///
/// Bias is "miss safely": a wrong correction can push borderline rows out of
/// the matcher's y-band, so the pass only fires when the angle is in band,
/// large enough to matter, agreed on by enough item rows, clearly better than
/// the one-row-aliased runner-up, and corroborated by rows actually tightening.
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

    if gate_reason.is_some() {
        return DeskewOutcome {
            angle_deg: angle,
            applied: false,
            gate_reason,
            candidate_count,
            inlier_count: inliers,
            consensus_ratio,
            row_tightening,
            new_y: None,
        };
    }

    let new_y = apply_shear(detections, angle, image_width);
    DeskewOutcome {
        angle_deg: angle,
        applied: true,
        gate_reason: None,
        candidate_count,
        inlier_count: inliers,
        consensus_ratio,
        row_tightening,
        new_y: Some(new_y),
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
