//! Data-driven merchant identity resolution.
//!
//! OCR mangles the store banner more than any other field: it sits in a
//! stylized logo, so PP-OCRv5 emits things like "OSTC0" for "COSTCO". Because
//! the merchant is a small, closed vocabulary (unlike line items), we can match
//! the noisy header against a curated family dictionary and normalize it — which
//! is exactly the field the user reads first, so a correct one lifts perceived
//! accuracy the most.
//!
//! The hazard is the inverse: confidently rewriting a *correct but unlisted*
//! merchant into a wrong-but-listed one ("COSTLESS" -> "COSTCO") launders
//! uncertainty into false authority, which is worse than the raw OCR error the
//! user could have spotted. So resolution is deliberately conservative:
//!
//!   * The **raw** OCR text is always preserved on the result.
//!   * A fuzzy (approximate) header match is only ever auto-applied
//!     (`Corrected`) when the receipt also carries a family **corroborator**
//!     token (e.g. Costco's "WHOLESALE" banner). Absent that, a close match is
//!     surfaced as a non-authoritative `Suggested` and the display string stays
//!     the raw text — the UI can show it in grey without trusting it.
//!
//! This generalizes the former hardcoded per-merchant branches
//! (Costco/FreshCo/Foody Mart) in `receipt_parse_helpers::extract_merchant`.

use std::cmp::Reverse;

use regex::Regex;

/// One canonical merchant and the spellings that map to it. Loaded from
/// `rules/default_merchant_families.toml` (see `rules::default_merchant_families`).
#[derive(Clone, Debug)]
pub struct MerchantFamily {
    /// Display name the merchant is normalized to.
    pub canonical: String,
    /// Alternate / OCR-mangled spellings matched as exact word-boundary
    /// substrings anywhere in the receipt text.
    pub aliases: Vec<String>,
    /// Tokens that, when present anywhere in the receipt, license a *fuzzy*
    /// header match to be auto-applied. Empty means a fuzzy match for this
    /// family can never exceed `Suggested`.
    pub corroborators: Vec<String>,
}

/// How much to trust `MerchantMatch::canonical`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MerchantMatchStatus {
    /// The raw text already contains a known merchant verbatim; no rewrite.
    Exact,
    /// Confidently normalized to a canonical family (exact alias, or a fuzzy
    /// header match backed by a corroborator). Safe to display in place of raw.
    Corrected,
    /// A plausible canonical family, but not corroborated. Surfaced for the UI
    /// to offer; the display string stays the raw OCR text.
    Suggested,
    /// No family matched; the raw OCR text is kept as-is.
    Unknown,
}

/// Result of resolving one OCR'd merchant header.
#[derive(Clone, Debug)]
pub struct MerchantMatch {
    /// Exactly what OCR/extraction produced for the merchant header.
    pub raw: String,
    /// The canonical family name, when one was matched.
    pub canonical: Option<String>,
    pub status: MerchantMatchStatus,
    /// Similarity of the chosen family in `[0, 1]` (`1.0` for an exact match,
    /// `0.0` when nothing matched). For display/diagnostics only.
    pub score: f64,
}

impl MerchantMatch {
    /// The string to render / post / diff as *the* merchant. Only an `Exact` or
    /// `Corrected` match is trusted enough to replace the raw OCR text; a
    /// `Suggested` or `Unknown` match falls back to raw so nothing is silently
    /// rewritten on a low-confidence guess.
    pub fn display(&self) -> &str {
        match self.status {
            MerchantMatchStatus::Exact | MerchantMatchStatus::Corrected => {
                self.canonical.as_deref().unwrap_or(&self.raw)
            }
            MerchantMatchStatus::Suggested | MerchantMatchStatus::Unknown => &self.raw,
        }
    }
}

