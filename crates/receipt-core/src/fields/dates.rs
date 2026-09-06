//! Receipt dates extraction.
use super::prices::normalize_decimal_spacing;
use crate::date::Date;
use regex::Regex;
use std::cmp::Ordering;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub(super) struct RankedDateCandidate {
    pub(super) score: i32,
    pub(super) line_index: usize,
    pub(super) start: usize,
    pub(super) date: Date,
}

/// Marks a line as date context, which is what lets `extract_date` consider the
/// year-first (`26/08/02`) reading at all — see the `ymd2` gate in
/// [`extract_date`], where a missing hint *discards* that candidate outright.
///
/// Matches the `DATE` prefix rather than whole words, because the suffix is
/// where OCR damage lands and it carries no information. A No Frills receipt
/// printing `DateTime: 26/08/02` came back as `Datelime`, and the lost word
/// boundary after `DATE` cost the hint, the year-first reading, and finally the
/// date itself: 2026-08-02 became 2002-08-26. Keying on the prefix survives any
/// mangling of `TIME` — `DATETIME`, `DATELIME`, `DATEIIME` all hint alike.
///
/// `\bDATE` still requires a boundary *before* the prefix, so `UPDATE` does not
/// match.
pub(super) fn re_date_context_hint() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b(DATE\w*|TRANS(?:ACTION)?\s*DATE\w*)").unwrap())
}
pub(super) fn re_separated_date() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(^|[^0-9])(\d{1,4})[./-](\d{1,2})[./-](\d{1,4})([^0-9]|$)").unwrap()
    })
}
pub(super) fn re_compact_date() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(^|[^0-9])(\d{4})(\d{2})(\d{2})([^0-9]|$)").unwrap())
}
pub(super) fn re_month_name_date() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\w*\s+(\d{1,2}),?\s+(\d{4})\b",
        )
        .unwrap()
    })
}

// Day-first month-name dates, e.g. "22-May-2026" or "22 May 2026". The month
// may carry an abbreviation period ("02-Apr.-2026", Clover's format).
pub(super) fn re_dmy_month_name_date() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(\d{1,2})[-\s]+(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\w*\.?[-\s]+(\d{4})\b",
        )
        .unwrap()
    })
}

