//! Reading the star map from the SDE: regions, constellations, systems (with their size),
//! stargates and NPC stations (with their in-game names).
//!
//! Every map file is optional: an archive without `mapSolarSystems.jsonl` gives an empty map.

use std::collections::{HashMap, HashSet};
use std::fs::File;

use serde::Deserialize;

use super::SdeError;
use super::extract::{entry_name, for_each_line};
use crate::universe::{Constellation, MapId, MapTables, Region, SolarSystem, Station};

const REGIONS_FILE: &str = "mapRegions.jsonl";
const CONSTELLATIONS_FILE: &str = "mapConstellations.jsonl";
const SYSTEMS_FILE: &str = "mapSolarSystems.jsonl";
const STARGATES_FILE: &str = "mapStargates.jsonl";
const PLANETS_FILE: &str = "mapPlanets.jsonl";
const STATIONS_FILE: &str = "npcStations.jsonl";
const CORPORATIONS_FILE: &str = "npcCorporations.jsonl";
const OPERATIONS_FILE: &str = "stationOperations.jsonl";

/// `stationServices` ids.
const SERVICE_CLONING: i32 = 10;
const SERVICE_REPAIR: i32 = 13;

/// Metres per astronomical unit.
const AU: f64 = 149_597_870_700.0;

#[derive(Deserialize)]
struct Name {
    #[serde(default)]
    en: Option<String>,
}

fn english(name: Option<Name>, id: i64) -> String {
    name.and_then(|n| n.en).unwrap_or_else(|| id.to_string())
}

#[derive(Deserialize, Clone, Copy)]
struct Position {
    x: f64,
    y: f64,
    z: f64,
}

impl Position {
    fn distance(&self, other: &Position) -> f64 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2) + (self.z - other.z).powi(2))
            .sqrt()
    }
}

const ORIGIN: Position = Position {
    x: 0.0,
    y: 0.0,
    z: 0.0,
};

#[derive(Deserialize)]
struct RegionLine {
    #[serde(rename = "_key")]
    key: MapId,
    #[serde(default)]
    name: Option<Name>,
}

#[derive(Deserialize)]
struct ConstellationLine {
    #[serde(rename = "_key")]
    key: MapId,
    #[serde(default)]
    name: Option<Name>,
    #[serde(rename = "regionID")]
    region_id: MapId,
}

#[derive(Deserialize)]
struct SystemLine {
    #[serde(rename = "_key")]
    key: MapId,
    #[serde(default)]
    name: Option<Name>,
    #[serde(rename = "constellationID")]
    constellation_id: MapId,
    #[serde(rename = "regionID")]
    region_id: MapId,
    #[serde(default, rename = "securityStatus")]
    security_status: f64,
    #[serde(default, rename = "planetIDs")]
    planet_ids: Vec<i64>,
}

#[derive(Deserialize)]
struct Destination {
    #[serde(rename = "solarSystemID")]
    solar_system_id: MapId,
}

#[derive(Deserialize)]
struct StargateLine {
    #[serde(rename = "solarSystemID")]
    solar_system_id: MapId,
    destination: Destination,
    position: Position,
}

#[derive(Deserialize)]
struct CelestialLine {
    #[serde(rename = "solarSystemID")]
    solar_system_id: MapId,
    position: Position,
}

#[derive(Deserialize)]
struct StationLine {
    #[serde(rename = "_key")]
    key: i64,
    #[serde(rename = "solarSystemID")]
    solar_system_id: MapId,
    #[serde(default, rename = "orbitID")]
    orbit_id: Option<i64>,
    #[serde(default, rename = "celestialIndex")]
    celestial_index: Option<u32>,
    #[serde(default, rename = "orbitIndex")]
    orbit_index: Option<u32>,
    #[serde(rename = "ownerID")]
    owner_id: i64,
    #[serde(default, rename = "operationID")]
    operation_id: Option<i32>,
    #[serde(default, rename = "useOperationName")]
    use_operation_name: bool,
    position: Position,
}

#[derive(Deserialize)]
struct CorporationLine {
    #[serde(rename = "_key")]
    key: i64,
    #[serde(default)]
    name: Option<Name>,
}

#[derive(Deserialize)]
struct OperationLine {
    #[serde(rename = "_key")]
    key: i32,
    #[serde(default, rename = "operationName")]
    operation_name: Option<Name>,
    #[serde(default)]
    services: Vec<i32>,
}

/// Lines of `file` when the archive has it.
fn optional<T: serde::de::DeserializeOwned>(
    archive: &mut zip::ZipArchive<File>,
    file: &'static str,
    mut handle: impl FnMut(T),
) -> Result<bool, SdeError> {
    if entry_name(archive, file).is_none() {
        tracing::warn!(file, "the SDE archive has no {file}");
        return Ok(false);
    }
    for_each_line::<T>(archive, file, |line, _| {
        handle(line);
        Ok(())
    })?;
    Ok(true)
}

