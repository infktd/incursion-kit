//! The simulated universe: characters, corporations, fleets, wallets, killmails, wars, incursions
//! and SSO grants, built from a [`Seed`] and changed by ESI calls and the control API.

use std::collections::{BTreeMap, HashMap, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::jwt::default_owner_hash;
use crate::seed::{
    Alliance, Character, Corporation, Incursion, JournalEntry, Killmail, NameEntry, Seed,
    SeedError, SeedSource, SeedTime, SsoClient, Wallet, War, WingLayout,
};
use crate::universe::Universe;

/// A failed operation: an HTTP status and a message (ESI's `{"error": ...}`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{status}: {message}")]
pub struct DevError {
    pub status: u16,
    pub message: String,
}

impl DevError {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(404, message)
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, message)
    }
    pub fn unprocessable(message: impl Into<String>) -> Self {
        Self::new(422, message)
    }
}

pub type DevResult<T> = Result<T, DevError>;

/// What ESI answers for fleet endpoints when the caller isn't the boss.
pub const NO_FLEET_ACCESS: &str = "The fleet does not exist or you don't have access to it!";

/// Runtime settings (initially from the seed; `PUT /_dev/settings` changes them).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub invite_accept_delay_ms: u64,
    pub default_layout: Vec<WingLayout>,
    pub incursions_cache_secs: u64,
    pub access_token_lifetime_secs: u64,
    pub issuer: String,
    pub clients: Vec<SsoClient>,
}

/// A fleet position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    FleetCommander,
    WingCommander,
    SquadCommander,
    SquadMember,
}

#[derive(Debug, Clone, Serialize)]
pub struct Fleet {
    pub fleet_id: i64,
    pub boss_id: i64,
    pub motd: String,
    pub is_free_move: bool,
    pub is_registered: bool,
    pub is_voice_enabled: bool,
    pub wings: Vec<Wing>,
    pub members: Vec<Member>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Wing {
    pub id: i64,
    pub name: String,
    pub squads: Vec<Squad>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Squad {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub character_id: i64,
    pub role: Role,
    /// -1 for the fleet commander.
    pub wing_id: i64,
    /// -1 for the fleet and wing commanders.
    pub squad_id: i64,
    pub join_time: SeedTime,
    pub takes_fleet_warp: bool,
}

/// A fleet invite waiting for the pilot's answer.
#[derive(Debug, Clone, Serialize)]
pub struct Invite {
    pub id: i64,
    pub fleet_id: i64,
    pub character_id: i64,
    pub invited_by: i64,
    pub role: Role,
    pub wing_id: i64,
    pub squad_id: i64,
    pub invited_at: SeedTime,
}

/// Something that happened, for the dashboard and for tests to assert on.
#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub id: u64,
    pub at: SeedTime,
    /// `login`, `token`, `fleet_created`, `invite`, `invite_accepted`, `invite_declined`,
    /// `joined`, `left`, `kicked`, `boss`, `motd`, `open_window`, `reset`, ...
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub character_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fleet_id: Option<i64>,
    pub detail: String,
}

/// An issued authorization code.
#[derive(Debug, Clone)]
pub struct AuthCode {
    pub client_id: String,
    pub redirect_uri: String,
    pub character_id: i64,
    pub scopes: Vec<String>,
    /// `(method, challenge)` when the login used PKCE.
    pub challenge: Option<(String, String)>,
    pub expires_at: i64,
}

/// What a refresh token grants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefreshGrant {
    pub character_id: i64,
    pub client_id: String,
    pub scopes: Vec<String>,
    pub issued_at: i64,
}

/// SSO grants. Kept across a reset, so logged-in sessions keep working.
#[derive(Debug, Default)]
pub struct SsoState {
    pub codes: HashMap<String, AuthCode>,
    /// Live refresh tokens (opaque, like EVE's; each is exchanged once, then replaced).
    pub refresh_tokens: HashMap<String, RefreshGrant>,
}

const MAX_EVENTS: usize = 500;

/// The whole simulated state.
#[derive(Debug)]
pub struct World {
    source: SeedSource,
    pub settings: Settings,
    pub skill_sets: BTreeMap<String, BTreeMap<i64, i64>>,
    pub alliances: BTreeMap<i64, Alliance>,
    pub corporations: BTreeMap<i64, Corporation>,
    pub characters: BTreeMap<i64, Character>,
    pub fleets: BTreeMap<i64, Fleet>,
    /// Pending invites, by invited character.
    pub invites: BTreeMap<i64, Invite>,
    pub killmails: BTreeMap<i64, Killmail>,
    pub wars: BTreeMap<i64, War>,
    pub incursions: Vec<Incursion>,
    /// When the incursion list last changed (`Last-Modified`).
    pub incursions_modified: DateTime<Utc>,
    pub names: BTreeMap<i64, NameEntry>,
    pub events: VecDeque<Event>,
    next_event: u64,
    next_id: i64,
    pub sso: SsoState,
}

