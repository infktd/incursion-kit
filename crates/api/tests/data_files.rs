//! Endpoints backed by the community data store: admin data files, categories and skill history.
//!
//! These need the real static data file (`deploy/sde/sde.sqlite` or `WL_SDE_PATH`) and skip
//! without it, like the wl-fitting tests. The data set is wl-fitting's example data.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::http::Method;
use common::TestApp;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tokio::sync::OnceCell;
use wl_eve_data::EveData;
use wl_fitting::DataStore;

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

async fn eve() -> Option<Arc<EveData>> {
    static EVE: OnceCell<Option<Arc<EveData>>> = OnceCell::const_new();
    EVE.get_or_init(|| async {
        let path = std::env::var_os("WL_SDE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace().join("deploy/sde/sde.sqlite"));
        if !path.exists() {
            eprintln!("skipping: static data file {} not found", path.display());
            return None;
        }
        Some(Arc::new(
            EveData::load(&path).await.expect("static data loads"),
        ))
    })
    .await
    .clone()
}

/// Write wl-fitting's bundled example data set into `dir`.
fn copy_examples(dir: &Path) {
    wl_fitting::example_data::seed(dir, true).expect("write example data");
}

async fn app() -> Option<TestApp> {
    let eve = eve().await?;
    TestApp::with_services(move |state, _, dir| {
        copy_examples(dir);
        let store = DataStore::open(eve.clone(), dir).expect("example data loads");
        state.with_eve_data(eve).with_data_store(Arc::new(store))
    })
    .await
}

#[tokio::test]
async fn categories() {
    let Some(app) = app().await else { return };
    let user = app.create_account(1001, "User", None).await;
    let res = app.get("/api/categories", Some(&user)).await;
    assert_eq!(res.status, 200, "{}", res.text());
    let expected = app
        .state
        .data_store()
        .unwrap()
        .get()
        .categories
        .categories
        .clone();
    assert!(!expected.is_empty());
    assert_eq!(res.json::<Value>(), json!({"categories": expected}));

    let res = app.get("/api/categories", None).await;
    assert_eq!(res.status, 401);
}

#[tokio::test]
async fn admin_lists_reads_and_reloads_data_files() {
    let Some(app) = app().await else { return };
    let admin = app.create_account(1001, "Admin", Some("admin")).await;

    let files: Value = app.get("/api/admin/data-files", Some(&admin)).await.json();
    let names: Vec<_> = files["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        [
            "categories.yaml",
            "fitnotes.yaml",
            "fits.dat",
            "implants.yaml",
            "modules.yaml",
            "skillplan.yaml",
            "skills.yaml",
            "tags.yaml",
        ]
    );
    let fits = &files["files"][2];
    assert_eq!(fits["file_type"], "DAT");
    assert_eq!(fits["requires_reload"], true);
    assert_eq!(files["files"][1]["requires_reload"], false);

    let on_disk = std::fs::read_to_string(app.data_dir.path().join("tags.yaml")).unwrap();
    let res = app
        .get("/api/admin/data-files/tags.yaml", Some(&admin))
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (200, on_disk.as_str()));

    let res = app
        .send_raw(
            Method::POST,
            "/api/admin/data-files/skills.yaml/reload",
            Some(&admin),
            "",
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (200, "File reloaded successfully")
    );
    let res = app
        .send_raw(
            Method::POST,
            "/api/admin/data-files/fitnotes.yaml/reload",
            Some(&admin),
            "",
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "File does not require reload")
    );
    let res = app
        .send_raw(
            Method::POST,
            "/api/admin/data-files/home.en.json/reload",
            Some(&admin),
            "",
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Unknown file type")
    );

    // A broken file on disk fails the reload and keeps the loaded data.
    std::fs::write(app.data_dir.path().join("tags.yaml"), "public_tags: [").unwrap();
    let res = app
        .send_raw(
            Method::POST,
            "/api/admin/data-files/tags.yaml/reload",
            Some(&admin),
            "",
        )
        .await;
    assert_eq!(res.status, 400);
    assert!(
        res.text().starts_with("Failed to reload tags: "),
        "{}",
        res.text()
    );
    assert!(
        app.state
            .data_store()
            .unwrap()
            .get()
            .public_tags
            .contains("DPS")
    );
}

