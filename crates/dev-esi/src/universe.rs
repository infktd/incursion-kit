//! Static data for the universe endpoints, from the waitlist's static data file
//! (`deploy/sde/sde.sqlite`, built by `waitlist sde`). Optional: without it, types are only known
//! from the seed's `names` and constellations/systems are 404s.

use std::collections::HashMap;
use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};

/// An inventory type.
#[derive(Debug, Clone)]
pub struct TypeInfo {
    pub name: String,
    pub group_id: i64,
    pub published: bool,
    /// Skill rank (training time multiplier), for skills.
    pub rank: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct ConstellationInfo {
    pub name: String,
    pub region_id: i64,
    pub systems: Vec<i64>,
}

#[derive(Debug, Clone)]
pub struct SystemInfo {
    pub name: String,
    pub constellation_id: i64,
    pub security: f64,
    pub stations: Vec<i64>,
}

/// The static data dev-esi serves, loaded into memory once.
#[derive(Debug, Default)]
pub struct Universe {
    pub types: HashMap<i64, TypeInfo>,
    pub regions: HashMap<i64, String>,
    pub constellations: HashMap<i64, ConstellationInfo>,
    pub systems: HashMap<i64, SystemInfo>,
    pub stations: HashMap<i64, String>,
}

/// The skill rank attribute (`skillTimeConstant`).
const RANK_ATTRIBUTE: i64 = 275;

impl Universe {
    /// Load the static data file.
    pub async fn load(path: &Path) -> Result<Self, sqlx::Error> {
        let pool = SqlitePool::connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .read_only(true)
                .immutable(true),
        )
        .await?;
        let mut universe = Universe::default();

        let ranks: HashMap<i64, f64> = sqlx::query_as::<_, (i64, f64)>(
            "SELECT typeID, CAST(COALESCE(valueFloat, valueInt) AS REAL) FROM dgmTypeAttributes WHERE attributeID = ?",
        )
        .bind(RANK_ATTRIBUTE)
        .fetch_all(&pool)
        .await?
        .into_iter()
        .collect();
        for (id, name, group_id, published) in sqlx::query_as::<_, (i64, String, i64, i64)>(
            "SELECT typeID, typeName, groupID, published FROM invTypes",
        )
        .fetch_all(&pool)
        .await?
        {
            universe.types.insert(
                id,
                TypeInfo {
                    name,
                    group_id,
                    published: published != 0,
                    rank: ranks.get(&id).copied(),
                },
            );
        }
        universe.regions = sqlx::query_as("SELECT regionID, regionName FROM mapRegions")
            .fetch_all(&pool)
            .await?
            .into_iter()
            .collect();
        for (id, name, region_id) in sqlx::query_as::<_, (i64, String, i64)>(
            "SELECT constellationID, constellationName, regionID FROM mapConstellations",
        )
        .fetch_all(&pool)
        .await?
        {
            universe.constellations.insert(
                id,
                ConstellationInfo {
                    name,
                    region_id,
                    systems: Vec::new(),
                },
            );
        }
        for (id, name, constellation_id, security) in sqlx::query_as::<_, (i64, String, i64, f64)>(
            "SELECT solarSystemID, solarSystemName, constellationID, CAST(security AS REAL) FROM mapSolarSystems \
             ORDER BY solarSystemID",
        )
        .fetch_all(&pool)
        .await?
        {
            if let Some(constellation) = universe.constellations.get_mut(&constellation_id) {
                constellation.systems.push(id);
            }
            universe.systems.insert(
                id,
                SystemInfo {
                    name,
                    constellation_id,
                    security,
                    stations: Vec::new(),
                },
            );
        }
        for (id, system_id, name) in sqlx::query_as::<_, (i64, i64, String)>(
            "SELECT stationID, solarSystemID, stationName FROM staStations ORDER BY stationID",
        )
        .fetch_all(&pool)
        .await?
        {
            if let Some(system) = universe.systems.get_mut(&system_id) {
                system.stations.push(id);
            }
            universe.stations.insert(id, name);
        }
        pool.close().await;
        Ok(universe)
    }

    /// The name and `POST /universe/names` category of a static ID.
    pub fn name(&self, id: i64) -> Option<(&str, &'static str)> {
        if let Some(t) = self.types.get(&id) {
            return Some((&t.name, "inventory_type"));
        }
        if let Some(s) = self.systems.get(&id) {
            return Some((&s.name, "solar_system"));
        }
        if let Some(c) = self.constellations.get(&id) {
            return Some((&c.name, "constellation"));
        }
        if let Some(r) = self.regions.get(&id) {
            return Some((r, "region"));
        }
        self.stations.get(&id).map(|s| (s.as_str(), "station"))
    }
}
