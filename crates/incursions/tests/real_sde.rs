//! Enrichment against the real static data file (`deploy/sde/sde.sqlite` or `WL_SDE_PATH`):
//! the star map, jumps, stations and role estimates. Skipped when the file is missing or was
//! built without the star map.

use std::path::PathBuf;

use wl_eve_data::{EveData, SecurityBand};
use wl_incursions::roles::{self, Role, RoleSource};
use wl_incursions::view::Context;

async fn eve() -> Option<EveData> {
    let path = std::env::var_os("WL_SDE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/sde/sde.sqlite")
        });
    if !path.exists() {
        eprintln!("SKIPPED: static data file {} not found", path.display());
        return None;
    }
    let eve = EveData::load(&path).await.expect("static data file loads");
    if eve.universe().is_empty() {
        eprintln!("SKIPPED: {} has no star map; rebuild it", path.display());
        return None;
    }
    Some(eve)
}

#[tokio::test]
async fn star_map_is_complete() {
    let Some(eve) = eve().await else { return };
    let map = eve.universe();
    assert!(map.system_count() > 8000, "{}", map.system_count());

    let jita = map.system_by_name("jita").expect("Jita");
    assert_eq!(jita.id, 30000142);
    assert_eq!(SecurityBand::of(jita.security), SecurityBand::Highsec);
    let perimeter = map.system_by_name("Perimeter").unwrap();
    assert!(map.neighbours(jita.id).contains(&perimeter.id));
    assert_eq!(map.jumps_from(jita.id, |_| true).to(perimeter.id), Some(1));
    // A system is roughly a few to a few hundred AU across.
    assert!(
        jita.radius_au > 5.0 && jita.radius_au < 200.0,
        "{}",
        jita.radius_au
    );
    assert!(jita.longest_warp_au >= jita.radius_au);

    let forge = map.region(jita.region_id).unwrap();
    assert_eq!(forge.name, "The Forge");
    let kimotoro = map.constellation(jita.constellation_id).unwrap();
    assert_eq!(kimotoro.name, "Kimotoro");
    assert!(kimotoro.systems.contains(&jita.id));

    let stations = map.stations(jita.id);
    let assembly = stations
        .iter()
        .find(|s| s.id == 60003760)
        .expect("Jita IV - Moon 4 - Caldari Navy Assembly Plant");
    assert_eq!(
        assembly.name,
        "Jita IV - Moon 4 - Caldari Navy Assembly Plant"
    );
    assert!(assembly.has_repair);
    assert!(assembly.has_cloning);
}

#[tokio::test]
async fn highsec_only_routes_are_never_shorter() {
    let Some(eve) = eve().await else { return };
    let map = eve.universe();
    let ctx_names = [
        "Jita".to_string(),
        "Amarr".to_string(),
        "Nowhere".to_string(),
    ];
    let ctx = Context::new(map, &ctx_names, 0);
    assert!(ctx.now() == 0);
    let jita = map.system_by_name("Jita").unwrap().id;
    let amarr = map.system_by_name("Amarr").unwrap().id;
    let shortest = map.jumps_from(jita, |_| true).to(amarr).unwrap();
    let safe = map
        .jumps_from(jita, |s| {
            SecurityBand::of(s.security) == SecurityBand::Highsec
        })
        .to(amarr)
        .unwrap();
    assert!(shortest >= 5 && safe >= shortest, "{shortest} {safe}");
}

/// The graph estimate against the recorded layouts it falls back from.
#[tokio::test]
async fn role_estimates_match_most_known_layouts() {
    let Some(eve) = eve().await else { return };
    let map = eve.universe();
    let (mut right, mut total, mut hq_right, mut layouts) = (0, 0, 0, 0);
    for layout in roles::known_layouts().values() {
        if layout.headquarters.is_empty() {
            continue;
        }
        layouts += 1;
        let mut infested = vec![layout.staging];
        infested.extend(&layout.vanguard);
        infested.extend(&layout.assault);
        infested.extend(&layout.headquarters);
        // Pretend the layout is unknown: use a constellation id with no layout.
        for (id, role, source) in roles::assign(-1, layout.staging, &infested, map) {
            let expected = if id == layout.staging {
                Role::Staging
            } else if layout.vanguard.contains(&id) {
                Role::Vanguard
            } else if layout.assault.contains(&id) {
                Role::Assault
            } else {
                Role::Headquarters
            };
            if id != layout.staging {
                assert_eq!(source, RoleSource::Estimated);
            }
            total += 1;
            if role == expected {
                right += 1;
                if role == Role::Headquarters {
                    hq_right += 1;
                }
            }
        }
    }
    let share = f64::from(right) / f64::from(total);
    let hq_share = f64::from(hq_right) / f64::from(layouts);
    eprintln!("role estimates: {share:.3} of systems, {hq_share:.3} of headquarters");
    assert!(share > 0.75, "{share}");
    assert!(hq_share > 0.6, "{hq_share}");
}

#[tokio::test]
async fn known_layout_wins_over_the_estimate() {
    let Some(eve) = eve().await else { return };
    // Claysson's constellation (20000396) has a recorded layout.
    let layout = &roles::known_layouts()[&20000396];
    let mut infested = vec![layout.staging];
    infested.extend(&layout.vanguard);
    infested.extend(&layout.assault);
    infested.extend(&layout.headquarters);
    let assigned = roles::assign(20000396, layout.staging, &infested, eve.universe());
    assert!(assigned.iter().all(|(_, _, s)| *s == RoleSource::Known));
    let hq = assigned
        .iter()
        .find(|(_, role, _)| *role == Role::Headquarters)
        .unwrap();
    assert_eq!(hq.0, layout.headquarters[0]);
}
