//! Statistics.
//!
//! - `GET /api/stats` (`stats-view`): fleet time, pilots and x-ups per month and hull, over all
//!   history (`fleet_activity`, `fit_history`), plus 28-day breakdowns. Series under a 1% share
//!   are folded into `"Other"`, which is always present. Months are `"YYYY-MM"` in the database
//!   session's time zone; all numbers are floats, as before.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::State;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_core::types::TypeId;
use wl_db::Db;
use wl_eve_data::{EveData, TypeError};

use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(statistics))
}

/// Share below which a series is folded into `"Other"`.
const OTHER_THRESHOLD: f64 = 0.01;
const DAYS_28: i64 = 28 * 86400;

/// `"YYYY-MM"`, ordered chronologically.
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy)]
struct YearMonth(i32, i32);

impl YearMonth {
    fn parse(input: &str) -> Result<Self, ApiError> {
        let invalid = || ApiError::internal(format!("unexpected month {input:?}"));
        let (year, month) = input.split_once('-').ok_or_else(invalid)?;
        Ok(Self(
            year.parse().map_err(|_| invalid())?,
            month.parse().map_err(|_| invalid())?,
        ))
    }
}

impl Serialize for YearMonth {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{:04}-{:02}", self.0, self.1))
    }
}

type Series = BTreeMap<String, f64>;

fn filter_into_other_1d(source: Series, threshold: f64) -> Series {
    let sum: f64 = source.values().sum();
    let mut other = 0.;
    let mut result = BTreeMap::new();
    for (name, value) in source {
        if value / sum > threshold {
            result.insert(name, value);
        } else {
            other += value;
        }
    }
    result.insert("Other".to_string(), other);
    result
}

fn filter_into_other_2d<T: Ord>(
    source: BTreeMap<T, Series>,
    threshold: f64,
) -> BTreeMap<T, Series> {
    let mut sums: BTreeMap<String, f64> = BTreeMap::new();
    let mut total = 0.;
    for series in source.values() {
        for (name, &value) in series {
            *sums.entry(name.clone()).or_default() += value;
            total += value;
        }
    }

    source
        .into_iter()
        .map(|(key, series)| {
            let mut other = 0.;
            let mut these = BTreeMap::new();
            for (name, value) in series {
                if sums.get(&name).copied().unwrap_or_default() / total > threshold {
                    these.insert(name, value);
                } else {
                    other += value;
                }
            }
            these.insert("Other".to_string(), other);
            (key, these)
        })
        .collect()
}

fn translate_hulls_1d(eve: &EveData, source: &BTreeMap<TypeId, f64>) -> Result<Series, TypeError> {
    source
        .iter()
        .map(|(id, value)| {
            let name = eve.name_of(*id).ok_or(TypeError::NothingMatched)?;
            Ok((name.to_string(), *value))
        })
        .collect()
}

fn translate_hulls_2d<K: Copy + Ord>(
    eve: &EveData,
    source: &BTreeMap<K, BTreeMap<TypeId, f64>>,
) -> Result<BTreeMap<K, Series>, TypeError> {
    source
        .iter()
        .map(|(k, series)| Ok((*k, translate_hulls_1d(eve, series)?)))
        .collect()
}

#[derive(sqlx::FromRow)]
struct MonthKeyRow {
    yearmonth: String,
    key: i64,
    value: i64,
}

#[derive(sqlx::FromRow)]
struct HullRow {
    hull: i32,
    value: i64,
}

/// `{month: {key: value}}` from rows of (month, key, value).
async fn by_month(
    db: &Db,
    sql: &'static str,
) -> ApiResult<BTreeMap<YearMonth, BTreeMap<i64, f64>>> {
    let rows: Vec<MonthKeyRow> = sqlx::query_as(sql).fetch_all(db).await?;
    let mut result: BTreeMap<YearMonth, BTreeMap<i64, f64>> = BTreeMap::new();
    for row in rows {
        result
            .entry(YearMonth::parse(&row.yearmonth)?)
            .or_default()
            .insert(row.key, row.value as f64);
    }
    Ok(result)
}

fn hull_keys(
    source: BTreeMap<YearMonth, BTreeMap<i64, f64>>,
) -> BTreeMap<YearMonth, BTreeMap<TypeId, f64>> {
    source
        .into_iter()
        .map(|(month, series)| {
            let series = series
                .into_iter()
                .filter_map(|(hull, value)| Some((TypeId::try_from(hull).ok()?, value)))
                .collect();
            (month, series)
        })
        .collect()
}

async fn by_hull_since(db: &Db, sql: &'static str, since: i64) -> ApiResult<BTreeMap<TypeId, f64>> {
    let rows: Vec<HullRow> = sqlx::query_as(sql).bind(since).fetch_all(db).await?;
    Ok(rows
        .into_iter()
        .map(|row| (row.hull, row.value as f64))
        .collect())
}

const FLEET_SECONDS_BY_CHARACTER_BY_MONTH: &str = "SELECT
        date_format(from_unixtime(first_seen), '%Y-%m') AS yearmonth,
        character_id AS `key`,
        CAST(SUM(last_seen - first_seen) AS SIGNED) AS value
    FROM fleet_activity
    GROUP BY 1, 2";

