//! Recover a timestamp from a larger-scale header without replacing the receipt.
use crate::{ScanError, ScanOptions, ScanRequest, OCR_IMAGE_PADDING};
use ocr_paddle::engine::Detection;
use receipt_core::date::Date;
use receipt_core::ocr_transform::{RawDetection, RawDetectionPage};
use receipt_core::process::process_receipt_request;

pub(super) fn header_timestamp(
    detections: Vec<Detection>,
    width: u32,
    height: u32,
    request: ScanRequest<'_>,
    options: &ScanOptions,
) -> Result<Option<RawDetection>, ScanError> {
    let raw: Vec<_> = detections
        .into_iter()
        .map(|d| RawDetection {
            points: d
                .points
                .iter()
                .map(|p| (p[0] as f64, p[1] as f64))
                .collect(),
            text: d.text,
            confidence: d.confidence as f64,
        })
        .collect();
    let candidates: Vec<_> = raw
        .iter()
        .filter_map(|d| timestamp_date(&d.text).map(|date| (date, d)))
        .collect();
    let Some((date, detection)) = candidates
        .iter()
        .max_by(|a, b| a.1.confidence.total_cmp(&b.1.confidence))
    else {
        return Ok(None);
    };
    // A second, conflicting timestamp is ambiguity, not an invitation to guess.
    if candidates.iter().any(|(other, _)| other != date) {
        return Ok(None);
    }
    let candidate = (*detection).clone();
    let date = *date;
    // Let the ordinary date parser inspect surrounding labels (return/contest
    // deadlines, etc.) too. A timestamp alone must not bypass those exclusions.
    let page =
        RawDetectionPage::try_new(raw, width as i64, height as i64, OCR_IMAGE_PADDING as i64)?;
    let header = process_receipt_request(page, request, options).map_err(ScanError::Rules)?;
    Ok((header.parsed.date == Some(date)).then_some(candidate))
}

/// Require a complete, unambiguous year-first date AND a clock time. Bare
/// dates, receipt numbers, OCR-repaired digits and free-form prose cannot vote.
fn timestamp_date(text: &str) -> Option<Date> {
    let tokens: Vec<_> = text.split_whitespace().collect();
    if tokens.len() != 2 {
        return None;
    }
    let parts: Vec<_> = tokens[0].split(['/', '-']).collect();
    if parts.len() != 3
        || parts[0].len() != 4
        || parts[1].len() != 2
        || parts[2].len() != 2
        || !parts.iter().all(|p| p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let time: Vec<_> = tokens[1].split(':').collect();
    if !(2..=3).contains(&time.len())
        || !time
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_digit()))
        || time[0].parse::<u32>().ok()? > 23
        || time[1].parse::<u32>().ok()? > 59
        || (time.len() == 3 && time[2].parse::<u32>().ok()? > 59)
    {
        return None;
    }
    Date::new(
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn detection(text: &str, y: f32, confidence: f32) -> Detection {
        Detection {
            text: text.into(),
            confidence,
            points: [[70.0, y], [400.0, y], [400.0, y + 20.0], [70.0, y + 20.0]],
        }
    }

    #[test]
    fn header_recovery_obeys_confidence_deadlines_and_ambiguity() {
        let request = ScanRequest {
            image_filename: "synthetic.jpg",
            today: Date::new(2026, 9, 15).unwrap(),
            credit_card_account: "Liabilities:CreditCard:PENDING",
            currency: "CAD",
            tax_account: "Expenses:Tax:HST",
            image_sha256: None,
        };
        let recover = |detections| {
            header_timestamp(detections, 600, 600, request, &ScanOptions::default()).unwrap()
        };
        let timestamp = detection("2026/09/12 14:49", 160.0, 0.99);
        assert_eq!(
            recover(vec![timestamp.clone()]).unwrap().text,
            timestamp.text
        );
        assert!(recover(vec![detection("2026/09/12 14:49", 160.0, 0.5)]).is_none());
        assert!(recover(vec![
            timestamp.clone(),
            detection("2026/09/13 15:00", 200.0, 0.99)
        ])
        .is_none());
        for label in ["RETURN DATE", "CONTEST ENDS"] {
            assert!(
                recover(vec![detection(label, 120.0, 0.99), timestamp.clone()]).is_none(),
                "{label}"
            );
        }
        assert!(recover(vec![detection("Receipt# 202609121449", 160.0, 0.99)]).is_none());
    }

    #[test]
    fn timestamp_requires_calendar_date_and_clock() {
        assert_eq!(timestamp_date("2026/09/12 14:49"), Date::new(2026, 9, 12));
        assert_eq!(
            timestamp_date("2026-09-12 14:49:00"),
            Date::new(2026, 9, 12)
        );
        for text in [
            "2026/09/12",
            "26/09/12 14:49",
            "2026/02/30 14:49",
            "2026/09/12 24:49",
            "2026/09/12 14:60",
            "2026/09/12 14:49:60",
            "Receipt# 20260912",
            "Return by 2026/09/12 14:49",
        ] {
            assert_eq!(timestamp_date(text), None, "{text}");
        }
    }
}
