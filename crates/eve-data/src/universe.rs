//! The star map: regions, constellations, solar systems, the stargate graph and NPC stations.
//!
//! Built from the `map*`/`staStations` tables of the static data file (see [`crate::sde`]).
//! Files built before those tables existed (schema version 1, or a legacy
//! `sqlite-shrunk.sqlite`) load with an empty map: [`Universe::is_empty`] is then true and every
//! lookup misses.

use std::collections::{HashMap, VecDeque};

/// EVE solar system, constellation or region id.
pub type MapId = i64;

/// A region of the map.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub id: MapId,
    pub name: String,
}

/// A constellation and its systems.
#[derive(Debug, Clone, PartialEq)]
pub struct Constellation {
    pub id: MapId,
    pub name: String,
    pub region_id: MapId,
    /// Ids of the constellation's solar systems, ascending.
    pub systems: Vec<MapId>,
}

/// A solar system.
#[derive(Debug, Clone, PartialEq)]
pub struct SolarSystem {
    pub id: MapId,
    pub name: String,
    pub constellation_id: MapId,
    pub region_id: MapId,
    /// The unrounded security status (see [`display_security`]).
    pub security: f64,
    /// Distance in AU from the sun to the farthest planet, stargate or NPC station.
    pub radius_au: f64,
    /// Largest distance in AU between two of the sun, the planets, stargates and NPC stations:
    /// an approximation of the longest warp in the system.
    pub longest_warp_au: f64,
}

/// An NPC station.
#[derive(Debug, Clone, PartialEq)]
pub struct Station {
    pub id: i64,
    pub system_id: MapId,
    pub name: String,
    pub has_repair: bool,
    pub has_cloning: bool,
}

/// Security band of a system, by its displayed (rounded) security status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecurityBand {
    Highsec,
    Lowsec,
    Nullsec,
}

impl SecurityBand {
    pub const ALL: [SecurityBand; 3] = [Self::Highsec, Self::Lowsec, Self::Nullsec];

    /// The band of a system with this (unrounded) security status: high-sec from a displayed
    /// 0.5, low-sec from a displayed 0.1, null-sec at 0.0 and below.
    pub fn of(security: f64) -> Self {
        let shown = display_security(security);
        if shown >= 0.45 {
            Self::Highsec
        } else if shown > 0.0 {
            Self::Lowsec
        } else {
            Self::Nullsec
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Highsec => "highsec",
            Self::Lowsec => "lowsec",
            Self::Nullsec => "nullsec",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|band| band.as_str() == value)
    }
}

/// The security status as the game shows it: rounded to one decimal, except that a status
/// between 0.0 and 0.05 shows as 0.1 (it is still low-sec).
pub fn display_security(security: f64) -> f64 {
    if security > 0.0 && security < 0.05 {
        0.1
    } else {
        (security * 10.0).round() / 10.0
    }
}

/// The raw map tables a [`Universe`] is built from.
#[derive(Debug, Clone, Default)]
pub struct MapTables {
    pub regions: Vec<Region>,
    /// `systems` may be empty here: it is filled from the systems' constellation ids.
    pub constellations: Vec<Constellation>,
    pub systems: Vec<SolarSystem>,
    /// Stargate connections `(from, to)`; each gate is listed once per direction.
    pub jumps: Vec<(MapId, MapId)>,
    pub stations: Vec<Station>,
}

/// The in-memory star map.
#[derive(Debug, Default)]
pub struct Universe {
    regions: HashMap<MapId, Region>,
    constellations: HashMap<MapId, Constellation>,
    systems: HashMap<MapId, SolarSystem>,
    names: HashMap<String, MapId>,
    neighbours: HashMap<MapId, Vec<MapId>>,
    stations: HashMap<MapId, Vec<Station>>,
}

impl Universe {
    pub fn from_tables(tables: MapTables) -> Self {
        let mut constellations: HashMap<MapId, Constellation> = tables
            .constellations
            .into_iter()
            .map(|mut c| {
                c.systems.clear();
                (c.id, c)
            })
            .collect();
        let mut systems = HashMap::with_capacity(tables.systems.len());
        let mut names = HashMap::with_capacity(tables.systems.len());
        for system in tables.systems {
            if let Some(constellation) = constellations.get_mut(&system.constellation_id) {
                constellation.systems.push(system.id);
            }
            names.insert(system.name.to_lowercase(), system.id);
            systems.insert(system.id, system);
        }
        for constellation in constellations.values_mut() {
            constellation.systems.sort_unstable();
        }
        let mut neighbours: HashMap<MapId, Vec<MapId>> = HashMap::new();
        for (from, to) in tables.jumps {
            neighbours.entry(from).or_default().push(to);
        }
        for list in neighbours.values_mut() {
            list.sort_unstable();
            list.dedup();
        }
        let mut stations: HashMap<MapId, Vec<Station>> = HashMap::new();
        for station in tables.stations {
            stations.entry(station.system_id).or_default().push(station);
        }
        for list in stations.values_mut() {
            list.sort_by(|a, b| a.name.cmp(&b.name));
        }
        Self {
            regions: tables.regions.into_iter().map(|r| (r.id, r)).collect(),
            constellations,
            systems,
            names,
            neighbours,
            stations,
        }
    }