const FLEET_SECONDS_BY_HULL_BY_MONTH: &str = "SELECT
        date_format(from_unixtime(first_seen), '%Y-%m') AS yearmonth,
        CAST(hull AS SIGNED) AS `key`,
        CAST(SUM(last_seen - first_seen) AS SIGNED) AS value
    FROM fleet_activity
    GROUP BY 1, 2";

const XES_BY_HULL_BY_MONTH: &str = "SELECT
        date_format(from_unixtime(logged_at), '%Y-%m') AS yearmonth,
        CAST(hull AS SIGNED) AS `key`,
        COUNT(DISTINCT character_id) AS value
    FROM fit_history
    JOIN fitting ON fit_history.fit_id = fitting.id
    GROUP BY 1, 2";

const XES_BY_HULL_SINCE: &str = "SELECT hull, COUNT(DISTINCT character_id) AS value
    FROM fit_history
    JOIN fitting ON fit_history.fit_id = fitting.id
    WHERE logged_at > ?
    GROUP BY 1";

const FLEET_SECONDS_BY_HULL_SINCE: &str = "SELECT hull,
        CAST(SUM(last_seen - first_seen) AS SIGNED) AS value
    FROM fleet_activity
    WHERE first_seen > ?
    GROUP BY 1";

fn x_vs_time_by_hull(
    eve: &EveData,
    source_x: &BTreeMap<TypeId, f64>,
    source_time: &BTreeMap<TypeId, f64>,
) -> Result<BTreeMap<String, BTreeMap<&'static str, f64>>, TypeError> {
    let sum_x: f64 = source_x.values().sum();
    let sum_time: f64 = source_time.values().sum();
    let translated_x = filter_into_other_1d(translate_hulls_1d(eve, source_x)?, OTHER_THRESHOLD);
    let translated_time = translate_hulls_1d(eve, source_time)?;

    let mut result: BTreeMap<String, BTreeMap<&'static str, f64>> = translated_x
        .into_iter()
        .map(|(hull, x_count)| (hull, BTreeMap::from([("X", x_count / sum_x), ("Time", 0.)])))
        .collect();
    for (hull, time) in translated_time {
        let key = if result.contains_key(&hull) {
            hull
        } else {
            "Other".to_string()
        };
        // `filter_into_other_1d` always adds "Other".
        if let Some(entry) = result.get_mut(&key) {
            *entry.entry("Time").or_default() += time / sum_time;
        }
    }
    Ok(result)
}

fn time_spent_in_fleet_by_month(
    source: &BTreeMap<YearMonth, BTreeMap<i64, f64>>,
) -> BTreeMap<YearMonth, BTreeMap<&'static str, f64>> {
    source
        .iter()
        .map(|(month, pilots)| {
            let mut this_month: BTreeMap<&'static str, f64> = BTreeMap::new();
            for &time in pilots.values() {
                let bucket = match time {
                    t if t <= 3600. => "a. <1h",
                    t if t <= 3600. * 5. => "b. 1-5h",
                    t if t <= 3600. * 15. => "c. 5-15h",
                    t if t <= 3600. * 40. => "d. 15-40h",
                    _ => "e. 40h+",
                };
                *this_month.entry(bucket).or_default() += 1.;
            }
            (*month, this_month)
        })
        .collect()
}

/// Waitlist statistics. Month keys are `"YYYY-MM"`; hull keys are hull names, with hulls under a
/// 1% share folded into `"Other"` (always present in hull series). All numbers are floats.
#[derive(Serialize, ToSchema)]
struct StatsResponse {
    /// Month → hull → seconds in fleet.
    #[schema(value_type = BTreeMap<String, BTreeMap<String, f64>>)]
    fleet_seconds_by_hull_by_month: BTreeMap<YearMonth, Series>,
    /// Month → hull → distinct pilots that x'd up with it.
    #[schema(value_type = BTreeMap<String, BTreeMap<String, f64>>)]
    xes_by_hull_by_month: BTreeMap<YearMonth, Series>,
    /// Month → total seconds in fleet (all pilots).
    #[schema(value_type = BTreeMap<String, f64>)]
    fleet_seconds_by_month: BTreeMap<YearMonth, f64>,
    /// Month → distinct pilots seen in fleet.
    #[schema(value_type = BTreeMap<String, f64>)]
    pilots_by_month: BTreeMap<YearMonth, f64>,
    /// Hull → distinct pilots that x'd up with it in the last 28 days.
    xes_by_hull_28d: Series,
    /// Hull → seconds in fleet in the last 28 days.
    fleet_seconds_by_hull_28d: Series,
    /// Hull → `{"X": share of x-ups, "Time": share of fleet time}` over the last 28 days. Hulls
    /// that are under 1% of x-ups count towards `"Other"` in both. A share is `null` when its
    /// total is zero (0/0, as the legacy waitlist sent it).
    #[schema(value_type = BTreeMap<String, BTreeMap<String, Option<f64>>>)]
    x_vs_time_by_hull_28d: BTreeMap<String, BTreeMap<&'static str, f64>>,
    /// Month → fleet-time bucket (`a. <1h`, `b. 1-5h`, `c. 5-15h`, `d. 15-40h`, `e. 40h+`) →
    /// pilots whose total fleet time that month falls in it. Empty buckets are omitted.
    #[schema(value_type = BTreeMap<String, BTreeMap<String, f64>>)]
    time_spent_in_fleet_by_month: BTreeMap<YearMonth, BTreeMap<&'static str, f64>>,
}

