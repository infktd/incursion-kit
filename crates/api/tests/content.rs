//! File-backed content: locales, guides, guide images and their admin endpoints.

mod common;

use std::fs;
use std::io::Cursor;

use axum::http::Method;
use common::{Login, TestApp};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

/// A data directory like the legacy repository's.
fn seed(app: &TestApp) {
    let dir = app.data_dir.path();
    fs::create_dir_all(dir.join("locales")).unwrap();
    fs::create_dir_all(dir.join("guides/ddd")).unwrap();
    for (name, content) in [
        ("locales/home.en.json", r#"{"title":"Home"}"#),
        ("locales/home.de.json", r#"{"title":"Start"}"#),
        ("locales/nav.en.json", r#"{"home":"Home"}"#),
        ("locales/nav.de.json", r#"{"home":"Start"}"#),
        ("locales/home.fr.json.backup.1", r#"{"title":"Accueil"}"#),
        ("locales/nav.fr.json", r#"{"home":"Accueil"}"#),
        (
            "locales/languages.json",
            r#"{"en":"English","fr":"Français"}"#,
        ),
        (
            "locales/marauder.en.json",
            r##"{"body":"# Marauders\ntext","section":"fc"}"##,
        ),
        (
            "guides/ddd/ddd.en.json",
            r#"{"title":"DDD Guide","subtitle":"sub","icon":"https://img/1","body":"x"}"#,
        ),
        ("guides/ddd/pic.png", "png-bytes"),
    ] {
        fs::write(dir.join(name), content).unwrap();
    }
}

async fn admin(app: &TestApp) -> Login {
    app.create_account(1001, "Admin", Some("admin")).await
}

#[tokio::test]
async fn languages_and_locales() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    seed(&app);

    let res = app.get("/api/v2/locales/languages", None).await;
    assert_eq!(
        res.json::<Value>(),
        json!({"languages": ["de", "en"], "labels": {"de": "DE", "en": "English"}})
    );

    let res = app.get("/api/v2/locales/home/de", None).await;
    assert_eq!(res.json::<Value>(), json!({"title": "Start"}));
    // Guides are served through the same endpoint, from the guide directory or the old place.
    let res = app.get("/api/v2/locales/ddd/en", None).await;
    assert_eq!(res.json::<Value>()["title"], "DDD Guide");
    let res = app.get("/api/v2/locales/marauder/en", None).await;
    assert_eq!(res.json::<Value>()["section"], "fc");

    let res = app.get("/api/v2/locales/home/fr", None).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Locale file not found: home.fr.json")
    );
    let res = app.get("/api/v2/locales/Home/en", None).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Page slug must start with a lowercase letter")
    );
    let res = app.get("/api/v2/locales/home/eng", None).await;
    assert_eq!(res.status, 400);
}

#[tokio::test]
async fn guides_and_assets() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    seed(&app);

    let res = app.get("/api/v2/guides", None).await;
    assert_eq!(
        res.json::<Value>(),
        json!({"guides": [
            {"slug": "ddd", "title": "DDD Guide", "subtitle": "sub", "icon": "https://img/1",
             "section": "public", "access": null},
            {"slug": "marauder", "title": "Marauders", "subtitle": null, "icon": null,
             "section": "fc", "access": null},
        ]})
    );

    let res = app.get("/api/v2/guides/ddd/assets/pic.png", None).await;
    assert_eq!(res.status, 200);
    assert_eq!(res.header("content-type"), Some("image/png"));
    assert_eq!(&res.body[..], b"png-bytes");

    let res = app.get("/api/v2/guides/ddd/assets/missing.png", None).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (404, "Guide asset not found: missing.png")
    );
    let res = app.get("/api/v2/guides/ddd/assets/notes.txt", None).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (404, "Unsupported guide asset type")
    );
    let res = app.get("/api/v2/guides/BAD/assets/pic.png", None).await;
    assert_eq!(res.status, 404);
}