    /// True when the static data file has no map data (built before it was added).
    pub fn is_empty(&self) -> bool {
        self.systems.is_empty()
    }

    pub fn system_count(&self) -> usize {
        self.systems.len()
    }

    pub fn system(&self, id: MapId) -> Option<&SolarSystem> {
        self.systems.get(&id)
    }

    /// A system by name, ignoring case.
    pub fn system_by_name(&self, name: &str) -> Option<&SolarSystem> {
        self.names
            .get(&name.trim().to_lowercase())
            .and_then(|id| self.systems.get(id))
    }

    pub fn constellation(&self, id: MapId) -> Option<&Constellation> {
        self.constellations.get(&id)
    }

    pub fn region(&self, id: MapId) -> Option<&Region> {
        self.regions.get(&id)
    }

    /// Systems one stargate jump away.
    pub fn neighbours(&self, id: MapId) -> &[MapId] {
        self.neighbours.get(&id).map_or(&[], Vec::as_slice)
    }

    /// NPC stations in a system, by name.
    pub fn stations(&self, system_id: MapId) -> &[Station] {
        self.stations.get(&system_id).map_or(&[], Vec::as_slice)
    }

    /// Stargate jumps from `from` to every system reachable through systems accepted by
    /// `allowed` (the start is always allowed). Breadth-first search over the stargate graph.
    pub fn jumps_from(&self, from: MapId, allowed: impl Fn(&SolarSystem) -> bool) -> Jumps {
        let mut distances = HashMap::new();
        if !self.systems.contains_key(&from) {
            return Jumps { distances };
        }
        distances.insert(from, 0u32);
        let mut queue = VecDeque::from([from]);
        while let Some(current) = queue.pop_front() {
            let next = distances[&current] + 1;
            for &neighbour in self.neighbours(current) {
                if distances.contains_key(&neighbour) {
                    continue;
                }
                if !self.systems.get(&neighbour).is_some_and(&allowed) {
                    continue;
                }
                distances.insert(neighbour, next);
                queue.push_back(neighbour);
            }
        }
        Jumps { distances }
    }
}

/// Shortest jump counts from one system (see [`Universe::jumps_from`]).
#[derive(Debug, Clone, Default)]
pub struct Jumps {
    distances: HashMap<MapId, u32>,
}

impl Jumps {
    /// Jumps to `to`, or `None` when it can't be reached.
    pub fn to(&self, to: MapId) -> Option<u32> {
        self.distances.get(&to).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system(id: MapId, constellation_id: MapId, security: f64) -> SolarSystem {
        SolarSystem {
            id,
            name: format!("S{id}"),
            constellation_id,
            region_id: 1,
            security,
            radius_au: 1.0,
            longest_warp_au: 2.0,
        }
    }

    #[test]
    fn security_is_displayed_like_the_game() {
        assert_eq!(display_security(0.449), 0.4);
        assert_eq!(display_security(0.45), 0.5);
        assert_eq!(display_security(0.01), 0.1);
        assert_eq!(display_security(0.0), 0.0);
        assert_eq!(display_security(-0.04), -0.0);
        assert_eq!(SecurityBand::of(0.46), SecurityBand::Highsec);
        assert_eq!(SecurityBand::of(0.44), SecurityBand::Lowsec);
        assert_eq!(SecurityBand::of(0.01), SecurityBand::Lowsec);
        assert_eq!(SecurityBand::of(0.0), SecurityBand::Nullsec);
        assert_eq!(SecurityBand::of(-0.7), SecurityBand::Nullsec);
        assert_eq!(SecurityBand::parse("lowsec"), Some(SecurityBand::Lowsec));
    }

    #[test]
    fn finds_shortest_and_restricted_routes() {
        // 1 - 2 - 3 - 4 and a low-sec shortcut 1 - 5 - 4.
        let universe = Universe::from_tables(MapTables {
            regions: vec![Region {
                id: 1,
                name: "R".into(),
            }],
            constellations: vec![Constellation {
                id: 10,
                name: "C".into(),
                region_id: 1,
                systems: vec![],
            }],
            systems: vec![
                system(1, 10, 0.9),
                system(2, 10, 0.8),
                system(3, 10, 0.7),
                system(4, 10, 0.6),
                system(5, 10, 0.2),
            ],
            jumps: [(1, 2), (2, 3), (3, 4), (1, 5), (5, 4)]
                .into_iter()
                .flat_map(|(a, b)| [(a, b), (b, a)])
                .collect(),
            stations: vec![],
        });
        assert_eq!(universe.constellation(10).unwrap().systems, [1, 2, 3, 4, 5]);
        assert_eq!(universe.system_by_name(" s3 ").unwrap().id, 3);
        assert_eq!(universe.jumps_from(1, |_| true).to(4), Some(2));
        let highsec =
            universe.jumps_from(1, |s| SecurityBand::of(s.security) == SecurityBand::Highsec);
        assert_eq!(highsec.to(4), Some(3));
        assert_eq!(highsec.to(5), None);
        assert_eq!(universe.jumps_from(99, |_| true).to(99), None);
    }
}