pub(super) fn extract(archive: &mut zip::ZipArchive<File>) -> Result<MapTables, SdeError> {
    let mut map = MapTables::default();
    if entry_name(archive, SYSTEMS_FILE).is_none() {
        tracing::warn!("the SDE archive has no map data; the star map stays empty");
        return Ok(map);
    }

    optional::<RegionLine>(archive, REGIONS_FILE, |line| {
        map.regions.push(Region {
            id: line.key,
            name: english(line.name, line.key),
        });
    })?;
    optional::<ConstellationLine>(archive, CONSTELLATIONS_FILE, |line| {
        map.constellations.push(Constellation {
            id: line.key,
            name: english(line.name, line.key),
            region_id: line.region_id,
            systems: Vec::new(),
        });
    })?;

    let mut planets = HashSet::new();
    let mut names = HashMap::new();
    optional::<SystemLine>(archive, SYSTEMS_FILE, |line| {
        planets.extend(line.planet_ids);
        let name = english(line.name, line.key);
        names.insert(line.key, name.clone());
        map.systems.push(SolarSystem {
            id: line.key,
            name,
            constellation_id: line.constellation_id,
            region_id: line.region_id,
            security: line.security_status,
            radius_au: 0.0,
            longest_warp_au: 0.0,
        });
    })?;

    // Warpable celestials per system, for its size. Moons and belts are left out: they sit next
    // to their planet and don't change the result noticeably.
    let mut celestials: HashMap<MapId, Vec<Position>> = HashMap::new();
    optional::<StargateLine>(archive, STARGATES_FILE, |line| {
        map.jumps
            .push((line.solar_system_id, line.destination.solar_system_id));
        celestials
            .entry(line.solar_system_id)
            .or_default()
            .push(line.position);
    })?;
    optional::<CelestialLine>(archive, PLANETS_FILE, |line| {
        celestials
            .entry(line.solar_system_id)
            .or_default()
            .push(line.position);
    })?;

    let mut corporations = HashMap::new();
    optional::<CorporationLine>(archive, CORPORATIONS_FILE, |line| {
        corporations.insert(line.key, english(line.name, line.key));
    })?;
    let mut operations = HashMap::new();
    optional::<OperationLine>(archive, OPERATIONS_FILE, |line| {
        operations.insert(
            line.key,
            (
                english(line.operation_name, i64::from(line.key)),
                line.services,
            ),
        );
    })?;
    optional::<StationLine>(archive, STATIONS_FILE, |line| {
        celestials
            .entry(line.solar_system_id)
            .or_default()
            .push(line.position);
        let operation = line.operation_id.and_then(|id| operations.get(&id));
        let services = operation.map_or(&[][..], |(_, services)| services.as_slice());
        map.stations.push(Station {
            id: line.key,
            system_id: line.solar_system_id,
            name: station_name(&line, &names, &planets, &corporations, operation),
            has_repair: services.contains(&SERVICE_REPAIR),
            has_cloning: services.contains(&SERVICE_CLONING),
        });
    })?;

    for system in &mut map.systems {
        let points = celestials.remove(&system.id).unwrap_or_default();
        let (radius, longest) = system_size(&points);
        system.radius_au = radius / AU;
        system.longest_warp_au = longest / AU;
    }
    Ok(map)
}

/// `(farthest celestial from the sun, longest distance between two celestials or the sun)`, in
/// metres.
fn system_size(points: &[Position]) -> (f64, f64) {
    let radius = points
        .iter()
        .map(|p| p.distance(&ORIGIN))
        .fold(0.0, f64::max);
    let mut longest = radius;
    for (i, a) in points.iter().enumerate() {
        for b in &points[i + 1..] {
            longest = longest.max(a.distance(b));
        }
    }
    (radius, longest)
}

/// The in-game station name: `<System> <planet numeral>[ - Moon <n>] - <owner>[ <operation>]`,
/// e.g. `Muvolailen X - Moon 3 - CBD Corporation Storage`.
fn station_name(
    line: &StationLine,
    systems: &HashMap<MapId, String>,
    planets: &HashSet<i64>,
    corporations: &HashMap<i64, String>,
    operation: Option<&(String, Vec<i32>)>,
) -> String {
    let mut name = systems
        .get(&line.solar_system_id)
        .cloned()
        .unwrap_or_else(|| line.solar_system_id.to_string());
    if let Some(index) = line.celestial_index {
        name.push(' ');
        name.push_str(&roman(index));
    }
    let orbits_moon = line.orbit_id.is_some_and(|orbit| !planets.contains(&orbit));
    if let (true, Some(moon)) = (orbits_moon, line.orbit_index) {
        name.push_str(&format!(" - Moon {moon}"));
    }
    name.push_str(" - ");
    name.push_str(
        corporations
            .get(&line.owner_id)
            .map_or("Unknown Corporation", String::as_str),
    );
    if line.use_operation_name
        && let Some((operation, _)) = operation
    {
        name.push(' ');
        name.push_str(operation);
    }
    name
}

fn roman(mut n: u32) -> String {
    const NUMERALS: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (value, numeral) in NUMERALS {
        while n >= value {
            out.push_str(numeral);
            n -= value;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roman_numerals() {
        assert_eq!(roman(4), "IV");
        assert_eq!(roman(9), "IX");
        assert_eq!(roman(14), "XIV");
        assert_eq!(roman(1), "I");
    }

    #[test]
    fn system_size_is_the_farthest_and_longest_distance() {
        let p = |x| Position { x, y: 0.0, z: 0.0 };
        assert_eq!(system_size(&[p(3.0), p(-5.0)]), (5.0, 8.0));
        assert_eq!(system_size(&[]), (0.0, 0.0));
    }
}