impl World {
    /// A world as the seed describes it.
    pub fn new(source: SeedSource) -> Result<Self, SeedError> {
        let seed = source.parse()?;
        let mut world = Self {
            source,
            settings: settings_of(&seed),
            skill_sets: BTreeMap::new(),
            alliances: BTreeMap::new(),
            corporations: BTreeMap::new(),
            characters: BTreeMap::new(),
            fleets: BTreeMap::new(),
            invites: BTreeMap::new(),
            killmails: BTreeMap::new(),
            wars: BTreeMap::new(),
            incursions: Vec::new(),
            incursions_modified: Utc::now(),
            names: BTreeMap::new(),
            events: VecDeque::new(),
            next_event: 1,
            next_id: 1_000_000_000_001,
            sso: SsoState::default(),
        };
        world.load(seed)?;
        Ok(world)
    }

    /// Back to the seed (re-read, so relative times are relative to now). SSO grants and the
    /// event log are kept.
    pub fn reset(&mut self) -> Result<(), SeedError> {
        let seed = self.source.parse()?;
        self.settings = settings_of(&seed);
        self.load(seed)?;
        self.event("reset", None, None, "world reset to the seed".into());
        Ok(())
    }

    fn load(&mut self, seed: Seed) -> Result<(), SeedError> {
        self.skill_sets = seed.skill_sets;
        self.alliances = seed.alliances.into_iter().map(|a| (a.id, a)).collect();
        self.corporations = seed.corporations.into_iter().map(|c| (c.id, c)).collect();
        self.characters = seed.characters.into_iter().map(|c| (c.id, c)).collect();
        self.fleets.clear();
        self.invites.clear();
        self.killmails.clear();
        for km in seed.killmails {
            self.add_killmail(km)
                .map_err(|e| SeedError::Invalid(e.message))?;
        }
        self.wars = seed.wars.into_iter().map(|w| (w.id, w)).collect();
        self.incursions = seed.incursions;
        self.incursions_modified = Utc::now();
        self.names = seed.names.into_iter().map(|n| (n.id, n)).collect();
        // Journal IDs are assigned like any other entry's.
        for corporation in self.corporations.values_mut() {
            for entry in &mut corporation.journal {
                if entry.id == 0 {
                    entry.id = self.next_id;
                    self.next_id += 1;
                }
            }
        }
        for fleet in seed.fleets {
            let fleet_id = self
                .create_fleet(fleet.boss_id)
                .map_err(|e| SeedError::Invalid(e.message))?;
            if let Some(f) = self.fleets.get_mut(&fleet_id) {
                f.motd = fleet.motd;
            }
            for member in fleet.members {
                let squad_id = member.squad.as_deref().and_then(|name| {
                    self.fleets[&fleet_id]
                        .wings
                        .iter()
                        .flat_map(|w| &w.squads)
                        .find(|s| s.name == name)
                        .map(|s| s.id)
                });
                if member.squad.is_some() && squad_id.is_none() {
                    return Err(SeedError::Invalid(format!(
                        "fleet of {}: no squad named {:?}",
                        fleet.boss_id, member.squad
                    )));
                }
                self.join_fleet(
                    fleet_id,
                    member.character_id,
                    Role::SquadMember,
                    None,
                    squad_id,
                )
                .map_err(|e| SeedError::Invalid(e.message))?;
            }
        }
        Ok(())
    }

    pub fn next_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn event(
        &mut self,
        kind: &str,
        character_id: Option<i64>,
        fleet_id: Option<i64>,
        detail: String,
    ) {
        tracing::info!(kind, ?character_id, ?fleet_id, "{detail}");
        self.events.push_back(Event {
            id: self.next_event,
            at: SeedTime::now(),
            kind: kind.to_string(),
            character_id,
            fleet_id,
            detail,
        });
        self.next_event += 1;
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
    }

    // --- characters ------------------------------------------------------------------------

    pub fn character(&self, id: i64) -> DevResult<&Character> {
        self.characters
            .get(&id)
            .ok_or_else(|| DevError::not_found("Character not found"))
    }

    pub fn character_name(&self, id: i64) -> String {
        self.characters
            .get(&id)
            .map_or_else(|| format!("character {id}"), |c| c.name.clone())
    }

