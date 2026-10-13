//! Character, corporation and alliance endpoints: skills, implants, the in-game window, search,
//! public info, affiliations and bulk names.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use wl_core::types::TypeId;

use eve_esi_client::types::GetCharactersCharacterIdSearchCategoriesItem as SearchCategoryItem;

use crate::error::{ErrorStyle, EsiError, EsiResult};
use crate::esi::{Esi, send};
use crate::scope::Scope;
use crate::util::{BULK_LIMIT, type_id};

/// One trained skill (`GET /characters/{id}/skills`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterSkill {
    pub skill_id: TypeId,
    /// The level trained.
    pub trained_skill_level: i8,
    /// The level in effect (lower than trained for an Alpha clone).
    pub active_skill_level: i8,
    pub skillpoints_in_skill: i64,
}

/// What [`Esi::search`] looks for. The waitlist searches characters, corporations and alliances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchCategory {
    Character,
    Corporation,
    Alliance,
}

impl SearchCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            SearchCategory::Character => "character",
            SearchCategory::Corporation => "corporation",
            SearchCategory::Alliance => "alliance",
        }
    }

    /// Parse one category name.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "character" => Some(SearchCategory::Character),
            "corporation" => Some(SearchCategory::Corporation),
            "alliance" => Some(SearchCategory::Alliance),
            _ => None,
        }
    }

    fn item(self) -> SearchCategoryItem {
        match self {
            SearchCategory::Character => SearchCategoryItem::Character,
            SearchCategory::Corporation => SearchCategoryItem::Corporation,
            SearchCategory::Alliance => SearchCategoryItem::Alliance,
        }
    }
}

/// IDs found by [`Esi::search`], per category searched.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub character: Vec<i64>,
    pub corporation: Vec<i64>,
    pub alliance: Vec<i64>,
}

/// Public character info (`GET /characters/{id}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterInfo {
    pub name: String,
    pub corporation_id: i64,
    pub alliance_id: Option<i64>,
}

/// Public corporation info (`GET /corporations/{id}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorporationInfo {
    pub name: String,
    pub ticker: String,
    pub alliance_id: Option<i64>,
}

/// Public alliance info (`GET /alliances/{id}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllianceInfo {
    pub name: String,
    pub ticker: String,
}

/// A character's corporation, alliance and faction (`POST /characters/affiliation`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterAffiliation {
    pub character_id: i64,
    pub corporation_id: i64,
    pub alliance_id: Option<i64>,
    pub faction_id: Option<i64>,
}

/// The kinds of entity a ban (or anything else keyed by `kind` + ID) can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntityKind {
    Character,
    Corporation,
    Alliance,
}

impl EntityKind {
    /// Parse `Character`/`Corporation`/`Alliance`, case-insensitively.
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "character" => Some(EntityKind::Character),
            "corporation" => Some(EntityKind::Corporation),
            "alliance" => Some(EntityKind::Alliance),
            _ => None,
        }
    }
}

impl Esi {
    /// `character_id`'s skills.
    pub async fn skills(&self, character_id: i64) -> EsiResult<Vec<CharacterSkill>> {
        let client = self
            .client_for(character_id, Scope::SkillsReadSkills)
            .await?;
        let skills = send(
            client
                .get_characters_character_id_skills()
                .character_id(character_id)
                .send(),
            ErrorStyle::Body,
        )
        .await?;
        skills
            .skills
            .into_iter()
            .map(|s| {
                Ok(CharacterSkill {
                    skill_id: type_id(s.skill_id)?,
                    trained_skill_level: level(s.trained_skill_level),
                    active_skill_level: level(s.active_skill_level),
                    skillpoints_in_skill: s.skillpoints_in_skill,
                })
            })
            .collect()
    }

    /// The type IDs of `character_id`'s active implants.
    pub async fn implants(&self, character_id: i64) -> EsiResult<Vec<TypeId>> {
        let client = self
            .client_for(character_id, Scope::ClonesReadImplants)
            .await?;
        let implants = send(
            client
                .get_characters_character_id_implants()
                .character_id(character_id)
                .send(),
            ErrorStyle::Body,
        )
        .await?;
        implants.0.into_iter().map(type_id).collect()
    }

    /// Open the show-info window for `target_id` in `character_id`'s game client.
    pub async fn open_information_window(
        &self,
        character_id: i64,
        target_id: i64,
    ) -> EsiResult<()> {
        let client = self.client_for(character_id, Scope::UiOpenWindow).await?;
        send(
            client
                .post_ui_openwindow_information()
                .target_id(target_id)
                .send(),
            ErrorStyle::Message,
        )
        .await
    }

