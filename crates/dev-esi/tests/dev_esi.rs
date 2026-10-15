//! dev-esi over HTTP, driven by the same clients the waitlist uses: eve-esi-client's SSO client
//! for logins and its generated ESI client for every endpoint (so responses must parse into the
//! types generated from CCP's spec).

use std::num::NonZeroU32;
use std::time::Duration;

use aws_lc_rs::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use dev_esi::{DevEsi, Running, SeedSource, Signer};
use serde_json::{Value, json};
use wl_esi::eve_esi_client::auth::{AuthError, CsrfToken, SsoClient, TokenSet};
use wl_esi::eve_esi_client::types::{
    GetCharactersCharacterIdSearchCategoriesItem as Category, PostFleetsFleetIdMembersBody,
    PostFleetsFleetIdMembersBodyRole, PutFleetsFleetIdBody,
};
use wl_esi::eve_esi_client::{Client, ErrorExt as _};

const CALLBACK: &str = "http://waitlist.test/auth/cb";
const FC: i64 = 2120000001; // boss of the seeded fleet
const PILOT: i64 = 2120000005;
const LEDGER: i64 = 2120000009; // SRP wallet reader
const SRP_CORP: i64 = 98800003;

const ALL_SCOPES: [&str; 8] = [
    "publicData",
    "esi-skills.read_skills.v1",
    "esi-clones.read_implants.v1",
    "esi-fleets.read_fleet.v1",
    "esi-fleets.write_fleet.v1",
    "esi-ui.open_window.v1",
    "esi-search.search_structures.v1",
    "esi-wallet.read_corporation_wallets.v1",
];

async fn start() -> Running {
    let dev = DevEsi::new(
        SeedSource::default_seed(),
        Signer::generate().unwrap(),
        None,
    )
    .unwrap();
    dev.world().settings.invite_accept_delay_ms = 50;
    dev.spawn("127.0.0.1:0").await.unwrap()
}