#[tokio::test]
async fn admin_saves_data_files_only_when_valid() {
    let Some(app) = app().await else { return };
    let admin = app.create_account(1001, "Admin", Some("admin")).await;
    let path = app.data_dir.path().join("categories.yaml");
    let before = std::fs::read_to_string(&path).unwrap();

    // An unknown item: rejected, and nothing is written.
    let bad = format!("{before}\n  - item: Not A Real Ship\n    category: dps\n");
    let res = app
        .send_raw(
            Method::POST,
            "/api/admin/data-files/categories.yaml",
            Some(&admin),
            bad,
        )
        .await;
    assert_eq!(res.status, 400, "{}", res.text());
    assert!(res.text().contains("Not A Real Ship"), "{}", res.text());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

    let res = app
        .send_raw(
            Method::POST,
            "/api/admin/data-files/tags.yaml",
            Some(&admin),
            "public_tags: [nope",
        )
        .await;
    assert_eq!(res.status, 400);

    // A valid edit is written (with a backup) and loaded.
    let good = "public_tags:\n  - HQ-FC\n  - NEWTAG\n";
    let res = app
        .send_raw(
            Method::POST,
            "/api/admin/data-files/tags.yaml",
            Some(&admin),
            good,
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (200, "File saved successfully")
    );
    assert_eq!(
        std::fs::read_to_string(app.data_dir.path().join("tags.yaml")).unwrap(),
        good
    );
    assert!(
        app.state
            .data_store()
            .unwrap()
            .get()
            .public_tags
            .contains("NEWTAG")
    );
    let backups = std::fs::read_dir(app.data_dir.path())
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("tags.yaml.backup.")
        })
        .count();
    assert_eq!(backups, 1);

    // The new rules file is not part of the legacy editor.
    let res = app
        .get("/api/admin/data-files/rules.yaml", Some(&admin))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "File not editable")
    );
}

#[tokio::test]
async fn skill_history_is_limited_to_relevant_skills() {
    let Some(app) = app().await else { return };
    let main = app.create_account(1001, "Main", None).await;
    let stranger = app.create_account(1002, "Stranger", None).await;
    let fc = app.create_account(1003, "Fc", Some("fc")).await;

    let data = app.state.data_store().unwrap().get();
    let (relevant_name, relevant_id) = data
        .skills
        .name_lookup
        .iter()
        .next()
        .map(|(name, id)| (name.clone(), *id))
        .expect("example data has skills");
    let irrelevant = data
        .eve()
        .id_of("Mining")
        .expect("Mining skill in the static data");
    assert!(!data.skills.relevant_skills.contains(&irrelevant));

    for (skill, old, new, at) in [
        (relevant_id, 3, 4, 100),
        (irrelevant, 0, 1, 200),
        (relevant_id, 4, 5, 300),
    ] {
        sqlx::query(
            "INSERT INTO skill_history (character_id, skill_id, old_level, new_level, logged_at)
             VALUES (1001, ?, ?, ?, ?)",
        )
        .bind(skill)
        .bind(old)
        .bind(new)
        .bind(at)
        .execute(app.pool())
        .await
        .unwrap();
    }

    for login in [&main, &fc] {
        let res: Value = app
            .get("/api/history/skills?character_id=1001", Some(login))
            .await
            .json();
        assert_eq!(
            res["history"],
            json!([
                {"skill_id": relevant_id, "old_level": 4, "new_level": 5, "logged_at": 300, "name": relevant_name},
                {"skill_id": relevant_id, "old_level": 3, "new_level": 4, "logged_at": 100, "name": relevant_name},
            ])
        );
        assert_eq!(res["ids"], json!(data.skills.name_lookup));
    }
    let res = app
        .get("/api/history/skills?character_id=1001", Some(&stranger))
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));
}