    /// Search names as `character_id` (needs the search scope FCs grant). `search` must be at
    /// least 3 characters; shorter fails with `WithMessage(400, ..)` without calling ESI.
    pub async fn search(
        &self,
        character_id: i64,
        categories: &[SearchCategory],
        search: &str,
        strict: bool,
    ) -> EsiResult<SearchResult> {
        if search.chars().count() < 3 {
            return Err(EsiError::WithMessage(
                400,
                "search must be at least 3 characters".to_string(),
            ));
        }
        if categories.is_empty() {
            return Ok(SearchResult::default());
        }
        let client = self
            .client_for(character_id, Scope::SearchStructures)
            .await?;
        let found = send(
            client
                .get_characters_character_id_search()
                .character_id(character_id)
                .categories(categories.iter().map(|c| c.item()).collect::<Vec<_>>())
                .search(search)
                .strict(strict)
                .send(),
            ErrorStyle::Body,
        )
        .await?;
        Ok(SearchResult {
            character: found.character,
            corporation: found.corporation,
            alliance: found.alliance,
        })
    }

    /// Public info for a character.
    pub async fn character(&self, character_id: i64) -> EsiResult<CharacterInfo> {
        let info = send(
            self.client()
                .get_characters_detail()
                .character_id(character_id)
                .send(),
            ErrorStyle::Status,
        )
        .await?;
        Ok(CharacterInfo {
            name: info.name,
            corporation_id: *info.corporation_id,
            alliance_id: info.alliance_id.map(|id| *id),
        })
    }

    /// Public info for a corporation.
    pub async fn corporation(&self, corporation_id: i64) -> EsiResult<CorporationInfo> {
        let info = send(
            self.client()
                .get_corporations_corporation_id()
                .corporation_id(corporation_id)
                .send(),
            ErrorStyle::Status,
        )
        .await?;
        Ok(CorporationInfo {
            name: info.name,
            ticker: info.ticker,
            alliance_id: info.alliance_id.map(|id| *id),
        })
    }

    /// Public info for an alliance.
    pub async fn alliance(&self, alliance_id: i64) -> EsiResult<AllianceInfo> {
        let info = send(
            self.client()
                .get_alliances_alliance_id()
                .alliance_id(alliance_id)
                .send(),
            ErrorStyle::Status,
        )
        .await?;
        Ok(AllianceInfo {
            name: info.name,
            ticker: info.ticker,
        })
    }

    /// The name of a character, corporation or alliance (what the ban form looks up).
    pub async fn entity_name(&self, kind: EntityKind, id: i64) -> EsiResult<String> {
        Ok(match kind {
            EntityKind::Character => self.character(id).await?.name,
            EntityKind::Corporation => self.corporation(id).await?.name,
            EntityKind::Alliance => self.alliance(id).await?.name,
        })
    }

    /// Corporations, alliances and factions of many characters, in requests of up to 1000 IDs.
    pub async fn character_affiliations(
        &self,
        character_ids: &[i64],
    ) -> EsiResult<Vec<CharacterAffiliation>> {
        let mut out = Vec::with_capacity(character_ids.len());
        for chunk in character_ids.chunks(BULK_LIMIT) {
            let found = send(
                self.client()
                    .post_characters_affiliation()
                    .body(chunk.to_vec())
                    .send(),
                ErrorStyle::Message,
            )
            .await?;
            out.extend(found.0.into_iter().map(|a| CharacterAffiliation {
                character_id: a.character_id,
                corporation_id: a.corporation_id,
                alliance_id: a.alliance_id,
                faction_id: a.faction_id,
            }));
        }
        Ok(out)
    }

    /// Names of many IDs of any kind, in requests of up to 1000 IDs. ESI fails a whole request
    /// (404) if any ID in it doesn't exist.
    pub async fn names(&self, ids: &[i64]) -> EsiResult<HashMap<i64, String>> {
        let mut out = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(BULK_LIMIT) {
            let found = send(
                self.client()
                    .post_universe_names()
                    .body(chunk.to_vec())
                    .send(),
                ErrorStyle::Message,
            )
            .await?;
            out.extend(found.0.into_iter().map(|n| (n.id, n.name)));
        }
        Ok(out)
    }

    /// An inventory type's name.
    pub async fn type_name(&self, type_id: i64) -> EsiResult<String> {
        let info = send(
            self.client()
                .get_universe_types_type_id()
                .type_id(type_id)
                .send(),
            ErrorStyle::Status,
        )
        .await?;
        Ok(info.name)
    }
}

fn level(value: i64) -> i8 {
    value.clamp(0, 5) as i8
}