fn sso(running: &Running, secret: Option<&str>) -> SsoClient {
    let mut builder = SsoClient::builder("dev-esi-client", CALLBACK)
        .authorize_url(running.authorize_url())
        .token_url(running.token_url());
    if let Some(secret) = secret {
        builder = builder.client_secret(secret);
    }
    builder.build().unwrap()
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

/// The callback query of picking `character_id` on the login page at `authorize_url`.
async fn pick(authorize_url: &str, character_id: i64) -> Vec<(String, String)> {
    let res = http()
        .get(format!("{authorize_url}&character_id={character_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 302, "{}", res.text().await.unwrap());
    let location = res.headers()["location"].to_str().unwrap().to_string();
    assert!(location.starts_with(CALLBACK), "{location}");
    url::Url::parse(&location)
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn param(query: &[(String, String)], name: &str) -> Option<String> {
    query
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
}

/// Log `character_id` in with `scopes` the way the waitlist does (client secret, no PKCE).
async fn login(running: &Running, character_id: i64, scopes: &[&str]) -> TokenSet {
    let sso = sso(running, Some("dev-esi-secret"));
    let url = sso
        .authorize_without_pkce(scopes.iter().copied(), CsrfToken::new("normal".into()))
        .unwrap();
    let code = param(&pick(&url, character_id).await, "code").unwrap();
    sso.exchange_without_pkce(code).await.unwrap()
}

fn esi(running: &Running, token: &TokenSet) -> Client {
    Client::builder()
        .user_agent("dev-esi tests")
        .base_url(running.url.clone())
        .build()
        .unwrap()
        .with_access_token(token.access_token.clone())
}

fn claims(token: &str) -> Value {
    serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(token.split('.').nth(1).unwrap())
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn the_login_page_lists_characters_and_keeps_the_state() {
    let running = start().await;
    let state = "alt:3f9c a+b/c&d=é";
    let url = sso(&running, Some("dev-esi-secret"))
        .authorize_without_pkce(["publicData"], CsrfToken::new(state.into()))
        .unwrap();

    let page = http().get(&url).send().await.unwrap();
    assert_eq!(page.status(), 200);
    let html = page.text().await.unwrap();
    assert!(html.contains("Dev FC Aurora") && html.contains("Dev Banned Rook"));
    assert!(html.contains("<code>publicData</code>"));

    // Submitting the page's form.
    let form: Vec<(String, String)> = url::Url::parse(&url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .chain([("character_id".to_string(), PILOT.to_string())])
        .collect();
    let res = http()
        .post(running.authorize_url())
        .form(&form)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 302);
    let location = url::Url::parse(res.headers()["location"].to_str().unwrap()).unwrap();
    let query: Vec<(String, String)> = location.query_pairs().into_owned().collect();
    assert_eq!(param(&query, "state").as_deref(), Some(state));
    assert!(param(&query, "code").is_some());

    // Cancelling.
    let res = http().get(format!("{url}&deny=1")).send().await.unwrap();
    let location = res.headers()["location"].to_str().unwrap();
    assert!(location.contains("error=access_denied"), "{location}");

    // An unknown application gets an error page, not a redirect.
    let bad = url.replace("client_id=dev-esi-client", "client_id=nope");
    assert_eq!(http().get(bad).send().await.unwrap().status(), 400);
}

#[tokio::test]
async fn tokens_carry_eve_claims_and_verify_against_the_jwks() {
    let running = start().await;
    let scopes = ["publicData", "esi-skills.read_skills.v1"];
    let tokens = login(&running, PILOT, &scopes).await;

    let c = claims(&tokens.access_token);
    assert_eq!(c["sub"], format!("CHARACTER:EVE:{PILOT}"));
    assert_eq!(c["name"], "Dev Vindi Tarek");
    assert_eq!(c["scp"], json!(scopes));
    assert_eq!(c["aud"], json!(["dev-esi-client", "EVE Online"]));
    assert_eq!(c["azp"], "dev-esi-client");
    assert_eq!(c["iss"], "https://login.eveonline.com");
    assert_eq!(c["owner"].as_str().unwrap().len(), 28);
    assert!(c["exp"].as_i64().unwrap() > c["iat"].as_i64().unwrap());
    let parsed = tokens.claims().unwrap();
    assert_eq!(parsed.character_id, Some(PILOT as u64));
    assert_eq!(parsed.scopes, scopes);

    // The signature checks out against the published key.
    let jwks: Value = reqwest::get(format!("{}/oauth/jwks", running.url))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let key = &jwks["keys"][0];
    assert_eq!(key["kid"], "JWT-Signature-Key");
    let decode = |field: &str| {
        URL_SAFE_NO_PAD
            .decode(key[field].as_str().unwrap())
            .unwrap()
    };
    let (n, e) = (decode("n"), decode("e"));
    let (input, signature) = tokens.access_token.rsplit_once('.').unwrap();
    RsaPublicKeyComponents { n: &n, e: &e }
        .verify(
            &RSA_PKCS1_2048_8192_SHA256,
            input.as_bytes(),
            &URL_SAFE_NO_PAD.decode(signature).unwrap(),
        )
        .expect("valid RS256 signature");

    // One scope is a string, as EVE sends it.
    let one = login(&running, PILOT, &["publicData"]).await;
    assert_eq!(claims(&one.access_token)["scp"], "publicData");
}

#[tokio::test]
async fn codes_are_single_use_and_refresh_tokens_rotate() {
    let running = start().await;
    let sso = sso(&running, Some("dev-esi-secret"));
    let url = sso
        .authorize_without_pkce(
            ["publicData", "esi-skills.read_skills.v1"],
            CsrfToken::new("s".into()),
        )
        .unwrap();
    let code = param(&pick(&url, PILOT).await, "code").unwrap();
    let tokens = sso.exchange_without_pkce(code.clone()).await.unwrap();
    let reused = sso.exchange_without_pkce(code).await.unwrap_err();
    assert!(reused.is_revoked(), "{reused}");

    // Narrower scopes on refresh are fine; the old refresh token is spent.
    let refresh = tokens.refresh_token.clone().unwrap();
    let narrowed = sso
        .refresh_with_scopes(&refresh, ["publicData"])
        .await
        .unwrap();
    assert_eq!(narrowed.claims().unwrap().scopes, ["publicData"]);
    let again = sso.refresh(&refresh).await.unwrap_err();
    assert!(again.is_revoked(), "{again}");

    // The rotated token still carries the whole grant, but nothing more.
    let next = narrowed.refresh_token.unwrap();
    let widened = sso
        .refresh_with_scopes(&next, ["esi-fleets.read_fleet.v1"])
        .await
        .unwrap_err();
    assert!(
        matches!(&widened, AuthError::Rejected { error, .. } if error == "invalid_scope"),
        "{widened}"
    );
    assert!(!widened.is_revoked());
    let full = sso.refresh(&next).await.unwrap();
    assert_eq!(full.claims().unwrap().scopes.len(), 2);

    // Revoking through the control API ends the grant.
    let res = http()
        .post(format!("{}/_dev/characters/{PILOT}/revoke", running.url))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let revoked = sso.refresh(&full.refresh_token.unwrap()).await.unwrap_err();
    assert!(revoked.is_revoked(), "{revoked}");

    // A wrong secret is the client's fault, not the token's.
    let wrong = sso_with_secret(&running, "wrong")
        .refresh(&next)
        .await
        .unwrap_err();
    assert!(
        matches!(&wrong, AuthError::Rejected { error, .. } if error == "invalid_client"),
        "{wrong}"
    );
}

fn sso_with_secret(running: &Running, secret: &str) -> SsoClient {
    sso(running, Some(secret))
}

#[tokio::test]
async fn public_clients_use_pkce() {
    let running = start().await;
    let sso = sso(&running, None);
    let pending = sso.authorize(["publicData"]);
    let query = pick(&pending.url, FC).await;
    assert_eq!(
        param(&query, "state").as_deref(),
        Some(pending.csrf_state.secret().as_str())
    );
    let code = param(&query, "code").unwrap();
    let tokens = sso.exchange(code, pending.pkce_verifier).await.unwrap();
    assert_eq!(tokens.character_id(), Some(FC as u64));

    // The wrong verifier is refused.
    let pending = sso.authorize(["publicData"]);
    let other = sso.authorize(["publicData"]);
    let code = param(&pick(&pending.url, FC).await, "code").unwrap();
    let err = sso.exchange(code, other.pkce_verifier).await.unwrap_err();
    assert!(err.is_revoked(), "{err}");
}

#[tokio::test]
async fn public_endpoints_speak_esi() {
    let running = start().await;
    let client = Client::builder()
        .user_agent("dev-esi tests")
        .base_url(running.url.clone())
        .build()
        .unwrap();

    let pilot = client
        .get_characters_detail()
        .character_id(PILOT)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(pilot.name, "Dev Vindi Tarek");
    assert_eq!(*pilot.corporation_id, 98800001);
    assert_eq!(pilot.alliance_id.map(|a| *a), Some(99800001));
    let missing = client
        .get_characters_detail()
        .character_id(1)
        .send()
        .await
        .unwrap_err();
    assert_eq!(missing.status().map(|s| s.as_u16()), Some(404));
    assert_eq!(
        missing.esi_error().map(|e| e.error.as_str()),
        Some("Character not found")
    );

    let corp = client
        .get_corporations_corporation_id()
        .corporation_id(98800001)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        (corp.name.as_str(), corp.ticker.as_str()),
        ("Dev Incursion Corp", "DINC")
    );
    let alliance = client
        .get_alliances_alliance_id()
        .alliance_id(99800001)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(alliance.ticker, "DEVIA");

    let affiliations = client
        .post_characters_affiliation()
        .body(vec![PILOT, LEDGER, 42])
        .send()
        .await
        .unwrap()
        .into_inner()
        .0;
    assert_eq!(affiliations.len(), 2);
    let ledger = affiliations
        .iter()
        .find(|a| a.character_id == LEDGER)
        .unwrap();
    assert_eq!(
        (ledger.corporation_id, ledger.alliance_id),
        (SRP_CORP, None)
    );

    let names = client
        .post_universe_names()
        .body(vec![PILOT, SRP_CORP, 500019])
        .send()
        .await
        .unwrap()
        .into_inner()
        .0;
    assert_eq!(names.len(), 3);
    let unknown = client
        .post_universe_names()
        .body(vec![PILOT, 42])
        .send()
        .await
        .unwrap_err();
    assert_eq!(unknown.status().map(|s| s.as_u16()), Some(404));

    let km = client
        .get_killmails_killmail_id_killmail_hash()
        .killmail_id(130000001)
        .killmail_hash("0d1e2f3a4b5c6d7e8f90a1b2c3d4e5f60718293a")
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(km.victim.character_id, Some(PILOT));
    assert_eq!(km.victim.ship_type_id, 17740);
    assert!(!km.victim.items.is_empty());
    let bad_hash = client
        .get_killmails_killmail_id_killmail_hash()
        .killmail_id(130000001)
        .killmail_hash("00")
        .send()
        .await
        .unwrap_err();
    assert_eq!(bad_hash.status().map(|s| s.as_u16()), Some(422));

    let wars = client.get_wars().send().await.unwrap().into_inner().0;
    assert_eq!(wars, [750001]);
    let war = client
        .get_wars_war_id()
        .war_id(750001)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(war.defender.corporation_id, Some(98800005));
    assert!(war.finished.is_none() && war.started.is_some());

    let incursions = client.get_incursions().send().await.unwrap();
    assert!(incursions.headers().contains_key("expires"));
    assert!(incursions.headers().contains_key("last-modified"));
    let incursions = incursions.into_inner().0;
    assert_eq!(incursions.len(), 3);
    assert_eq!(incursions[0].staging_solar_system_id, 30000052);

    // Without static data, the seed's names still resolve; systems don't.
    let system = client
        .get_universe_systems_system_id()
        .system_id(30000052)
        .send()
        .await
        .unwrap_err();
    assert_eq!(system.status().map(|s| s.as_u16()), Some(404));
}

#[tokio::test]
async fn character_endpoints_check_tokens_and_scopes() {
    let running = start().await;
    let tokens = login(&running, PILOT, &ALL_SCOPES).await;
    let client = esi(&running, &tokens);

    let skills = client
        .get_characters_character_id_skills()
        .character_id(PILOT)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert!(skills.skills.len() > 100);
    assert!(skills.total_sp > 0);
    let implants = client
        .get_characters_character_id_implants()
        .character_id(PILOT)
        .send()
        .await
        .unwrap()
        .into_inner()
        .0;
    assert!(implants.contains(&20499));

    // Another character's data, and a scope the token lacks.
    let other = client
        .get_characters_character_id_skills()
        .character_id(FC)
        .send()
        .await
        .unwrap_err();
    assert_eq!(other.status().map(|s| s.as_u16()), Some(403));
    let narrow = login(&running, PILOT, &["publicData"]).await;
    let no_scope = esi(&running, &narrow)
        .get_characters_character_id_implants()
        .character_id(PILOT)
        .send()
        .await
        .unwrap_err();
    assert_eq!(no_scope.status().map(|s| s.as_u16()), Some(403));
    assert_eq!(
        no_scope.esi_error().map(|e| e.error.as_str()),
        Some("token not valid for scope")
    );

    let found = client
        .get_characters_character_id_search()
        .character_id(PILOT)
        .categories(vec![Category::Character, Category::Corporation])
        .search("aurora")
        .strict(false)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(found.character.len(), 3);
    assert!(found.corporation.is_empty());

    let not_in_fleet = client
        .get_characters_character_id_fleet()
        .character_id(PILOT)
        .send()
        .await
        .unwrap_err();
    assert_eq!(not_in_fleet.status().map(|s| s.as_u16()), Some(404));
    assert_eq!(
        not_in_fleet.esi_error().map(|e| e.error.as_str()),
        Some("Character is not in a fleet")
    );

    client
        .post_ui_openwindow_information()
        .target_id(FC)
        .send()
        .await
        .unwrap();
    let events: Vec<Value> = reqwest::get(format!("{}/_dev/events", running.url))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(events.iter().any(|e| e["kind"] == "open_window"));

    // The SRP wallet, read by a member of its corporation only.
    let ledger = esi(&running, &login(&running, LEDGER, &ALL_SCOPES).await);
    let wallets = ledger
        .get_corporations_corporation_id_wallets()
        .corporation_id(SRP_CORP)
        .send()
        .await
        .unwrap()
        .into_inner()
        .0;
    assert_eq!(wallets.len(), 7);
    assert_eq!(wallets[0].balance, 1_250_000_000.0);
    let journal = ledger
        .get_corporations_corporation_id_wallets_division_journal()
        .corporation_id(SRP_CORP)
        .division(1)
        .page(NonZeroU32::MIN)
        .send()
        .await
        .unwrap();
    assert_eq!(journal.headers()["x-pages"], "1");
    let journal = journal.into_inner().0;
    assert_eq!(journal.len(), 2);
    assert!(journal[0].date > journal[1].date, "newest first");
    assert_eq!(journal[0].amount, Some(20_000_000.0));
    let outsider = client
        .get_corporations_corporation_id_wallets()
        .corporation_id(SRP_CORP)
        .send()
        .await
        .unwrap_err();
    assert_eq!(outsider.status().map(|s| s.as_u16()), Some(403));
}

#[tokio::test]
async fn fleets_invite_and_pilots_accept() {
    let running = start().await;
    let boss = esi(&running, &login(&running, FC, &ALL_SCOPES).await);

    let fleet = boss
        .get_characters_character_id_fleet()
        .character_id(FC)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fleet.fleet_boss_id, FC);
    let fleet_id = fleet.fleet_id;
    let wings = boss
        .get_fleets_fleet_id_wings()
        .fleet_id(fleet_id)
        .send()
        .await
        .unwrap()
        .into_inner()
        .0;
    let (wing_id, squad_id) = (wings[0].id, wings[0].squads[2].id);

    boss.put_fleets_fleet_id()
        .fleet_id(fleet_id)
        .body(PutFleetsFleetIdBody {
            is_free_move: None,
            motd: Some("x up".into()),
        })
        .send()
        .await
        .unwrap();
    let info = boss
        .get_fleets_fleet_id()
        .fleet_id(fleet_id)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(info.motd, "x up");

    let invite = |character_id| PostFleetsFleetIdMembersBody {
        character_id,
        role: PostFleetsFleetIdMembersBodyRole::SquadMember,
        squad_id: Some(squad_id),
        wing_id: Some(wing_id),
    };
    boss.post_fleets_fleet_id_members()
        .fleet_id(fleet_id)
        .body(invite(PILOT))
        .send()
        .await
        .unwrap();
    // The pilot accepts after the (shortened) delay and lands in the squad.
    let mut joined = None;
    for _ in 0..50 {
        let members = boss
            .get_fleets_fleet_id_members()
            .fleet_id(fleet_id)
            .send()
            .await
            .unwrap()
            .into_inner()
            .0;
        if let Some(m) = members.into_iter().find(|m| m.character_id == PILOT) {
            joined = Some(m);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let member = joined.expect("the pilot joined");
    assert_eq!((member.wing_id, member.squad_id), (wing_id, squad_id));
    assert_eq!(member.ship_type_id, 17740);
    assert_eq!(member.role_name, "Squad Member (Other)");

    // Inviting again: already in a fleet.
    let again = boss
        .post_fleets_fleet_id_members()
        .fleet_id(fleet_id)
        .body(invite(PILOT))
        .send()
        .await
        .unwrap_err();
    assert_eq!(again.status().map(|s| s.as_u16()), Some(422));
    assert_eq!(
        again.esi_error().map(|e| e.error.as_str()),
        Some(format!("Character is already in a fleet, character_id: {PILOT}").as_str())
    );

    // Only the boss sees the fleet.
    let pilot = esi(&running, &login(&running, PILOT, &ALL_SCOPES).await);
    let not_boss = pilot
        .get_fleets_fleet_id_members()
        .fleet_id(fleet_id)
        .send()
        .await
        .unwrap_err();
    assert_eq!(not_boss.status().map(|s| s.as_u16()), Some(404));

    boss.delete_fleets_fleet_id_members_member_id()
        .fleet_id(fleet_id)
        .member_id(PILOT)
        .send()
        .await
        .unwrap();
    let gone = boss
        .delete_fleets_fleet_id_members_member_id()
        .fleet_id(fleet_id)
        .member_id(PILOT)
        .send()
        .await
        .unwrap_err();
    assert_eq!(gone.status().map(|s| s.as_u16()), Some(404));

    // A pilot without auto-accept waits for the control API.
    let control = |path: &str| format!("{}/_dev/{path}", running.url);
    http()
        .patch(control(&format!("characters/{PILOT}")))
        .json(&json!({"auto_accept_invites": false}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    boss.post_fleets_fleet_id_members()
        .fleet_id(fleet_id)
        .body(invite(PILOT))
        .send()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(running.dev.world().fleet_of(PILOT).is_none());
    let accepted: Value = http()
        .post(control(&format!("invites/{PILOT}/accept")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(accepted["fleet_id"], fleet_id);
    assert!(running.dev.world().fleet_of(PILOT).is_some());
}

#[tokio::test]
async fn the_control_api_changes_the_world() {
    let running = start().await;
    let control = |path: &str| format!("{}/_dev/{path}", running.url);
    let http = http();

    // A new fleet for a second FC, a pilot placed in a squad by name, a new boss.
    let fleet: Value = http
        .post(control("fleets"))
        .json(&json!({"boss_id": 2120000011}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let fleet_id = fleet["fleet_id"].as_i64().unwrap();
    let fleet: Value = http
        .post(control(&format!("fleets/{fleet_id}/members")))
        .json(&json!({"character_id": PILOT, "squad": "Logi"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fleet["members"].as_array().unwrap().len(), 2);
    http.put(control(&format!("fleets/{fleet_id}/boss")))
        .json(&json!({"character_id": PILOT}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let pilot: Value = http
        .get(control(&format!("characters/{PILOT}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pilot["fleet"]["is_boss"], true);
    assert_eq!(pilot["fleet"]["role_name"], "Squad Member (Logi) (Boss)");

    // Skills and implants.
    let res: Value = http
        .put(control(&format!("characters/{PILOT}/skills")))
        .json(&json!({"skill_set": null, "skills": {"3336": 4}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["effective_skills"], json!({"3336": 4}));
    let res: Value = http
        .put(control(&format!("characters/{PILOT}/implants")))
        .json(&json!([3119]))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["implants"], json!([3119]));
    let bad = http
        .patch(control(&format!("characters/{PILOT}")))
        .json(&json!({"corporation_id": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);

    // A killmail and a wallet deposit.
    let km: Value = http
        .post(control("killmails"))
        .json(&json!({
            "solar_system_id": 30000052,
            "victim": {"character_id": PILOT, "ship_type_id": 17740, "damage_taken": 1}
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(km["killmail_id"], 130000003);
    assert!(
        km["link"]
            .as_str()
            .unwrap()
            .contains("/killmails/130000003/")
    );
    let entry: Value = http
        .post(control(&format!("corporations/{SRP_CORP}/journal")))
        .json(&json!({"amount": 45000000.0, "first_party_id": PILOT, "second_party_id": SRP_CORP}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(entry["balance"], 1_295_000_000.0);
    assert_eq!(
        entry["description"],
        "Dev Vindi Tarek deposited cash into Dev SRP Holding's account"
    );

    // Incursions, settings, reset.
    http.put(control("incursions"))
        .json(&json!([]))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert!(running.dev.world().incursions.is_empty());
    let settings: Value = http
        .put(control("settings"))
        .json(&json!({"invite_accept_delay_ms": 10}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(settings["invite_accept_delay_ms"], 10);
    http.post(control("reset"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let state: Value = http
        .get(control("state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(state["fleets"].as_array().unwrap().len(), 1);
    assert_eq!(state["incursions"].as_array().unwrap().len(), 3);
    assert_eq!(state["killmails"].as_array().unwrap().len(), 2);

    // The dashboard.
    let page = http.get(control("")).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert!(page.text().await.unwrap().contains("dev-esi"));
}

#[tokio::test]
async fn a_state_dir_keeps_tokens_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let start_in = |dir: &std::path::Path| {
        let dev = DevEsi::new(
            SeedSource::default_seed(),
            Signer::load_or_create(&dir.join("signing-key.der")).unwrap(),
            None,
        )
        .unwrap();
        dev.persist_grants(dir.join("refresh-tokens.json")).unwrap();
        dev
    };
    let first = start_in(dir.path()).spawn("127.0.0.1:0").await.unwrap();
    let tokens = login(&first, PILOT, &["publicData"]).await;
    let refresh = tokens.refresh_token.clone().unwrap();
    // The waitlist stores refresh tokens in a varchar(255), as EVE's are short.
    assert!(refresh.len() < 64, "{refresh}");
    drop(first);

    let second = start_in(dir.path()).spawn("127.0.0.1:0").await.unwrap();
    assert!(second.dev.signer().verify(&tokens.access_token).is_some());
    let refreshed = sso(&second, Some("dev-esi-secret"))
        .refresh(&refresh)
        .await
        .unwrap();
    assert_eq!(refreshed.character_id(), Some(PILOT as u64));
}
