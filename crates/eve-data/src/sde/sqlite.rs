//! The SQLite layout of the static data file: writing it and reading it back.

use std::path::Path;

use sqlx::{
    Connection, QueryBuilder, Sqlite, SqliteConnection,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous},
};

use super::{SdeError, SdeInfo, extract::Extracted, io_error};
use crate::universe::{Constellation, MapTables, Region, SolarSystem, Station};
use crate::{AttributeId, AttributeRow, EffectId, EffectRow, MetaRow, Tables, TypeRow};

/// Bumped when the layout changes incompatibly.
const SCHEMA_VERSION: &str = "2";

/// The schema version files are built with: 2 added the star map tables.
pub(crate) const CURRENT_SCHEMA_VERSION: u32 = 2;

/// Same tables and columns as the legacy waitlist's `sqlite-shrunk.sqlite`, plus `sdeInfo` and
/// the star map (`map*`, `staStations`).
const SCHEMA: [&str; 11] = [
    "CREATE TABLE invTypes (
        typeID INTEGER PRIMARY KEY,
        typeName TEXT NOT NULL,
        groupID INTEGER NOT NULL,
        published INTEGER NOT NULL
    )",
    "CREATE TABLE invGroups (
        groupID INTEGER PRIMARY KEY,
        categoryID INTEGER NOT NULL
    )",
    "CREATE TABLE invMetaTypes (
        typeID INTEGER NOT NULL,
        parentTypeID INTEGER,
        metaGroupID INTEGER
    )",
    "CREATE TABLE dgmTypeAttributes (
        typeID INTEGER NOT NULL,
        attributeID INTEGER NOT NULL,
        valueInt INTEGER,
        valueFloat REAL,
        PRIMARY KEY (typeID, attributeID)
    )",
    "CREATE TABLE dgmTypeEffects (
        typeID INTEGER NOT NULL,
        effectID INTEGER NOT NULL,
        PRIMARY KEY (typeID, effectID)
    )",
    "CREATE TABLE sdeInfo (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    )",
    "CREATE TABLE mapRegions (
        regionID INTEGER PRIMARY KEY,
        regionName TEXT NOT NULL
    )",
    "CREATE TABLE mapConstellations (
        constellationID INTEGER PRIMARY KEY,
        constellationName TEXT NOT NULL,
        regionID INTEGER NOT NULL
    )",
    "CREATE TABLE mapSolarSystems (
        solarSystemID INTEGER PRIMARY KEY,
        solarSystemName TEXT NOT NULL,
        constellationID INTEGER NOT NULL,
        regionID INTEGER NOT NULL,
        security REAL NOT NULL,
        radiusAu REAL NOT NULL,
        longestWarpAu REAL NOT NULL
    )",
    "CREATE TABLE mapSolarSystemJumps (
        fromSolarSystemID INTEGER NOT NULL,
        toSolarSystemID INTEGER NOT NULL,
        PRIMARY KEY (fromSolarSystemID, toSolarSystemID)
    )",
    "CREATE TABLE staStations (
        stationID INTEGER PRIMARY KEY,
        solarSystemID INTEGER NOT NULL,
        stationName TEXT NOT NULL,
        hasRepair INTEGER NOT NULL,
        hasCloning INTEGER NOT NULL
    )",
];

const INDEXES: [&str; 5] = [
    "CREATE INDEX staStations_solarSystemID ON staStations (solarSystemID)",
    "CREATE INDEX invTypes_name ON invTypes (typeName)",
    "CREATE INDEX invMetaTypes_typeID ON invMetaTypes (typeID)",
    "CREATE INDEX invMetaTypes_parentTypeID ON invMetaTypes (parentTypeID)",
    "CREATE INDEX dgmTypeAttributes_typeID ON dgmTypeAttributes (typeID)",
];

/// Rows per multi-row `INSERT`; keeps the bound parameter count well under SQLite's limit.
const CHUNK: usize = 2000;