    /// The character's alliance (through its corporation).
    pub fn alliance_of(&self, character: &Character) -> Option<i64> {
        self.corporations
            .get(&character.corporation_id)
            .and_then(|c| c.alliance_id)
    }

    pub fn owner_hash(&self, character: &Character) -> String {
        character
            .owner
            .clone()
            .unwrap_or_else(|| default_owner_hash(character.id))
    }

    /// Trained skill levels: the skill set with the character's own levels on top.
    pub fn skills_of(&self, character: &Character) -> BTreeMap<i64, i64> {
        let mut skills = character
            .skill_set
            .as_ref()
            .and_then(|set| self.skill_sets.get(set))
            .cloned()
            .unwrap_or_default();
        for (&skill, &level) in &character.skills {
            if level <= 0 {
                skills.remove(&skill);
            } else {
                skills.insert(skill, level.min(5));
            }
        }
        skills
    }

    /// Add a character (its corporation must exist).
    pub fn add_character(&mut self, character: Character) -> DevResult<()> {
        if self.characters.contains_key(&character.id)
            || self.corporations.contains_key(&character.id)
            || self.alliances.contains_key(&character.id)
        {
            return Err(DevError::new(409, format!("ID {} is taken", character.id)));
        }
        self.check_character(&character)?;
        self.event(
            "character_added",
            Some(character.id),
            None,
            format!("{} added", character.name),
        );
        self.characters.insert(character.id, character);
        Ok(())
    }

    /// Replace a character's data (its ID stays).
    pub fn update_character(&mut self, character: Character) -> DevResult<()> {
        self.character(character.id)?;
        self.check_character(&character)?;
        self.characters.insert(character.id, character);
        Ok(())
    }

    fn check_character(&self, character: &Character) -> DevResult<()> {
        if !self.corporations.contains_key(&character.corporation_id) {
            return Err(DevError::bad_request(format!(
                "unknown corporation {}",
                character.corporation_id
            )));
        }
        if let Some(set) = &character.skill_set
            && !self.skill_sets.contains_key(set)
        {
            return Err(DevError::bad_request(format!("unknown skill set {set:?}")));
        }
        Ok(())
    }

    /// Revoke the character's refresh tokens (the waitlist has to log it in again).
    pub fn revoke_tokens(&mut self, character_id: i64) -> DevResult<()> {
        self.character(character_id)?;
        self.sso
            .refresh_tokens
            .retain(|_, grant| grant.character_id != character_id);
        self.event(
            "revoked",
            Some(character_id),
            None,
            format!(
                "{}'s refresh tokens revoked",
                self.character_name(character_id)
            ),
        );
        Ok(())
    }

    // --- names -----------------------------------------------------------------------------

    /// Name and `POST /universe/names` category of any ID dev-esi knows.
    pub fn name_of(&self, id: i64, universe: &Universe) -> Option<(String, String)> {
        if let Some(c) = self.characters.get(&id) {
            return Some((c.name.clone(), "character".into()));
        }
        if let Some(c) = self.corporations.get(&id) {
            return Some((c.name.clone(), "corporation".into()));
        }
        if let Some(a) = self.alliances.get(&id) {
            return Some((a.name.clone(), "alliance".into()));
        }
        if let Some(n) = self.names.get(&id) {
            return Some((n.name.clone(), n.category.clone()));
        }
        universe
            .name(id)
            .map(|(name, category)| (name.to_string(), category.to_string()))
    }

    // --- corporations ----------------------------------------------------------------------

    pub fn corporation(&self, id: i64) -> DevResult<&Corporation> {
        self.corporations
            .get(&id)
            .ok_or_else(|| DevError::not_found("Corporation not found"))
    }

    /// All seven wallet divisions.
    pub fn wallets(&self, corporation_id: i64) -> DevResult<Vec<Wallet>> {
        let corporation = self.corporation(corporation_id)?;
        Ok((1..=7)
            .map(|division| Wallet {
                division,
                balance: corporation
                    .wallets
                    .iter()
                    .find(|w| w.division == division)
                    .map_or(0.0, |w| w.balance),
            })
            .collect())
    }