/// Waitlist statistics
///
/// Fleet time, pilots and x-ups per month and hull over all history, plus breakdowns of the last
/// 28 days, for charts. Hull series fold hulls under a 1% share into `"Other"`, which is always
/// present. Months are `"YYYY-MM"` in the database session's time zone. Requires `stats-view`.
/// A hull unknown to the static data fails the request with `400 type error: no matches for
/// item`.
#[utoipa::path(
    get,
    path = "/api/stats",
    tag = tags::HISTORY,
    security(("session" = ["stats-view"])),
    responses(
        (status = 200, description = "The statistics.", body = StatsResponse),
        (
            status = 400,
            description = "`type error: no matches for item` (a hull unknown to the static data).",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn statistics(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<StatsResponse>> {
    account.require_access("stats-view")?;
    let eve = state.eve_data()?;
    let db = state.db();
    let since = chrono::Utc::now().timestamp() - DAYS_28;

    let seconds_by_character_month = by_month(db, FLEET_SECONDS_BY_CHARACTER_BY_MONTH).await?;
    let seconds_by_hull_month = hull_keys(by_month(db, FLEET_SECONDS_BY_HULL_BY_MONTH).await?);
    let xes_by_hull_month = hull_keys(by_month(db, XES_BY_HULL_BY_MONTH).await?);
    let xes_by_hull_28d = by_hull_since(db, XES_BY_HULL_SINCE, since).await?;
    let seconds_by_hull_28d = by_hull_since(db, FLEET_SECONDS_BY_HULL_SINCE, since).await?;

    Ok(Json(StatsResponse {
        fleet_seconds_by_hull_by_month: filter_into_other_2d(
            translate_hulls_2d(eve, &seconds_by_hull_month)?,
            OTHER_THRESHOLD,
        ),
        xes_by_hull_by_month: filter_into_other_2d(
            translate_hulls_2d(eve, &xes_by_hull_month)?,
            OTHER_THRESHOLD,
        ),
        fleet_seconds_by_month: seconds_by_hull_month
            .iter()
            .map(|(month, values)| (*month, values.values().sum()))
            .collect(),
        pilots_by_month: seconds_by_character_month
            .iter()
            .map(|(month, values)| (*month, values.len() as f64))
            .collect(),
        xes_by_hull_28d: filter_into_other_1d(
            translate_hulls_1d(eve, &xes_by_hull_28d)?,
            OTHER_THRESHOLD,
        ),
        fleet_seconds_by_hull_28d: filter_into_other_1d(
            translate_hulls_1d(eve, &seconds_by_hull_28d)?,
            OTHER_THRESHOLD,
        ),
        x_vs_time_by_hull_28d: x_vs_time_by_hull(eve, &xes_by_hull_28d, &seconds_by_hull_28d)?,
        time_spent_in_fleet_by_month: time_spent_in_fleet_by_month(&seconds_by_character_month),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(items: &[(&str, f64)]) -> Series {
        items.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn small_series_fold_into_other() {
        let folded = filter_into_other_1d(series(&[("A", 990.), ("B", 10.)]), 0.01);
        assert_eq!(folded, series(&[("A", 990.), ("Other", 10.)]));
        assert_eq!(
            filter_into_other_1d(BTreeMap::new(), 0.01),
            series(&[("Other", 0.)])
        );
    }

    #[test]
    fn two_dimensional_folding_uses_overall_shares() {
        let source = BTreeMap::from([
            (YearMonth(2024, 1), series(&[("A", 500.), ("B", 5.)])),
            (YearMonth(2024, 2), series(&[("A", 495.), ("B", 0.)])),
        ]);
        let folded = filter_into_other_2d(source, 0.01);
        assert_eq!(
            folded[&YearMonth(2024, 1)],
            series(&[("A", 500.), ("Other", 5.)])
        );
        assert_eq!(
            serde_json::to_string(&YearMonth(2024, 7)).unwrap(),
            "\"2024-07\""
        );
    }

    #[test]
    fn fleet_time_buckets() {
        let source = BTreeMap::from([(
            YearMonth(2024, 1),
            BTreeMap::from([(1, 3600.), (2, 3601.), (3, 200_000.)]),
        )]);
        let buckets = time_spent_in_fleet_by_month(&source);
        assert_eq!(
            buckets[&YearMonth(2024, 1)],
            BTreeMap::from([("a. <1h", 1.), ("b. 1-5h", 1.), ("e. 40h+", 1.)])
        );
    }
}