#[tokio::test]
async fn admin_lists_and_reads_content_files() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    seed(&app);
    fs::write(app.data_dir.path().join("tags.yaml"), "public_tags: []\n").unwrap();
    fs::write(app.data_dir.path().join("tags.yaml.backup.5"), "old").unwrap();
    fs::write(app.data_dir.path().join("notes.txt"), "not listed").unwrap();
    let admin = admin(&app).await;

    let res = app.get("/api/admin/data-files", Some(&admin)).await;
    assert_eq!(res.status, 200, "{}", res.text());
    let row = |name: &str, file_type: &str, requires_reload: bool, size: u64, is_guide: bool| {
        json!({"name": name, "file_type": file_type, "requires_reload": requires_reload,
               "size": size, "is_guide": is_guide})
    };
    assert_eq!(
        res.json::<Value>(),
        json!({"files": [
            row("ddd.en.json", "Guide JSON", false, 72, true),
            row("home.de.json", "Locale JSON", false, 17, false),
            row("home.en.json", "Locale JSON", false, 16, false),
            row("languages.json", "Language labels", false, 33, false),
            row("marauder.en.json", "Guide JSON", false, 43, true),
            row("nav.de.json", "Locale JSON", false, 16, false),
            row("nav.en.json", "Locale JSON", false, 15, false),
            row("nav.fr.json", "Locale JSON", false, 18, false),
            row("tags.yaml", "YAML", true, 16, false),
        ]})
    );

    let res = app
        .get("/api/admin/data-files/home.en.json", Some(&admin))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (200, r#"{"title":"Home"}"#)
    );
    assert!(
        res.header("content-type")
            .unwrap()
            .starts_with("text/plain")
    );
    let res = app
        .get("/api/admin/data-files/languages.json", Some(&admin))
        .await;
    assert_eq!(res.text(), r#"{"en":"English","fr":"Français"}"#);
    let res = app
        .get("/api/admin/data-files/notes.txt", Some(&admin))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "File not editable")
    );
    let res = app
        .get("/api/admin/data-files/..%2Fsecret", Some(&admin))
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (400, "Invalid filename"));
    let res = app
        .get("/api/admin/data-files/home.it.json", Some(&admin))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Locale file not found: home.it.json")
    );

    fs::remove_file(app.data_dir.path().join("locales/languages.json")).unwrap();
    let res = app
        .get("/api/admin/data-files/languages.json", Some(&admin))
        .await;
    assert_eq!(res.text(), "{}");
}

#[tokio::test]
async fn admin_saves_and_deletes_content_files() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    seed(&app);
    let admin = admin(&app).await;
    let dir = app.data_dir.path();

    // Validated before anything is written.
    let res = app
        .send_raw(
            Method::POST,
            "/api/admin/data-files/home.en.json",
            Some(&admin),
            r#"{"title":1}"#,
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Key \"title\" must have a string value")
    );
    assert_eq!(
        fs::read_to_string(dir.join("locales/home.en.json")).unwrap(),
        r#"{"title":"Home"}"#
    );

    let res = app
        .send_raw(
            Method::POST,
            "/api/admin/data-files/home.en.json",
            Some(&admin),
            r#"{"title":"New"}"#,
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (200, "Locale file saved successfully")
    );
    assert_eq!(
        fs::read_to_string(dir.join("locales/home.en.json")).unwrap(),
        r#"{"title":"New"}"#
    );
    let backups = fs::read_dir(dir.join("locales"))
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("home.en.json.backup.")
        })
        .count();
    assert_eq!(backups, 1);

    // A new guide, created through `kind=guide`.
    let res = app
        .send_raw(
            Method::POST,
            "/api/admin/data-files/boxing.en.json?kind=guide",
            Some(&admin),
            r##"{"body":"# Boxing"}"##,
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (200, "Guide file saved successfully")
    );
    assert!(dir.join("guides/boxing/boxing.en.json").exists());
    let guides: Value = app.get("/api/v2/guides", None).await.json();
    assert!(
        guides["guides"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["title"] == "Boxing")
    );

    let res = app
        .send_raw(
            Method::POST,
            "/api/admin/data-files/languages.json",
            Some(&admin),
            r#"{"english":"x"}"#,
        )
        .await;
    assert_eq!(res.status, 400);
    let res = app
        .send_raw(
            Method::POST,
            "/api/admin/data-files/languages.json",
            Some(&admin),
            r#"{"en":"EN"}"#,
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (200, "Language labels saved successfully")
    );

    let res = app
        .send_raw(
            Method::POST,
            "/api/admin/data-files/other.txt",
            Some(&admin),
            "x",
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "File not editable")
    );

    let res = app
        .delete("/api/admin/data-files/boxing.en.json", Some(&admin))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (200, "Locale file deleted successfully")
    );
    assert!(!dir.join("guides/boxing/boxing.en.json").exists());
    let res = app
        .delete("/api/admin/data-files/boxing.en.json", Some(&admin))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Locale file not found: boxing.en.json")
    );
    let res = app
        .delete("/api/admin/data-files/skills.yaml", Some(&admin))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (
            400,
            "Only locale translation files can be deleted from here"
        )
    );
}