    /// Add a journal entry: assigns its ID, moves the division's balance by `amount` and records
    /// the new balance, and writes EVE's usual description for a donation from a known character
    /// when none is given.
    pub fn add_journal_entry(
        &mut self,
        corporation_id: i64,
        division: i64,
        mut entry: JournalEntry,
    ) -> DevResult<JournalEntry> {
        if !(1..=7).contains(&division) {
            return Err(DevError::bad_request("division must be 1-7"));
        }
        self.corporation(corporation_id)?;
        entry.division = division;
        if entry.id == 0 {
            entry.id = self.next_id();
        }
        if entry.description.is_empty()
            && entry.ref_type == "player_donation"
            && let Some(payer) = entry.first_party_id.and_then(|id| self.characters.get(&id))
        {
            entry.description = format!(
                "{} deposited cash into {}'s account",
                payer.name, self.corporations[&corporation_id].name
            );
        }
        let corporation = self
            .corporations
            .get_mut(&corporation_id)
            .expect("checked above");
        if let Some(amount) = entry.amount {
            let wallet = match corporation
                .wallets
                .iter_mut()
                .position(|w| w.division == division)
            {
                Some(i) => &mut corporation.wallets[i],
                None => {
                    corporation.wallets.push(Wallet {
                        division,
                        balance: 0.0,
                    });
                    corporation.wallets.last_mut().expect("just pushed")
                }
            };
            wallet.balance += amount;
            entry.balance.get_or_insert(wallet.balance);
        }
        corporation.journal.push(entry.clone());
        Ok(entry)
    }

    // --- killmails -------------------------------------------------------------------------

    /// Add a killmail, assigning its ID and hash when absent.
    pub fn add_killmail(&mut self, mut km: Killmail) -> DevResult<Killmail> {
        if km.killmail_id == 0 {
            km.killmail_id = self
                .killmails
                .keys()
                .next_back()
                .map_or(130_000_001, |id| id + 1);
        }
        if km.killmail_hash.is_empty() {
            let digest = aws_lc_rs::digest::digest(
                &aws_lc_rs::digest::SHA256,
                format!("dev-esi killmail {}", km.killmail_id).as_bytes(),
            );
            km.killmail_hash = digest.as_ref()[..20]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
        }
        if self.killmails.contains_key(&km.killmail_id) {
            return Err(DevError::new(
                409,
                format!("killmail {} exists", km.killmail_id),
            ));
        }
        self.killmails.insert(km.killmail_id, km.clone());
        Ok(km)
    }

    // --- fleets ----------------------------------------------------------------------------

    /// The fleet a character is in, and its membership.
    pub fn fleet_of(&self, character_id: i64) -> Option<(&Fleet, &Member)> {
        self.fleets.values().find_map(|fleet| {
            fleet
                .members
                .iter()
                .find(|m| m.character_id == character_id)
                .map(|m| (fleet, m))
        })
    }

    /// A fleet, as seen by `caller` (who must be its boss, as ESI requires).
    pub fn fleet_for_boss(&self, fleet_id: i64, caller: i64) -> DevResult<&Fleet> {
        self.fleets
            .get(&fleet_id)
            .filter(|f| f.boss_id == caller)
            .ok_or_else(|| DevError::not_found(NO_FLEET_ACCESS))
    }

    pub fn fleet(&self, fleet_id: i64) -> DevResult<&Fleet> {
        self.fleets
            .get(&fleet_id)
            .ok_or_else(|| DevError::not_found("Fleet not found"))
    }

    /// Form a new fleet with `boss_id` as its boss and fleet commander (it leaves any fleet it
    /// is in). The layout is `fleet.default_layout`.
    pub fn create_fleet(&mut self, boss_id: i64) -> DevResult<i64> {
        self.character(boss_id)?;
        self.leave_fleet(boss_id);
        self.invites.remove(&boss_id);
        let fleet_id = self.next_id();
        let layout = self.settings.default_layout.clone();
        let wings = layout
            .iter()
            .map(|wing| Wing {
                id: self.next_id(),
                name: wing.name.clone(),
                squads: wing
                    .squads
                    .iter()
                    .map(|name| Squad {
                        id: self.next_id(),
                        name: name.clone(),
                    })
                    .collect(),
            })
            .collect();
        self.fleets.insert(
            fleet_id,
            Fleet {
                fleet_id,
                boss_id,
                motd: String::new(),
                is_free_move: false,
                is_registered: false,
                is_voice_enabled: false,
                wings,
                members: vec![Member {
                    character_id: boss_id,
                    role: Role::FleetCommander,
                    wing_id: -1,
                    squad_id: -1,
                    join_time: SeedTime::now(),
                    takes_fleet_warp: true,
                }],
            },
        );
        self.event(
            "fleet_created",
            Some(boss_id),
            Some(fleet_id),
            format!("{} formed fleet {fleet_id}", self.character_name(boss_id)),
        );
        Ok(fleet_id)
    }