/// Whether a date is explicitly labelled as a return deadline rather than the
/// transaction date.
///
/// The label can sit one or two OCR lines above the date itself (LCBO prints an
/// English line, a French line, then the deadline), so inspecting only the
/// candidate line is insufficient. A date between that label and the current
/// line closes the context: it is the deadline itself, and a following date is
/// free to be the transaction timestamp. Returning no date is safer than
/// booking a purchase on the last day it may be returned.
pub(super) fn is_return_deadline_context(lines: &[String], line_index: usize) -> bool {
    let start = line_index.saturating_sub(2);
    let context = lines[start..=line_index].join(" ").to_ascii_uppercase();
    let has_return_label =
        context.contains("DATE") && (context.contains("RETURN") || context.contains("RETOUR"));
    let has_intervening_date = lines[start..line_index].iter().any(|line| {
        re_separated_date().is_match(line)
            || re_compact_date().is_match(line)
            || re_month_name_date().is_match(line)
            || re_dmy_month_name_date().is_match(line)
    });
    has_return_label && !has_intervening_date
}
pub(super) fn month_number_from_name(name: &str) -> Option<i32> {
    match name.get(..3).unwrap_or("").to_ascii_lowercase().as_str() {
        "jan" => Some(1),
        "feb" => Some(2),
        "mar" => Some(3),
        "apr" => Some(4),
        "may" => Some(5),
        "jun" => Some(6),
        "jul" => Some(7),
        "aug" => Some(8),
        "sep" => Some(9),
        "oct" => Some(10),
        "nov" => Some(11),
        "dec" => Some(12),
        _ => None,
    }
}
pub(super) fn to_four_digit_year(year: i32) -> i32 {
    if year < 100 {
        if year <= 69 {
            2000 + year
        } else {
            1900 + year
        }
    } else {
        year
    }
}
pub(super) fn numeric_date_candidates(
    part1: &str,
    part2: &str,
    part3: &str,
) -> Vec<(Date, &'static str)> {
    let a = match part1.parse::<i32>() {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let b = match part2.parse::<i32>() {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let c = match part3.parse::<i32>() {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };

    let mut candidates = Vec::new();
    let mut add = |year: i32, month: i32, day: i32, kind: &'static str| {
        let (Ok(month), Ok(day)) = (u32::try_from(month), u32::try_from(day)) else {
            return;
        };
        if let Some(parsed) = Date::new(year, month, day) {
            candidates.push((parsed, kind));
        }
    };

    if part1.len() == 4 {
        add(a, b, c, "ymd4");
        return candidates;
    }

    if part3.len() == 4 {
        if a > 12 && b <= 12 {
            add(c, b, a, "dmy4");
        } else if b > 12 && a <= 12 {
            add(c, a, b, "mdy4");
        } else {
            add(c, a, b, "mdy4");
            add(c, b, a, "dmy4");
        }
        return candidates;
    }

    let year_a = to_four_digit_year(a);
    let year_c = to_four_digit_year(c);

    if b <= 12 && c <= 31 {
        add(year_a, b, c, "ymd2");
    }
    if a <= 12 && b <= 31 {
        add(year_c, a, b, "mdy2");
    }
    if b <= 12 && a <= 31 {
        add(year_c, b, a, "dmy2");
    }

    candidates
}
pub(super) fn year_score(candidate_year: i32, current_year: i32) -> i32 {
    10 - (candidate_year - current_year).abs().min(10)
}
pub(super) fn kind_base_score(kind: &str) -> i32 {
    match kind {
        "ymd4" => 35,
        "ymd2" => 28,
        "mdy4" => 25,
        "dmy4" => 24,
        "mdy2" => 22,
        "dmy2" => 20,
        _ => 0,
    }
}
pub(super) fn compare_ranked_candidates(
    left: &RankedDateCandidate,
    right: &RankedDateCandidate,
) -> Ordering {
    right
        .score
        .cmp(&left.score)
        .then_with(|| left.line_index.cmp(&right.line_index))
        .then_with(|| left.start.cmp(&right.start))
}
pub fn extract_date(lines: &[String], full_text: &str, current_year: i32) -> Option<Date> {
    if lines.is_empty() && full_text.is_empty() {
        return None;
    }

    let source_lines: Vec<String> = if lines.is_empty() {
        full_text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()
    } else {
        lines.to_vec()
    };
    let current_yy = current_year.rem_euclid(100);
    let mut ranked_candidates = Vec::new();

    for (line_index, line) in source_lines.iter().enumerate() {
        let normalized_line = normalize_decimal_spacing(line);
        if is_return_deadline_context(&source_lines, line_index) {
            continue;
        }
        let hint_bonus = if re_date_context_hint().is_match(&normalized_line) {
            40
        } else {
            0
        };
        let prefer_year_first = hint_bonus > 0;

        for captures in re_separated_date().captures_iter(&normalized_line) {
            let part1 = captures.get(2).map(|m| m.as_str()).unwrap_or("");
            let part2 = captures.get(3).map(|m| m.as_str()).unwrap_or("");
            let part3 = captures.get(4).map(|m| m.as_str()).unwrap_or("");
            let start = captures.get(2).map(|m| m.start()).unwrap_or(0);
            for (candidate_date, kind) in numeric_date_candidates(part1, part2, part3) {
                if kind == "ymd2" {
                    let year_token = match part1.parse::<i32>() {
                        Ok(value) => value,
                        Err(_) => continue,
                    };
                    if !(prefer_year_first && (20..=current_yy + 1).contains(&year_token)) {
                        continue;
                    }
                }
                let mut base = kind_base_score(kind);
                if kind == "mdy2" {
                    base += 2;
                }
                if kind == "ymd2" && prefer_year_first {
                    base += 3;
                }
                ranked_candidates.push(RankedDateCandidate {
                    score: base + hint_bonus + year_score(candidate_date.year(), current_year),
                    line_index,
                    start,
                    date: candidate_date,
                });
            }
        }

        for captures in re_compact_date().captures_iter(&normalized_line) {
            let year = captures.get(2).and_then(|m| m.as_str().parse::<i32>().ok());
            let month = captures.get(3).and_then(|m| m.as_str().parse::<i32>().ok());
            let day = captures.get(4).and_then(|m| m.as_str().parse::<i32>().ok());
            let start = captures.get(2).map(|m| m.start()).unwrap_or(0);
            if let (Some(year), Some(month), Some(day)) = (year, month, day) {
                if let Some(compact_date) =
                    Date::new(year, u32::try_from(month).ok()?, u32::try_from(day).ok()?)
                {
                    ranked_candidates.push(RankedDateCandidate {
                        score: 30 + hint_bonus + year_score(compact_date.year(), current_year),
                        line_index,
                        start,
                        date: compact_date,
                    });
                }
            }
        }

        for captures in re_month_name_date().captures_iter(&normalized_line) {
            let month = captures
                .get(1)
                .and_then(|m| month_number_from_name(m.as_str()));
            let day = captures.get(2).and_then(|m| m.as_str().parse::<i32>().ok());
            let year = captures.get(3).and_then(|m| m.as_str().parse::<i32>().ok());
            let start = captures.get(1).map(|m| m.start()).unwrap_or(0);
            if let (Some(month), Some(day), Some(year)) = (month, day, year) {
                if let Some(parsed) =
                    Date::new(year, u32::try_from(month).ok()?, u32::try_from(day).ok()?)
                {
                    ranked_candidates.push(RankedDateCandidate {
                        score: 26 + hint_bonus + year_score(parsed.year(), current_year),
                        line_index,
                        start,
                        date: parsed,
                    });
                }
            }
        }

        for captures in re_dmy_month_name_date().captures_iter(&normalized_line) {
            let day = captures.get(1).and_then(|m| m.as_str().parse::<i32>().ok());
            let month = captures
                .get(2)
                .and_then(|m| month_number_from_name(m.as_str()));
            let year = captures.get(3).and_then(|m| m.as_str().parse::<i32>().ok());
            let start = captures.get(1).map(|m| m.start()).unwrap_or(0);
            if let (Some(month), Some(day), Some(year)) = (month, day, year) {
                if let Some(parsed) =
                    Date::new(year, u32::try_from(month).ok()?, u32::try_from(day).ok()?)
                {
                    ranked_candidates.push(RankedDateCandidate {
                        score: 26 + hint_bonus + year_score(parsed.year(), current_year),
                        line_index,
                        start,
                        date: parsed,
                    });
                }
            }
        }
    }

    if ranked_candidates.is_empty() {
        return None;
    }

    ranked_candidates.sort_by(compare_ranked_candidates);
    ranked_candidates.first().map(|candidate| candidate.date)
}