/// Writes a fresh static data file at `path` (any existing file there is replaced).
pub(super) async fn write(path: &Path, data: &Extracted, info: &SdeInfo) -> Result<(), SdeError> {
    if path.exists() {
        std::fs::remove_file(path).map_err(io_error(path))?;
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Off)
        .synchronous(SqliteSynchronous::Off);
    let mut conn = SqliteConnection::connect_with(&options).await?;
    let mut tx = conn.begin().await?;

    for statement in SCHEMA {
        sqlx::query(statement).execute(&mut *tx).await?;
    }

    for chunk in data.types.chunks(CHUNK) {
        QueryBuilder::<Sqlite>::new("INSERT INTO invTypes (typeID, typeName, groupID, published) ")
            .push_values(chunk, |mut row, ty| {
                row.push_bind(ty.id)
                    .push_bind(ty.name.clone())
                    .push_bind(ty.group_id)
                    .push_bind(ty.published);
            })
            .build()
            .execute(&mut *tx)
            .await?;
    }

    for chunk in data.groups.chunks(CHUNK) {
        QueryBuilder::<Sqlite>::new("INSERT OR REPLACE INTO invGroups (groupID, categoryID) ")
            .push_values(chunk, |mut row, &(group, category)| {
                row.push_bind(group).push_bind(category);
            })
            .build()
            .execute(&mut *tx)
            .await?;
    }

    for chunk in data.metas.chunks(CHUNK) {
        QueryBuilder::<Sqlite>::new(
            "INSERT INTO invMetaTypes (typeID, parentTypeID, metaGroupID) ",
        )
        .push_values(chunk, |mut row, meta| {
            row.push_bind(meta.type_id)
                .push_bind(meta.parent_type_id)
                .push_bind(meta.meta_group_id);
        })
        .build()
        .execute(&mut *tx)
        .await?;
    }

    for chunk in data.attributes.chunks(CHUNK) {
        QueryBuilder::<Sqlite>::new(
            "INSERT OR REPLACE INTO dgmTypeAttributes (typeID, attributeID, valueInt, valueFloat) ",
        )
        .push_values(chunk, |mut row, attribute| {
            row.push_bind(attribute.type_id)
                .push_bind(attribute.attribute_id)
                .push_bind(attribute.value_int)
                .push_bind(attribute.value_float);
        })
        .build()
        .execute(&mut *tx)
        .await?;
    }

    for chunk in data.effects.chunks(CHUNK) {
        QueryBuilder::<Sqlite>::new("INSERT OR IGNORE INTO dgmTypeEffects (typeID, effectID) ")
            .push_values(chunk, |mut row, effect| {
                row.push_bind(effect.type_id).push_bind(effect.effect.0);
            })
            .build()
            .execute(&mut *tx)
            .await?;
    }

    write_map(&mut tx, &data.map).await?;

    let mut entries = vec![
        ("schemaVersion", SCHEMA_VERSION.to_string()),
        ("buildNumber", info.build_number.to_string()),
    ];
    if let Some(release_date) = &info.release_date {
        entries.push(("releaseDate", release_date.clone()));
    }
    QueryBuilder::<Sqlite>::new("INSERT INTO sdeInfo (key, value) ")
        .push_values(entries, |mut row, (key, value)| {
            row.push_bind(key).push_bind(value);
        })
        .build()
        .execute(&mut *tx)
        .await?;

    for statement in INDEXES {
        sqlx::query(statement).execute(&mut *tx).await?;
    }

    tx.commit().await?;
    conn.close().await?;
    Ok(())
}