    /// Where a member with `role` goes: `(wing_id, squad_id)`. Missing IDs default to the first
    /// wing/squad; given IDs must exist in the fleet.
    fn position(
        fleet: &Fleet,
        role: Role,
        wing_id: Option<i64>,
        squad_id: Option<i64>,
    ) -> DevResult<(i64, i64)> {
        let invalid = || DevError::unprocessable("Invalid wing or squad ID");
        match role {
            Role::FleetCommander => {
                if fleet.members.iter().any(|m| m.role == Role::FleetCommander) {
                    return Err(DevError::unprocessable(
                        "The fleet commander position is taken",
                    ));
                }
                Ok((-1, -1))
            }
            Role::WingCommander => {
                let wing = match wing_id {
                    Some(id) => fleet
                        .wings
                        .iter()
                        .find(|w| w.id == id)
                        .ok_or_else(invalid)?,
                    None => fleet.wings.first().ok_or_else(invalid)?,
                };
                Ok((wing.id, -1))
            }
            Role::SquadCommander | Role::SquadMember => {
                let found = fleet.wings.iter().find_map(|w| {
                    w.squads
                        .iter()
                        .find(|s| squad_id.is_none_or(|id| s.id == id))
                        .filter(|_| wing_id.is_none_or(|id| w.id == id))
                        .map(|s| (w.id, s.id))
                });
                found.ok_or_else(invalid)
            }
        }
    }

    /// Put a character into a fleet directly (leaving any other fleet).
    pub fn join_fleet(
        &mut self,
        fleet_id: i64,
        character_id: i64,
        role: Role,
        wing_id: Option<i64>,
        squad_id: Option<i64>,
    ) -> DevResult<()> {
        self.character(character_id)?;
        self.fleet(fleet_id)?;
        if self.fleet_of(character_id).map(|(f, _)| f.fleet_id) == Some(fleet_id) {
            return Err(DevError::unprocessable(format!(
                "Character is already in a fleet, character_id: {character_id}"
            )));
        }
        let (wing_id, squad_id) = Self::position(&self.fleets[&fleet_id], role, wing_id, squad_id)?;
        self.leave_fleet(character_id);
        self.invites.remove(&character_id);
        let fleet = self.fleets.get_mut(&fleet_id).ok_or_else(|| {
            // The character was the last member of this very fleet: impossible, checked above.
            DevError::not_found("Fleet not found")
        })?;
        fleet.members.push(Member {
            character_id,
            role,
            wing_id,
            squad_id,
            join_time: SeedTime::now(),
            takes_fleet_warp: true,
        });
        self.event(
            "joined",
            Some(character_id),
            Some(fleet_id),
            format!(
                "{} joined fleet {fleet_id} ({wing_id}/{squad_id})",
                self.character_name(character_id)
            ),
        );
        Ok(())
    }

    /// Take a character out of its fleet; returns the fleet. The boss role passes to another
    /// member (the fleet commander first); an empty fleet disbands.
    pub fn leave_fleet(&mut self, character_id: i64) -> Option<i64> {
        let fleet_id = self.fleet_of(character_id)?.0.fleet_id;
        let fleet = self.fleets.get_mut(&fleet_id)?;
        fleet.members.retain(|m| m.character_id != character_id);
        let name = self.character_name(character_id);
        let fleet = self.fleets.get_mut(&fleet_id)?;
        if fleet.members.is_empty() {
            self.fleets.remove(&fleet_id);
            self.invites.retain(|_, i| i.fleet_id != fleet_id);
            self.event(
                "left",
                Some(character_id),
                Some(fleet_id),
                format!("{name} left fleet {fleet_id}; the fleet disbanded"),
            );
            return Some(fleet_id);
        }
        if fleet.boss_id == character_id {
            let next = fleet
                .members
                .iter()
                .find(|m| m.role == Role::FleetCommander)
                .unwrap_or(&fleet.members[0])
                .character_id;
            fleet.boss_id = next;
        }
        self.event(
            "left",
            Some(character_id),
            Some(fleet_id),
            format!("{name} left fleet {fleet_id}"),
        );
        Some(fleet_id)
    }

    /// Disband a fleet.
    pub fn disband(&mut self, fleet_id: i64) -> DevResult<()> {
        self.fleets
            .remove(&fleet_id)
            .ok_or_else(|| DevError::not_found("Fleet not found"))?;
        self.invites.retain(|_, i| i.fleet_id != fleet_id);
        self.event(
            "disbanded",
            None,
            Some(fleet_id),
            format!("fleet {fleet_id} disbanded"),
        );
        Ok(())
    }

