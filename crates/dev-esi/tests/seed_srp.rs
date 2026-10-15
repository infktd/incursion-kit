//! The seed's SRP deposits buy coverage that is active whenever the seed is (re)loaded, and that
//! covers the depositors' seeded losses, at any time of day and for any daily reset hour. A
//! deposit of a fixed age (`-6h`) had run out for six hours a day, so tests and demos built on
//! the seed depended on the clock.

use chrono::{DateTime, Duration, Utc};
use dev_esi::seed::{DEFAULT_SEED, SeedTime};
use serde_json::Value;
use wl_core::config::SrpConfig;
use wl_srp::payments::deposit_character_name;
use wl_srp::tiers::coverage_for;

/// A seeded deposit, with its date as written (relative times unresolved).
#[derive(Debug)]
struct Deposit {
    date: String,
    amount: f64,
    payer: i64,
    description: String,
}

/// A seeded loss: `(killmail id, victim, time as written)`.
type Loss = (i64, i64, String);

fn seed() -> Value {
    let options = serde_saphyr::options! {
        merge_keys: serde_saphyr::MergeKeyPolicy::Merge,
    };
    serde_saphyr::from_str_with_options(DEFAULT_SEED, options).unwrap()
}

/// Every deposit in the seed's wallet journals.
fn seeded_deposits(seed: &Value) -> Vec<Deposit> {
    seed["corporations"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|corporation| corporation["journal"].as_array().into_iter().flatten())
        .filter_map(|entry| {
            let amount = entry["amount"].as_f64().filter(|amount| *amount > 0.0)?;
            let description = entry["description"].as_str()?;
            deposit_character_name(description)?;
            Some(Deposit {
                date: entry["date"].as_str().unwrap_or("now").to_string(),
                amount,
                payer: entry["first_party_id"].as_i64()?,
                description: description.to_string(),
            })
        })
        .collect()
}

/// The seeded killmails whose victim made a seeded deposit.
fn covered_losses(seed: &Value, deposits: &[Deposit]) -> Vec<Loss> {
    seed["killmails"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|km| {
            let victim = km["victim"]["character_id"].as_i64()?;
            deposits.iter().any(|d| d.payer == victim).then(|| {
                (
                    km["killmail_id"].as_i64().unwrap(),
                    victim,
                    km["killmail_time"].as_str().unwrap_or("now").to_string(),
                )
            })
        })
        .collect()
}

#[test]
fn seeded_srp_coverage_holds_at_any_time_of_day() {
    let seed = seed();
    let deposits = seeded_deposits(&seed);
    assert!(deposits.len() >= 2, "{deposits:?}");
    let losses = covered_losses(&seed, &deposits);
    assert!(!losses.is_empty(), "a seeded loss of a depositor");
    let day: DateTime<Utc> = "2026-03-01T00:00:00Z".parse().unwrap();
    for reset_hour in 0..24 {
        let config = SrpConfig {
            daily_reset_hour_utc: reset_hour,
            ..SrpConfig::default()
        };
        // The seed loaded (dev-esi started or reset) at every minute of the day.
        for minute in 0..24 * 60 {
            let loaded = day + Duration::minutes(minute);
            let resolve = |date: &str| SeedTime::parse_at(date, SeedTime(loaded)).unwrap().0;
            // `(payer, paid, ends)` of each deposit.
            let mut coverage = Vec::new();
            for deposit in &deposits {
                let paid = resolve(&deposit.date);
                let bought = coverage_for(&config, deposit.amount, paid, false, 0.5)
                    .unwrap_or_else(|| panic!("{deposit:?} buys no coverage"));
                // Active when the seed loads (the reconciliation runs after that).
                assert!(
                    bought.expires_at > loaded,
                    "{} (date {}): coverage paid at {paid} has run out when the seed loads at \
                     {loaded} (ends {}, daily reset {reset_hour}:00 UTC)",
                    deposit.description,
                    deposit.date,
                    bought.expires_at
                );
                coverage.push((deposit.payer, paid, bought.expires_at));
            }
            // Each depositor's loss is covered at its time (as `coverage_at` checks it:
            // paid <= time <= ends).
            for (killmail_id, victim, time) in &losses {
                let died = resolve(time);
                assert!(
                    coverage.iter().any(|(payer, paid, ends)| payer == victim
                        && *paid <= died
                        && died <= *ends),
                    "killmail {killmail_id} (time {time}, at {died}) is not covered when the seed \
                     loads at {loaded} (daily reset {reset_hour}:00 UTC): {coverage:?}"
                );
            }
        }
    }
}

#[test]
fn seeded_dates_resolve_against_the_load_time() {
    let loaded = SeedTime("2026-03-01T10:59:59Z".parse().unwrap());
    assert_eq!(SeedTime::parse_at("now", loaded), Some(loaded));
    assert_eq!(
        SeedTime::parse_at("-6h", loaded).unwrap().esi(),
        "2026-03-01T04:59:59Z"
    );
}

/// Dates written the same (`now`) resolve in file order: the deposit, read first, is never after
/// the loss it covers.
#[test]
fn a_deposit_and_a_loss_at_the_same_time_stay_in_order() {
    let seed = dev_esi::SeedSource::default_seed().parse().unwrap();
    for km in &seed.killmails {
        let Some(victim) = km.victim.character_id else {
            continue;
        };
        for deposit in seed
            .corporations
            .iter()
            .flat_map(|c| &c.journal)
            .filter(|e| e.first_party_id == Some(victim) && e.amount.is_some_and(|a| a > 0.0))
        {
            assert!(deposit.date <= km.killmail_time, "{deposit:?} {km:?}");
        }
    }
}