/// A fuzzy header match must clear this similarity to be auto-applied (with a
/// corroborator) — tuned so a single dropped/confusable char on a ~6-char name
/// still passes (e.g. "OSTCO" vs "COSTCO" = 0.833).
const HIGH_SIMILARITY: f64 = 0.82;
/// Below `HIGH` but above this, a match is only ever offered as `Suggested`.
const MEDIUM_SIMILARITY: f64 = 0.66;
/// Substitution cost for a visually confusable pair — cheap, so OCR's systematic
/// glyph swaps rank far ahead of coincidental edits.
const CONFUSABLE_SUB_COST: f64 = 0.3;

/// Resolve an OCR'd merchant header to a canonical family, conservatively.
///
/// * `raw_header` — best-effort OCR'd merchant name (may be noisy or empty).
/// * `full_text_upper` — the whole receipt text, uppercased, for alias/token search.
/// * `known_merchants` — exact keywords (from the merchant *rules*); matching one
///   verbatim preserves prior behavior and yields `Exact`.
/// * `families` — the canonical/alias/corroborator dictionary.
pub fn resolve(
    raw_header: &str,
    full_text_upper: &str,
    known_merchants: &[String],
    families: &[MerchantFamily],
) -> MerchantMatch {
    let raw = raw_header.trim().to_string();

    let canonical_for = |surface_upper: &str| -> Option<String> {
        families
            .iter()
            .find(|family| {
                family.canonical.eq_ignore_ascii_case(surface_upper)
                    || family
                        .aliases
                        .iter()
                        .any(|alias| alias.eq_ignore_ascii_case(surface_upper))
            })
            .map(|family| family.canonical.clone())
    };

    // Step 1: exact known-merchant keyword anywhere in the text. This is the
    // primary, behavior-preserving path (a verbatim "COSTCO" stays "COSTCO"),
    // upgraded so a keyword that is also a family alias resolves to its canonical.
    let mut keywords: Vec<&String> = known_merchants.iter().collect();
    keywords.sort_by_key(|keyword| Reverse(keyword.len()));
    for keyword in keywords {
        let keyword_upper = keyword.to_ascii_uppercase();
        if word_boundary_contains(full_text_upper, &keyword_upper) {
            let canonical = canonical_for(&keyword_upper).unwrap_or_else(|| keyword.clone());
            let status = if canonical.eq_ignore_ascii_case(keyword) {
                MerchantMatchStatus::Exact
            } else {
                MerchantMatchStatus::Corrected
            };
            return MerchantMatch {
                raw,
                canonical: Some(canonical),
                status,
                score: 1.0,
            };
        }
    }

    // Step 2: exact family canonical/alias as a word-boundary substring anywhere
    // in the text. Recovers cases where the banner is mangled but the correct
    // spelling reappears (FreshCo in the address footer) or the name runs into
    // the branch/address on one line (Foody Mart).
    let mut surfaces: Vec<(String, &str)> = Vec::new();
    for family in families {
        surfaces.push((
            family.canonical.to_ascii_uppercase(),
            family.canonical.as_str(),
        ));
        for alias in &family.aliases {
            surfaces.push((alias.to_ascii_uppercase(), family.canonical.as_str()));
        }
    }
    surfaces.sort_by_key(|(surface, _)| Reverse(surface.len()));
    for (surface, canonical) in &surfaces {
        if word_boundary_contains(full_text_upper, surface) {
            let status = if raw.eq_ignore_ascii_case(canonical) {
                MerchantMatchStatus::Exact
            } else {
                MerchantMatchStatus::Corrected
            };
            return MerchantMatch {
                raw,
                canonical: Some((*canonical).to_string()),
                status,
                score: 1.0,
            };
        }
    }

    // Step 3: fuzzy match of the header against each family, using a
    // confusion-weighted distance. Auto-apply only when corroborated; otherwise
    // offer as a suggestion.
    let raw_norm = normalize(&raw);
    if !raw_norm.is_empty() {
        let mut best: Option<(f64, &MerchantFamily)> = None;
        for family in families {
            let mut score = similarity(&raw_norm, &normalize(&family.canonical));
            for alias in &family.aliases {
                score = score.max(similarity(&raw_norm, &normalize(alias)));
            }
            if best.map_or(true, |(best_score, _)| score > best_score) {
                best = Some((score, family));
            }
        }
        if let Some((score, family)) = best {
            let corroborated = corroborator_present(full_text_upper, &family.corroborators);
            if score >= HIGH_SIMILARITY && corroborated {
                return MerchantMatch {
                    raw,
                    canonical: Some(family.canonical.clone()),
                    status: MerchantMatchStatus::Corrected,
                    score,
                };
            }
            if score >= MEDIUM_SIMILARITY {
                return MerchantMatch {
                    raw,
                    canonical: Some(family.canonical.clone()),
                    status: MerchantMatchStatus::Suggested,
                    score,
                };
            }
        }
    }

    // Step 4: nothing matched — keep the raw OCR text.
    MerchantMatch {
        raw,
        canonical: None,
        status: MerchantMatchStatus::Unknown,
        score: 0.0,
    }
}