async fn write_map(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    map: &MapTables,
) -> Result<(), sqlx::Error> {
    for chunk in map.regions.chunks(CHUNK) {
        QueryBuilder::<Sqlite>::new("INSERT OR REPLACE INTO mapRegions (regionID, regionName) ")
            .push_values(chunk, |mut row, region| {
                row.push_bind(region.id).push_bind(region.name.clone());
            })
            .build()
            .execute(&mut **tx)
            .await?;
    }
    for chunk in map.constellations.chunks(CHUNK) {
        QueryBuilder::<Sqlite>::new(
            "INSERT OR REPLACE INTO mapConstellations (constellationID, constellationName, regionID) ",
        )
        .push_values(chunk, |mut row, c| {
            row.push_bind(c.id)
                .push_bind(c.name.clone())
                .push_bind(c.region_id);
        })
        .build()
        .execute(&mut **tx)
        .await?;
    }
    for chunk in map.systems.chunks(CHUNK) {
        QueryBuilder::<Sqlite>::new(
            "INSERT OR REPLACE INTO mapSolarSystems (solarSystemID, solarSystemName, \
             constellationID, regionID, security, radiusAu, longestWarpAu) ",
        )
        .push_values(chunk, |mut row, s| {
            row.push_bind(s.id)
                .push_bind(s.name.clone())
                .push_bind(s.constellation_id)
                .push_bind(s.region_id)
                .push_bind(s.security)
                .push_bind(s.radius_au)
                .push_bind(s.longest_warp_au);
        })
        .build()
        .execute(&mut **tx)
        .await?;
    }
    for chunk in map.jumps.chunks(CHUNK) {
        QueryBuilder::<Sqlite>::new(
            "INSERT OR IGNORE INTO mapSolarSystemJumps (fromSolarSystemID, toSolarSystemID) ",
        )
        .push_values(chunk, |mut row, &(from, to)| {
            row.push_bind(from).push_bind(to);
        })
        .build()
        .execute(&mut **tx)
        .await?;
    }
    for chunk in map.stations.chunks(CHUNK) {
        QueryBuilder::<Sqlite>::new(
            "INSERT OR REPLACE INTO staStations (stationID, solarSystemID, stationName, \
             hasRepair, hasCloning) ",
        )
        .push_values(chunk, |mut row, s| {
            row.push_bind(s.id)
                .push_bind(s.system_id)
                .push_bind(s.name.clone())
                .push_bind(s.has_repair)
                .push_bind(s.has_cloning);
        })
        .build()
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Reads every table of a static data file.
pub(crate) async fn read_tables(
    path: &Path,
) -> Result<(Tables, MapTables, Option<SdeInfo>), sqlx::Error> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .immutable(true);
    let mut conn = SqliteConnection::connect_with(&options).await?;

    let types = sqlx::query_as::<_, (i32, String, i32, bool, Option<i32>)>(
        "SELECT invTypes.typeID, invTypes.typeName, invTypes.groupID, invTypes.published,
                invGroups.categoryID
         FROM invTypes LEFT JOIN invGroups ON invGroups.groupID = invTypes.groupID",
    )
    .fetch_all(&mut conn)
    .await?
    .into_iter()
    .map(|(id, name, group_id, published, category_id)| TypeRow {
        id,
        name,
        group_id,
        category_id,
        published,
    })
    .collect();

    let attributes = sqlx::query_as::<_, (i32, i32, Option<f64>)>(
        "SELECT typeID, attributeID, CAST(COALESCE(valueInt, valueFloat) AS REAL)
         FROM dgmTypeAttributes",
    )
    .fetch_all(&mut conn)
    .await?
    .into_iter()
    .filter_map(|(type_id, attribute, value)| {
        Some(AttributeRow {
            type_id,
            attribute: AttributeId(attribute),
            value: value?,
        })
    })
    .collect();

    let effects = sqlx::query_as::<_, (i32, i32)>("SELECT typeID, effectID FROM dgmTypeEffects")
        .fetch_all(&mut conn)
        .await?
        .into_iter()
        .map(|(type_id, effect)| EffectRow {
            type_id,
            effect: EffectId(effect),
        })
        .collect();

    let metas = sqlx::query_as::<_, (i32, Option<i32>, Option<i32>)>(
        "SELECT typeID, parentTypeID, metaGroupID FROM invMetaTypes",
    )
    .fetch_all(&mut conn)
    .await?
    .into_iter()
    .map(|(type_id, parent_type_id, meta_group_id)| MetaRow {
        type_id,
        parent_type_id,
        meta_group_id,
    })
    .collect();

    let info = read_info(&mut conn).await?;
    let map = read_map(&mut conn).await?;
    conn.close().await?;

    Ok((
        Tables {
            types,
            attributes,
            effects,
            metas,
        },
        map,
        info,
    ))
}

/// Reads the star map; files built before schema version 2 have none.
async fn read_map(conn: &mut SqliteConnection) -> Result<MapTables, sqlx::Error> {
    if !has_table(conn, "mapSolarSystems").await? {
        return Ok(MapTables::default());
    }
    let regions = sqlx::query_as::<_, (i64, String)>("SELECT regionID, regionName FROM mapRegions")
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .map(|(id, name)| Region { id, name })
        .collect();
    let constellations = sqlx::query_as::<_, (i64, String, i64)>(
        "SELECT constellationID, constellationName, regionID FROM mapConstellations",
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|(id, name, region_id)| Constellation {
        id,
        name,
        region_id,
        systems: Vec::new(),
    })
    .collect();
    let systems = sqlx::query_as::<_, (i64, String, i64, i64, f64, f64, f64)>(
        "SELECT solarSystemID, solarSystemName, constellationID, regionID, security, radiusAu, \
         longestWarpAu FROM mapSolarSystems",
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(
        |(id, name, constellation_id, region_id, security, radius_au, longest_warp_au)| {
            SolarSystem {
                id,
                name,
                constellation_id,
                region_id,
                security,
                radius_au,
                longest_warp_au,
            }
        },
    )
    .collect();
    let jumps = sqlx::query_as::<_, (i64, i64)>(
        "SELECT fromSolarSystemID, toSolarSystemID FROM mapSolarSystemJumps",
    )
    .fetch_all(&mut *conn)
    .await?;
    let stations = sqlx::query_as::<_, (i64, i64, String, bool, bool)>(
        "SELECT stationID, solarSystemID, stationName, hasRepair, hasCloning FROM staStations",
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|(id, system_id, name, has_repair, has_cloning)| Station {
        id,
        system_id,
        name,
        has_repair,
        has_cloning,
    })
    .collect();
    Ok(MapTables {
        regions,
        constellations,
        systems,
        jumps,
        stations,
    })
}

async fn has_table(conn: &mut SqliteConnection, table: &str) -> Result<bool, sqlx::Error> {
    let found: Option<(String,)> =
        sqlx::query_as("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?")
            .bind(table)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(found.is_some())
}

/// The `schemaVersion` recorded in a static data file (0 when none is recorded, e.g. a legacy
/// file).
pub(crate) async fn schema_version(path: &Path) -> Result<u32, sqlx::Error> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .immutable(true);
    let mut conn = SqliteConnection::connect_with(&options).await?;
    let version = if has_table(&mut conn, "sdeInfo").await? {
        sqlx::query_as::<_, (String,)>("SELECT value FROM sdeInfo WHERE key = 'schemaVersion'")
            .fetch_optional(&mut conn)
            .await?
            .and_then(|(v,)| v.parse().ok())
            .unwrap_or(0)
    } else {
        0
    };
    conn.close().await?;
    Ok(version)
}

/// Reads `sdeInfo`; files built by the legacy waitlist do not have it.
async fn read_info(conn: &mut SqliteConnection) -> Result<Option<SdeInfo>, sqlx::Error> {
    let has_table: Option<(String,)> =
        sqlx::query_as("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'sdeInfo'")
            .fetch_optional(&mut *conn)
            .await?;
    if has_table.is_none() {
        return Ok(None);
    }
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM sdeInfo")
        .fetch_all(&mut *conn)
        .await?;
    let value = |key: &str| {
        rows.iter()
            .find(|(k, _)| k == key)
            .map(|(_, value)| value.clone())
    };
    Ok(value("buildNumber")
        .and_then(|build| build.parse().ok())
        .map(|build_number| SdeInfo {
            build_number,
            release_date: value("releaseDate"),
        }))
}
