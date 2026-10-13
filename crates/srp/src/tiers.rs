//! Subscription tiers: which deposits buy coverage, of what kind and until when.
//!
//! The prices come from the `[srp]` config section ([`SrpConfig`]); the defaults are the legacy
//! waitlist's (daily: 20, 35, 45 ... 80 M ISK; per focus: 125, 225, 295 ... 680 M ISK, the n-th
//! price covering n characters).

use chrono::{DateTime, Duration, NaiveTime, Utc};
use serde::Serialize;
use wl_core::config::SrpConfig;

/// Daily or per-focus coverage (the `coverage_type` column).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageType {
    Daily,
    PerFocus,
}

impl CoverageType {
    pub fn as_str(self) -> &'static str {
        match self {
            CoverageType::Daily => "daily",
            CoverageType::PerFocus => "per_focus",
        }
    }
}

/// The coverage a deposit buys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coverage {
    pub kind: CoverageType,
    pub expires_at: DateTime<Utc>,
    /// Characters covered (the tier).
    pub characters: usize,
}

/// The tier table as clients see it (prices in millions of ISK; entry n covers n characters).
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct TierTable {
    /// Daily coverage prices (M ISK).
    pub daily: Vec<f64>,
    /// Per-focus coverage prices (M ISK).
    pub per_focus: Vec<f64>,
}

impl TierTable {
    pub fn from_config(config: &SrpConfig) -> Self {
        Self {
            daily: config.daily_prices.clone(),
            per_focus: config.per_focus_prices.clone(),
        }
    }
}

fn amount_matches(amount_millions: f64, expected_millions: f64) -> bool {
    (amount_millions - expected_millions).abs() < 0.01
}

/// When daily coverage bought at `paid_at` ends: the reset hour the same day when paid before
/// it, else the next day's.
pub fn daily_coverage_end(config: &SrpConfig, paid_at: DateTime<Utc>) -> DateTime<Utc> {
    let reset = NaiveTime::from_hms_opt(config.daily_reset_hour_utc.min(23), 0, 0)
        .unwrap_or(NaiveTime::MIN);
    let paid = paid_at.naive_utc();
    let day = if paid.time() < reset {
        paid.date()
    } else {
        paid.date() + Duration::days(1)
    };
    day.and_time(reset).and_utc()
}

/// Classify a deposit of `amount_isk`. Full prices are matched first (so an FC paying the
/// normal price gets that tier), then, for FC-eligible payers, prices times `fc_price_factor`.
pub fn coverage_for(
    config: &SrpConfig,
    amount_isk: f64,
    paid_at: DateTime<Utc>,
    fc_eligible: bool,
    fc_price_factor: f64,
) -> Option<Coverage> {
    let millions = amount_isk / 1_000_000.0;
    let mut factors = vec![1.0];
    if fc_eligible {
        factors.push(fc_price_factor);
    }
    for factor in factors {
        for (kind, prices) in [
            (CoverageType::Daily, &config.daily_prices),
            (CoverageType::PerFocus, &config.per_focus_prices),
        ] {
            if let Some(index) = prices
                .iter()
                .position(|price| amount_matches(millions, price * factor))
            {
                let expires_at = match kind {
                    CoverageType::Daily => daily_coverage_end(config, paid_at),
                    CoverageType::PerFocus => paid_at + Duration::days(config.per_focus_days),
                };
                return Some(Coverage {
                    kind,
                    expires_at,
                    characters: index + 1,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn daily_coverage_ends_at_the_next_reset() {
        let config = SrpConfig::default();
        assert_eq!(
            daily_coverage_end(&config, at("2024-05-01T10:59:59Z")),
            at("2024-05-01T11:00:00Z")
        );
        assert_eq!(
            daily_coverage_end(&config, at("2024-05-01T11:00:00Z")),
            at("2024-05-02T11:00:00Z")
        );
        assert_eq!(
            daily_coverage_end(&config, at("2024-05-31T23:00:00Z")),
            at("2024-06-01T11:00:00Z")
        );
    }

    #[test]
    fn classifies_legacy_tiers() {
        let config = SrpConfig::default();
        let t = at("2024-05-01T12:00:00Z");
        let daily = coverage_for(&config, 35_000_000.0, t, false, 0.5).unwrap();
        assert_eq!(daily.kind, CoverageType::Daily);
        assert_eq!(daily.characters, 2);
        assert_eq!(daily.expires_at, at("2024-05-02T11:00:00Z"));

        let focus = coverage_for(&config, 680_000_000.0, t, false, 0.5).unwrap();
        assert_eq!(focus.kind, CoverageType::PerFocus);
        assert_eq!(focus.characters, 12);
        assert_eq!(focus.expires_at, at("2024-05-09T12:00:00Z"));

        assert_eq!(coverage_for(&config, 21_000_000.0, t, false, 0.5), None);
        // 10M is half of the 1-character daily price: only for FCs.
        assert_eq!(coverage_for(&config, 10_000_000.0, t, false, 0.5), None);
        let fc = coverage_for(&config, 10_000_000.0, t, true, 0.5).unwrap();
        assert_eq!((fc.kind, fc.characters), (CoverageType::Daily, 1));
        // 62.5M = half of 125M: per-focus FC price.
        let fc = coverage_for(&config, 62_500_000.0, t, true, 0.5).unwrap();
        assert_eq!((fc.kind, fc.characters), (CoverageType::PerFocus, 1));
        // A full price wins over a discounted match (35M is also 0.5 × 70M).
        let fc = coverage_for(&config, 35_000_000.0, t, true, 0.5).unwrap();
        assert_eq!((fc.kind, fc.characters), (CoverageType::Daily, 2));
    }

    #[test]
    fn uses_configured_tiers() {
        let config = SrpConfig {
            daily_prices: vec![15.0],
            per_focus_prices: vec![],
            daily_reset_hour_utc: 0,
            per_focus_days: 3,
            ..SrpConfig::default()
        };
        let t = at("2024-05-01T12:00:00Z");
        let daily = coverage_for(&config, 15_000_000.0, t, false, 0.5).unwrap();
        assert_eq!(daily.expires_at, at("2024-05-02T00:00:00Z"));
        assert_eq!(coverage_for(&config, 20_000_000.0, t, false, 0.5), None);
    }
}