fn opaque_png(width: u32, height: u32) -> Vec<u8> {
    // Noise, so the PNG stays large.
    let mut seed = 12345u32;
    let img = image::RgbaImage::from_fn(width, height, |_, _| {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        let [a, b, c, _] = seed.to_le_bytes();
        image::Rgba([a, b, c, 255])
    });
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

#[tokio::test]
async fn admin_guide_assets() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    seed(&app);
    let admin = admin(&app).await;

    let res = app.get("/api/admin/guides/ddd/assets", Some(&admin)).await;
    assert_eq!(
        res.json::<Value>(),
        json!({"assets": [{"name": "pic.png", "size": 9}]})
    );
    let res = app
        .get("/api/admin/guides/new_guide/assets", Some(&admin))
        .await;
    assert_eq!(res.json::<Value>(), json!({"assets": []}));

    // Small files are stored as they are.
    let res = app
        .send_raw(
            Method::POST,
            "/api/admin/guides/ddd/assets/small.gif",
            Some(&admin),
            "GIF89a",
        )
        .await;
    assert_eq!(
        res.json::<Value>(),
        json!({"filename": "small.gif", "size": 6})
    );

    // A large opaque PNG becomes a JPEG.
    let png = opaque_png(1800, 900);
    assert!(png.len() > 1024 * 1024);
    let res = app
        .send_raw(
            Method::POST,
            "/api/admin/guides/ddd/assets/shot.png",
            Some(&admin),
            png,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let body: Value = res.json();
    assert_eq!(body["filename"], "shot.jpg");
    let saved = fs::read(app.data_dir.path().join("guides/ddd/shot.jpg")).unwrap();
    assert_eq!(body["size"], saved.len());
    assert_eq!(
        image::guess_format(&saved).unwrap(),
        image::ImageFormat::Jpeg
    );

    let res = app
        .send_raw(
            Method::POST,
            "/api/admin/guides/ddd/assets/evil.html",
            Some(&admin),
            "x",
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Unsupported guide asset type")
    );
    let res = app
        .send_raw(
            Method::POST,
            "/api/admin/guides/ddd/assets/big.gif",
            Some(&admin),
            vec![0u8; 10_000_001],
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Upload exceeds 10 MiB limit")
    );

    let res = app
        .delete("/api/admin/guides/ddd/assets/small.gif", Some(&admin))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (200, "Asset deleted successfully")
    );
    let res = app
        .delete("/api/admin/guides/ddd/assets/small.gif", Some(&admin))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Guide asset not found: small.gif")
    );
}

#[tokio::test]
async fn admin_endpoints_need_admin() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    seed(&app);
    let fc_plus = app.create_account(1001, "FcPlus", Some("fc+")).await;
    for res in [
        app.get("/api/admin/data-files", Some(&fc_plus)).await,
        app.get("/api/admin/data-files/home.en.json", Some(&fc_plus))
            .await,
        app.send_raw(
            Method::POST,
            "/api/admin/data-files/home.en.json",
            Some(&fc_plus),
            "{}",
        )
        .await,
        app.delete("/api/admin/data-files/home.en.json", Some(&fc_plus))
            .await,
        app.send_raw(
            Method::POST,
            "/api/admin/data-files/skills.yaml/reload",
            Some(&fc_plus),
            "",
        )
        .await,
        app.get("/api/admin/guides/ddd/assets", Some(&fc_plus))
            .await,
        app.send_raw(
            Method::POST,
            "/api/admin/guides/ddd/assets/a.gif",
            Some(&fc_plus),
            "GIF",
        )
        .await,
        app.delete("/api/admin/guides/ddd/assets/pic.png", Some(&fc_plus))
            .await,
    ] {
        assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));
    }
    let res = app.get("/api/admin/data-files", None).await;
    assert_eq!(res.status, 401);
}
