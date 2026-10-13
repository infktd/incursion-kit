//! Converting a small synthetic SDE archive end to end.

use std::io::Write;

use zip::write::SimpleFileOptions;

use super::{BuildOptions, EnsureOutcome, SdeInfo, build, ensure_sde};
use crate::{AttributeId, Category, EveData, Slot};

const TYPES: &str = r#"{"_key": 670, "groupID": 29, "name": {"en": "Capsule", "de": "Kapsel"}, "published": true}
{"_key": 3057, "groupID": 53, "name": {"en": "Mega Pulse Laser II"}, "published": true, "metaGroupID": 2, "variationParentTypeID": 3025}
{"_key": 3025, "groupID": 53, "name": {"en": "Mega Pulse Laser I"}, "published": true, "metaGroupID": 1}
{"_key": 12816, "groupID": 86, "name": {"en": "Conflagration L"}, "published": true}
{"_key": 99, "groupID": 53, "name": {"de": "Nur Deutsch"}, "published": false}

{"_key": 3300, "groupID": 255, "name": {"en": "Gunnery"}, "published": true}
"#;

const GROUPS: &str = r#"{"_key": 29, "categoryID": 6, "name": {"en": "Capsule"}}
{"_key": 53, "categoryID": 7}
{"_key": 86, "categoryID": 8}
{"_key": 255, "categoryID": 16}
"#;

const DOGMA: &str = r#"{"_key": 3057, "dogmaAttributes": [{"attributeID": 182, "value": 3300.0}, {"attributeID": 277, "value": 5}, {"attributeID": 633, "value": 5.0}, {"attributeID": 9999, "value": 1.5}], "dogmaEffects": [{"effectID": 12, "isDefault": false}, {"effectID": 42, "isDefault": true}]}
{"_key": 3300, "dogmaAttributes": [{"attributeID": 275, "value": 1.0}]}
"#;

fn write_archive(dir: &std::path::Path, with_info: bool, prefix: &str) -> std::path::PathBuf {
    let path = dir.join("sde.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut files = vec![
        ("types.jsonl", TYPES),
        ("groups.jsonl", GROUPS),
        ("typeDogma.jsonl", DOGMA),
    ];
    let info = r#"{"_key": "sde", "buildNumber": 1234, "releaseDate": "2026-01-01T00:00:00Z"}"#;
    if with_info {
        files.push(("_sde.jsonl", info));
    }
    for (name, content) in files {
        zip.start_file(format!("{prefix}{name}"), options).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    path
}

#[tokio::test]
async fn converts_an_archive_and_loads_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let archive = write_archive(dir.path(), true, "");
    let out = dir.path().join("nested/sde.sqlite");
    let options = BuildOptions {
        archive: Some(archive.clone()),
        ..BuildOptions::default()
    };

    let info = build(&out, &options).await.unwrap();
    assert_eq!(
        info,
        SdeInfo {
            build_number: 1234,
            release_date: Some("2026-01-01T00:00:00Z".into())
        }
    );
    // The partial file is gone and the archive was not deleted.
    assert!(archive.exists());
    assert!(!out.with_extension("sqlite.partial").exists());

    let eve = EveData::load(&out).await.unwrap();
    assert_eq!(eve.info(), Some(&info));
    assert_eq!(eve.type_count(), 5);
    assert_eq!(eve.id_of("Nur Deutsch"), None);

    let capsule = eve.by_name("Capsule").unwrap();
    assert_eq!(capsule.id, 670);
    assert_eq!(capsule.category, Category::Ship);

    let laser = eve.require(3057).unwrap();
    assert_eq!(laser.category, Category::Module);
    assert_eq!(laser.slot(), Some(Slot::High));
    assert_eq!(laser.attribute(AttributeId::META_LEVEL), Some(5.0));
    assert_eq!(laser.attributes.len(), 3, "unused attributes are dropped");
    assert_eq!(laser.effects.len(), 1, "unused effects are dropped");
    assert_eq!(laser.skill_requirements.get(&3300), Some(&5));

    let ammo = eve.by_name("Conflagration L").unwrap();
    assert!(ammo.is_always_cargo());

    let variations = eve.type_variations(3057);
    assert_eq!(variations.get(&3025), Some(&0));
    assert_eq!(variations.get(&3057), Some(&5));

    // A second ensure leaves the file alone.
    assert_eq!(
        ensure_sde(&out, &options).await.unwrap(),
        EnsureOutcome::Present
    );
}

#[tokio::test]
async fn uses_the_requested_build_number_without_an_info_file() {
    let dir = tempfile::tempdir().unwrap();
    let archive = write_archive(dir.path(), false, "eve-online-static-data-77-jsonl/");
    let out = dir.path().join("sde.sqlite");
    let options = BuildOptions {
        archive: Some(archive),
        build_number: Some(77),
        ..BuildOptions::default()
    };
    let outcome = ensure_sde(&out, &options).await.unwrap();
    assert_eq!(
        outcome,
        EnsureOutcome::Built(SdeInfo {
            build_number: 77,
            release_date: None
        })
    );
    assert_eq!(EveData::load(&out).await.unwrap().type_count(), 5);
}

#[tokio::test]
async fn missing_file_is_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let error = EveData::load(dir.path().join("nope.sqlite"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("waitlist sde"), "{error}");
}