    /// Make a member the fleet boss.
    pub fn set_boss(&mut self, fleet_id: i64, character_id: i64) -> DevResult<()> {
        let fleet = self
            .fleets
            .get_mut(&fleet_id)
            .ok_or_else(|| DevError::not_found("Fleet not found"))?;
        if !fleet.members.iter().any(|m| m.character_id == character_id) {
            return Err(DevError::unprocessable("Character is not in this fleet"));
        }
        fleet.boss_id = character_id;
        self.event(
            "boss",
            Some(character_id),
            Some(fleet_id),
            format!("{} is now the boss", self.character_name(character_id)),
        );
        Ok(())
    }

    /// `PUT /fleets/{id}`.
    pub fn update_fleet(
        &mut self,
        fleet_id: i64,
        caller: i64,
        motd: Option<String>,
        is_free_move: Option<bool>,
    ) -> DevResult<()> {
        self.fleet_for_boss(fleet_id, caller)?;
        let fleet = self.fleets.get_mut(&fleet_id).expect("checked");
        if let Some(motd) = motd {
            fleet.motd = motd;
        }
        if let Some(free) = is_free_move {
            fleet.is_free_move = free;
        }
        let motd = fleet.motd.clone();
        self.event(
            "motd",
            Some(caller),
            Some(fleet_id),
            format!("fleet settings updated; MOTD: {motd}"),
        );
        Ok(())
    }

    /// `POST /fleets/{id}/members` as `caller`. Returns the invite and whether the pilot accepts
    /// it by itself.
    pub fn invite(
        &mut self,
        fleet_id: i64,
        caller: i64,
        character_id: i64,
        role: Role,
        wing_id: Option<i64>,
        squad_id: Option<i64>,
    ) -> DevResult<(Invite, bool)> {
        let fleet = self.fleet_for_boss(fleet_id, caller)?;
        let Some(character) = self.characters.get(&character_id) else {
            return Err(DevError::unprocessable(format!(
                "Character not found, character_id: {character_id}"
            )));
        };
        if self.fleet_of(character_id).is_some() {
            return Err(DevError::unprocessable(format!(
                "Character is already in a fleet, character_id: {character_id}"
            )));
        }
        if matches!(role, Role::SquadMember | Role::SquadCommander)
            && (squad_id.is_none() || wing_id.is_none())
        {
            return Err(DevError::unprocessable(
                "squad_id and wing_id are required for this role",
            ));
        }
        let (wing_id, squad_id) = Self::position(fleet, role, wing_id, squad_id)?;
        let auto_accept = character.auto_accept_invites;
        let invite = Invite {
            id: self.next_id(),
            fleet_id,
            character_id,
            invited_by: caller,
            role,
            wing_id,
            squad_id,
            invited_at: SeedTime::now(),
        };
        self.invites.insert(character_id, invite.clone());
        self.event(
            "invite",
            Some(character_id),
            Some(fleet_id),
            format!(
                "{} invited {} ({wing_id}/{squad_id})",
                self.character_name(caller),
                self.character_name(character_id)
            ),
        );
        Ok((invite, auto_accept))
    }

    /// The pilot accepts its pending invite (only invite `invite_id` when given).
    pub fn accept_invite(&mut self, character_id: i64, invite_id: Option<i64>) -> DevResult<i64> {
        let invite = self
            .invites
            .get(&character_id)
            .filter(|i| invite_id.is_none_or(|id| id == i.id))
            .cloned()
            .ok_or_else(|| DevError::not_found("No pending invite"))?;
        self.invites.remove(&character_id);
        let fleet = self
            .fleets
            .get(&invite.fleet_id)
            .ok_or_else(|| DevError::not_found("The fleet no longer exists"))?;
        // The squad may have gone (or the FC seat filled) meanwhile: then any squad.
        let (wing_id, squad_id) = Self::position(
            fleet,
            invite.role,
            Some(invite.wing_id),
            Some(invite.squad_id),
        )
        .or_else(|_| Self::position(fleet, Role::SquadMember, None, None))?;
        let role = if wing_id == invite.wing_id && squad_id == invite.squad_id {
            invite.role
        } else {
            Role::SquadMember
        };
        self.event(
            "invite_accepted",
            Some(character_id),
            Some(invite.fleet_id),
            format!("{} accepted the invite", self.character_name(character_id)),
        );
        self.join_fleet(
            invite.fleet_id,
            character_id,
            role,
            Some(wing_id),
            Some(squad_id),
        )?;
        Ok(invite.fleet_id)
    }