/// True if any corroborator token appears in the receipt text. Besides a direct
/// substring, the whitespace-collapsed text is also checked so a corroborator the
/// OCR split across a space still counts — e.g. Costco's "WHOLESALE" frequently
/// reads as "WHOL ESALE", which would otherwise drop the fuzzy header match
/// ("OSTCO" -> "COSTCO") from an authoritative `Corrected` down to a `Suggested`.
/// `full_text_upper` is expected pre-uppercased.
fn corroborator_present(full_text_upper: &str, corroborators: &[String]) -> bool {
    if corroborators.is_empty() {
        return false;
    }
    let collapsed: String = full_text_upper.split_whitespace().collect();
    corroborators.iter().any(|token| {
        let token = token.to_ascii_uppercase();
        full_text_upper.contains(&token) || collapsed.contains(&token.replace(' ', ""))
    })
}

/// True if `needle` occurs in `haystack` bounded by word boundaries. Both are
/// expected pre-uppercased; `needle` is regex-escaped so names with `&`/`-`/`'`
/// are matched literally.
fn word_boundary_contains(haystack: &str, needle: &str) -> bool {
    let pattern = format!(r"\b{}\b", regex::escape(needle));
    Regex::new(&pattern).is_ok_and(|re| re.is_match(haystack))
}

/// Uppercase and strip everything but ASCII alphanumerics, so fuzzy comparison
/// ignores spacing/punctuation ("FOODY MART" vs "FOODYMART").
fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Visually confusable glyph pairs OCR routinely swaps. Order-independent.
fn is_confusable(a: char, b: char) -> bool {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    matches!(
        (lo, hi),
        ('0', 'O')
            | ('0', 'Q')
            | ('1', 'I')
            | ('1', 'L')
            | ('I', 'L')
            | ('5', 'S')
            | ('8', 'B')
            | ('2', 'Z')
            | ('6', 'G')
            | ('C', 'G')
    )
}

