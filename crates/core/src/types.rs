//! Wire types shared by several crates. Field names and shapes are part of the public API.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// EVE type id (ships, modules, skills, implants...).
pub type TypeId = i32;

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, ToSchema)]
pub struct Character {
    pub id: i64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub corporation_id: Option<i64>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, ToSchema)]
pub struct Hull {
    #[schema(value_type = i32)]
    pub id: TypeId,
    pub name: String,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, ToSchema)]
pub struct WaitlistCategory {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
pub struct Entity {
    pub id: i64,
    pub name: Option<String>,
    pub category: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
pub struct Ban {
    pub id: Option<i64>,
    pub entity: Option<Entity>,
    pub issued_at: Option<i64>,
    pub issued_by: Option<Character>,
    pub public_reason: Option<String>,
    pub reason: String,
    pub revoked_at: Option<i64>,
    pub revoked_by: Option<Character>,
}

#[derive(Serialize, Debug, Clone, ToSchema)]
pub struct CharacterAndLevel {
    pub id: i64,
    pub name: String,
    pub tags: Vec<String>,
    pub active_bans: Option<Vec<Ban>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
pub struct Alliance {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
pub struct Corporation {
    pub id: i64,
    pub name: String,
    pub alliance: Option<Alliance>,
    pub last_updated: Option<i64>,
}