    pub fn decline_invite(&mut self, character_id: i64) -> DevResult<()> {
        let invite = self
            .invites
            .remove(&character_id)
            .ok_or_else(|| DevError::not_found("No pending invite"))?;
        self.event(
            "invite_declined",
            Some(character_id),
            Some(invite.fleet_id),
            format!("{} declined the invite", self.character_name(character_id)),
        );
        Ok(())
    }

    /// `DELETE /fleets/{id}/members/{member}` as `caller`.
    pub fn kick(&mut self, fleet_id: i64, caller: i64, member_id: i64) -> DevResult<()> {
        let fleet = self.fleet_for_boss(fleet_id, caller)?;
        if !fleet.members.iter().any(|m| m.character_id == member_id) {
            return Err(DevError::not_found("Not found"));
        }
        self.event(
            "kicked",
            Some(member_id),
            Some(fleet_id),
            format!(
                "{} kicked {}",
                self.character_name(caller),
                self.character_name(member_id)
            ),
        );
        self.leave_fleet(member_id);
        Ok(())
    }

    /// `PUT /fleets/{id}/members/{member}` as `caller`: move a member.
    pub fn move_member(
        &mut self,
        fleet_id: i64,
        caller: i64,
        member_id: i64,
        role: Role,
        wing_id: Option<i64>,
        squad_id: Option<i64>,
    ) -> DevResult<()> {
        let fleet = self.fleet_for_boss(fleet_id, caller)?;
        if !fleet.members.iter().any(|m| m.character_id == member_id) {
            return Err(DevError::not_found("Not found"));
        }
        let mut probe = fleet.clone();
        probe.members.retain(|m| m.character_id != member_id);
        let (wing_id, squad_id) = Self::position(&probe, role, wing_id, squad_id)?;
        let fleet = self.fleets.get_mut(&fleet_id).expect("checked");
        if let Some(member) = fleet
            .members
            .iter_mut()
            .find(|m| m.character_id == member_id)
        {
            member.role = role;
            member.wing_id = wing_id;
            member.squad_id = squad_id;
        }
        self.event(
            "moved",
            Some(member_id),
            Some(fleet_id),
            format!(
                "{} moved to {wing_id}/{squad_id}",
                self.character_name(member_id)
            ),
        );
        Ok(())
    }

    /// The `role_name` ESI reports for a member, e.g. `Squad Member (Logi)`.
    pub fn role_name(fleet: &Fleet, member: &Member) -> String {
        let wing = fleet.wings.iter().find(|w| w.id == member.wing_id);
        let squad = wing.and_then(|w| w.squads.iter().find(|s| s.id == member.squad_id));
        let mut name = match member.role {
            Role::FleetCommander => "Fleet Commander".to_string(),
            Role::WingCommander => {
                format!("Wing Commander ({})", wing.map_or("", |w| w.name.as_str()))
            }
            Role::SquadCommander => {
                format!(
                    "Squad Commander ({})",
                    squad.map_or("", |s| s.name.as_str())
                )
            }
            Role::SquadMember => {
                format!("Squad Member ({})", squad.map_or("", |s| s.name.as_str()))
            }
        };
        if member.character_id == fleet.boss_id {
            name.push_str(" (Boss)");
        }
        name
    }

    // --- incursions and wars ---------------------------------------------------------------

    pub fn set_incursions(&mut self, incursions: Vec<Incursion>) {
        self.incursions = incursions;
        self.incursions_modified = Utc::now();
        self.event(
            "incursions",
            None,
            None,
            format!("{} incursions set", self.incursions.len()),
        );
    }

    pub fn set_wars(&mut self, wars: Vec<War>) {
        self.wars = wars.into_iter().map(|w| (w.id, w)).collect();
        self.event("wars", None, None, format!("{} wars set", self.wars.len()));
    }
}

