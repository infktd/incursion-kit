//! Incursions and the map (public endpoints).

use serde::{Deserialize, Serialize};

use chrono::{DateTime, Utc};

use crate::error::{self, ErrorStyle, EsiResult};
use crate::esi::{Esi, send};
use crate::util::enum_str;

/// An active incursion (`GET /incursions`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Incursion {
    pub constellation_id: i64,
    pub faction_id: i64,
    pub has_boss: bool,
    pub infested_solar_systems: Vec<i64>,
    pub influence: f64,
    pub staging_solar_system_id: i64,
    /// `mobilizing`, `established` or `withdrawing`.
    pub state: String,
    #[serde(rename = "type")]
    pub type_: String,
}

/// `GET /incursions` with the response's caching headers.
#[derive(Debug, Clone, PartialEq)]
pub struct IncursionsSnapshot {
    pub incursions: Vec<Incursion>,
    /// When ESI will have new data (`Expires`); polling earlier only hits the cache.
    pub expires: Option<DateTime<Utc>>,
    /// When ESI generated this data (`Last-Modified`).
    pub last_modified: Option<DateTime<Utc>>,
}

/// An HTTP date header (`Sun, 11 Oct 2026 06:35:20 GMT`).
fn http_date(
    headers: &reqwest::header::HeaderMap,
    name: reqwest::header::HeaderName,
) -> Option<DateTime<Utc>> {
    let value = headers.get(name)?.to_str().ok()?;
    DateTime::parse_from_rfc2822(value.trim())
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn incursion(i: eve_esi_client::types::IncursionsGetItem) -> Incursion {
    Incursion {
        constellation_id: i.constellation_id,
        faction_id: i.faction_id,
        has_boss: i.has_boss,
        infested_solar_systems: i.infested_solar_systems,
        influence: i.influence,
        staging_solar_system_id: i.staging_solar_system_id,
        state: enum_str(&i.state),
        type_: i.type_,
    }
}

/// `GET /universe/constellations/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConstellationInfo {
    pub constellation_id: i64,
    pub name: String,
    pub region_id: i64,
    pub systems: Vec<i64>,
}

/// `GET /universe/systems/{id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SolarSystemInfo {
    pub constellation_id: i64,
    pub name: String,
    pub planets: Vec<Planet>,
    pub position: Position,
    pub security_class: Option<String>,
    pub security_status: f64,
    pub star_id: Option<i64>,
    pub stargates: Vec<i64>,
    pub stations: Vec<i64>,
    pub system_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Planet {
    pub planet_id: i64,
    pub asteroid_belts: Vec<i64>,
    pub moons: Vec<i64>,
}

/// A position in space (metres).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Position {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Esi {
    /// Every active incursion.
    pub async fn incursions(&self) -> EsiResult<Vec<Incursion>> {
        let incursions = send(self.client().get_incursions().send(), ErrorStyle::Status).await?;
        Ok(incursions.0.into_iter().map(incursion).collect())
    }

    /// Every active incursion, with when ESI generated the list and when it will next change.
    /// Like every GET, a request before `expires` is answered from the client's cache.
    pub async fn incursions_snapshot(&self) -> EsiResult<IncursionsSnapshot> {
        let response = self
            .client()
            .get_incursions()
            .send()
            .await
            .map_err(|e| error::from_client(e, ErrorStyle::Status))?;
        let expires = http_date(response.headers(), reqwest::header::EXPIRES);
        let last_modified = http_date(response.headers(), reqwest::header::LAST_MODIFIED);
        Ok(IncursionsSnapshot {
            incursions: response.into_inner().0.into_iter().map(incursion).collect(),
            expires,
            last_modified,
        })
    }

    pub async fn constellation(&self, constellation_id: i64) -> EsiResult<ConstellationInfo> {
        let c = send(
            self.client()
                .get_universe_constellations_constellation_id()
                .constellation_id(constellation_id)
                .send(),
            ErrorStyle::Status,
        )
        .await?;
        Ok(ConstellationInfo {
            constellation_id: c.constellation_id,
            name: c.name,
            region_id: c.region_id,
            systems: c.systems,
        })
    }

    pub async fn solar_system(&self, system_id: i64) -> EsiResult<SolarSystemInfo> {
        let s = send(
            self.client()
                .get_universe_systems_system_id()
                .system_id(system_id)
                .send(),
            ErrorStyle::Status,
        )
        .await?;
        Ok(SolarSystemInfo {
            constellation_id: s.constellation_id,
            name: s.name,
            planets: s
                .planets
                .into_iter()
                .map(|p| Planet {
                    planet_id: p.planet_id,
                    asteroid_belts: p.asteroid_belts,
                    moons: p.moons,
                })
                .collect(),
            position: Position {
                x: s.position.x,
                y: s.position.y,
                z: s.position.z,
            },
            security_class: s.security_class,
            security_status: s.security_status,
            star_id: s.star_id,
            stargates: s.stargates,
            stations: s.stations,
            system_id: s.system_id,
        })
    }
}