/// Levenshtein distance where a confusable substitution is discounted to
/// `CONFUSABLE_SUB_COST` (insert/delete/ordinary-substitute cost 1.0).
fn weighted_distance(a: &[char], b: &[char]) -> f64 {
    if a.is_empty() {
        return b.len() as f64;
    }
    if b.is_empty() {
        return a.len() as f64;
    }
    let mut prev: Vec<f64> = (0..=b.len()).map(|j| j as f64).collect();
    let mut curr: Vec<f64> = vec![0.0; b.len() + 1];
    for i in 1..=a.len() {
        curr[0] = i as f64;
        for j in 1..=b.len() {
            let sub = if a[i - 1] == b[j - 1] {
                0.0
            } else if is_confusable(a[i - 1], b[j - 1]) {
                CONFUSABLE_SUB_COST
            } else {
                1.0
            };
            curr[j] = (prev[j] + 1.0)
                .min(curr[j - 1] + 1.0)
                .min(prev[j - 1] + sub);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Normalized similarity in `[0, 1]`: `1 - distance / longer_length`.
fn similarity(a: &str, b: &str) -> f64 {
    let ca: Vec<char> = a.chars().collect();
    let cb: Vec<char> = b.chars().collect();
    let longer = ca.len().max(cb.len());
    if longer == 0 {
        return 0.0;
    }
    1.0 - weighted_distance(&ca, &cb) / longer as f64
}

#[cfg(test)]
mod tests {
    use super::*;

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
        ]
    }

    #[test]
    fn fuzzy_header_with_corroborator_is_corrected() {
        // "OSTCO" (dropped leading C) + "WHOLESALE" banner -> confident Costco.
        let m = resolve("OSTCO", "OSTCO WHOLESALE BRANCH #001", &[], &families());
        assert_eq!(m.status, MerchantMatchStatus::Corrected);
        assert_eq!(m.display(), "COSTCO");
        assert_eq!(m.raw, "OSTCO");
    }

    #[test]
    fn split_corroborator_still_corrects_costco() {
        // OCR dropped Costco's leading C ("OSTCO") and split "WHOLESALE" across a
        // space ("WHOL ESALE"). The fuzzy header still matches COSTCO, and the
        // whitespace-collapsed corroborator recovers "WHOLESALE" so the match is
        // authoritative (Corrected) rather than a non-applied Suggestion.
        let m = resolve(
            "OSTCO WHOL ESALE",
            "OSTCO WHOL ESALE #545 65 KIRKHAM DRIVE",
            &[],
            &families(),
        );
        assert_eq!(m.status, MerchantMatchStatus::Corrected);
        assert_eq!(m.display(), "COSTCO");
    }

    #[test]
    fn lookalike_without_corroborator_is_not_silently_rewritten() {
        // "COSTLESS" is close-ish to COSTCO but is a different, real store and
        // there is no corroborator: never auto-applied, display stays raw.
        let m = resolve("COSTLESS", "COSTLESS FOODS 123 MAIN ST", &[], &families());
        assert_ne!(m.status, MerchantMatchStatus::Corrected);
        assert_eq!(m.display(), "COSTLESS");
    }

    #[test]
    fn exact_alias_in_footer_is_corrected() {
        // Banner OCR'd "FRESHCC"; correct spelling reappears in the address.
        let m = resolve(
            "FRESHCC",
            "FRESHCC 123 EXAMPLE ST FRESHCO",
            &[],
            &families(),
        );
        assert_eq!(m.status, MerchantMatchStatus::Corrected);
        assert_eq!(m.display(), "FRESHCO");
    }

    #[test]
    fn pharmasave_recovered_from_franchise_banner() {
        // The independently-owned franchise name ("GRAND GENESIS") is the raw
        // header; the "PHARMASAVE" banner sits on the line below and recovers the
        // drugstore, so the merchant is no longer left Unknown.
        let families = vec![MerchantFamily {
            canonical: "PHARMASAVE".to_string(),
            aliases: vec!["PHARMASAVE".to_string()],
            corroborators: vec![],
        }];
        let m = resolve(
            "GRAND GENESIS",
            "GRAND GENESIS PHARMASAVE HAVE A GREAT DAY",
            &[],
            &families,
        );
        assert_eq!(m.status, MerchantMatchStatus::Corrected);
        assert_eq!(m.display(), "PHARMASAVE");
        assert_eq!(m.raw, "GRAND GENESIS");
    }

    #[test]
    fn exact_known_keyword_is_exact_and_preserved() {
        let m = resolve(
            "COSTCO",
            "COSTCO WHOLESALE",
            &["COSTCO".to_string()],
            &families(),
        );
        assert_eq!(m.status, MerchantMatchStatus::Exact);
        assert_eq!(m.display(), "COSTCO");
    }

    #[test]
    fn unrelated_merchant_is_unknown() {
        let m = resolve("SHOPRITE", "SHOPRITE 123 MAIN STREET", &[], &families());
        assert_eq!(m.status, MerchantMatchStatus::Unknown);
        assert_eq!(m.canonical, None);
        assert_eq!(m.display(), "SHOPRITE");
    }

    #[test]
    fn confusable_substitution_beats_ordinary_edit() {
        // 0<->O is discounted, so "C0STCO" scores higher against COSTCO than a
        // same-position ordinary substitution would.
        assert!(similarity("C0STCO", "COSTCO") > similarity("CXSTCO", "COSTCO"));
    }
}