fn settings_of(seed: &Seed) -> Settings {
    Settings {
        invite_accept_delay_ms: seed.fleet.invite_accept_delay_ms,
        default_layout: seed.fleet.default_layout.clone(),
        incursions_cache_secs: seed.esi.incursions_cache_secs,
        access_token_lifetime_secs: seed.sso.access_token_lifetime_secs,
        issuer: seed.sso.issuer.clone(),
        clients: seed.sso_clients(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FC: i64 = 2120000011;
    const PILOT: i64 = 2120000005;
    const OTHER: i64 = 2120000004;

    fn world() -> World {
        World::new(SeedSource::default_seed()).unwrap()
    }

    #[test]
    fn the_seed_builds_a_world() {
        let world = world();
        assert_eq!(world.fleets.len(), 1, "the seeded fleet");
        let (fleet, member) = world.fleet_of(2120000001).unwrap();
        assert_eq!(fleet.boss_id, 2120000001);
        assert_eq!(member.role, Role::FleetCommander);
        assert_eq!(fleet.wings.len(), 2);
        let skills = world.skills_of(&world.characters[&2120000006]);
        assert!(!skills.contains_key(&28667), "Marauders removed");
        assert_eq!(skills.get(&3336), Some(&5), "Gallente Battleship");
        assert_eq!(world.wallets(98800003).unwrap().len(), 7);
        assert!(
            world
                .killmails
                .values()
                .all(|k| k.killmail_hash.len() == 40)
        );
    }

    #[test]
    fn invites_join_the_requested_squad() {
        let mut world = world();
        let fleet_id = world.create_fleet(FC).unwrap();
        let squad = world.fleets[&fleet_id].wings[0].squads[1].clone();
        let wing_id = world.fleets[&fleet_id].wings[0].id;

        // Only the boss may invite.
        let err = world
            .invite(
                fleet_id,
                PILOT,
                OTHER,
                Role::SquadMember,
                Some(wing_id),
                Some(squad.id),
            )
            .unwrap_err();
        assert_eq!(err, DevError::not_found(NO_FLEET_ACCESS));
        let err = world
            .invite(
                fleet_id,
                FC,
                PILOT,
                Role::SquadMember,
                Some(wing_id),
                Some(-5),
            )
            .unwrap_err();
        assert_eq!(err.status, 422);

        let (invite, auto) = world
            .invite(
                fleet_id,
                FC,
                PILOT,
                Role::SquadMember,
                Some(wing_id),
                Some(squad.id),
            )
            .unwrap();
        assert!(auto);
        assert_eq!(world.accept_invite(PILOT, Some(invite.id)), Ok(fleet_id));
        let (fleet, member) = world.fleet_of(PILOT).unwrap();
        assert_eq!((member.wing_id, member.squad_id), (wing_id, squad.id));
        assert_eq!(World::role_name(fleet, member), "Squad Member (Bastion)");

        // Already in a fleet.
        let err = world
            .invite(
                fleet_id,
                FC,
                PILOT,
                Role::SquadMember,
                Some(wing_id),
                Some(squad.id),
            )
            .unwrap_err();
        assert_eq!(
            err.message,
            format!("Character is already in a fleet, character_id: {PILOT}")
        );
        assert!(world.accept_invite(PILOT, None).is_err(), "nothing pending");
    }

    #[test]
    fn the_boss_role_passes_on_and_empty_fleets_disband() {
        let mut world = world();
        let fleet_id = world.create_fleet(FC).unwrap();
        world
            .join_fleet(fleet_id, PILOT, Role::SquadMember, None, None)
            .unwrap();
        assert_eq!(world.leave_fleet(FC), Some(fleet_id));
        assert_eq!(world.fleets[&fleet_id].boss_id, PILOT);
        world.kick(fleet_id, PILOT, PILOT).unwrap();
        assert!(!world.fleets.contains_key(&fleet_id));
        assert!(world.fleet_of(PILOT).is_none());
        assert_eq!(
            world.kick(fleet_id, PILOT, PILOT).unwrap_err(),
            DevError::not_found(NO_FLEET_ACCESS)
        );
    }

    #[test]
    fn journal_entries_move_the_balance() {
        let mut world = world();
        let entry = JournalEntry {
            division: 1,
            id: 0,
            date: SeedTime::now(),
            ref_type: "player_donation".into(),
            description: String::new(),
            amount: Some(35_000_000.0),
            balance: None,
            context_id: None,
            context_id_type: None,
            first_party_id: Some(PILOT),
            second_party_id: Some(98800003),
            reason: None,
            tax: None,
            tax_receiver_id: None,
        };
        let added = world.add_journal_entry(98800003, 1, entry).unwrap();
        assert_ne!(added.id, 0);
        assert_eq!(added.balance, Some(1_285_000_000.0));
        assert_eq!(
            added.description,
            "Dev Vindi Tarek deposited cash into Dev SRP Holding's account"
        );
        assert_eq!(world.wallets(98800003).unwrap()[0].balance, 1_285_000_000.0);
    }

    #[test]
    fn reset_restores_the_seed() {
        let mut world = world();
        world.disband(*world.fleets.keys().next().unwrap()).unwrap();
        world.characters.remove(&PILOT);
        world.reset().unwrap();
        assert_eq!(world.fleets.len(), 1);
        assert!(world.characters.contains_key(&PILOT));
    }
}
