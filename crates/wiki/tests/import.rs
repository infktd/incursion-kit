//! Importing a real static-site wiki (`tests/fixtures/content`, copied from a frontend's
//! `content/` directory: wiki pages, sections and FC cheatsheets in MDX).
//!
//! `WIKI_IMPORT_DIR=<dir>` checks another directory with the same layout as well.

use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use wl_db::test_support::TestDb;
use wl_wiki::import::{CHEATSHEET_PERMISSION, CHEATSHEET_SECTION, plan};
use wl_wiki::{SearchCache, Status, Viewer, Visibility, Wiki};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/content")
}

#[test]
fn real_content_plans_without_errors() {
    let mut dirs = vec![fixtures()];
    if let Some(extra) = std::env::var_os("WIKI_IMPORT_DIR") {
        dirs.push(PathBuf::from(extra));
    }
    for dir in dirs {
        let plan = plan(&dir);
        assert_eq!(plan.errors, vec![], "{}", dir.display());
        assert_eq!(plan.warnings, vec![], "{}", dir.display());
        assert!(plan.pages.len() >= 15, "{}", plan.pages.len());
    }

    let plan = plan(&fixtures());
    assert_eq!(plan.pages.len(), 16);
    assert_eq!(plan.sections.len(), 6);
    let newbro = plan
        .pages
        .iter()
        .find(|p| p.slug == "guides/newbro")
        .unwrap();
    assert_eq!(newbro.section, "start");
    assert_eq!(newbro.position, 1);
    assert_eq!(newbro.audience.as_deref(), Some("All new pilots"));
    assert!(
        newbro
            .summary
            .starts_with("Welcome aboard. This guide takes you")
    );
    assert!(
        newbro
            .body
            .starts_with(":::callout\nAnything in \\[square brackets]")
    );
    assert!(
        newbro
            .body
            .contains("::::steps\n:::step{title=\"Join comms\"}\nJoin the in-game")
    );
    assert!(
        newbro
            .body
            .contains("::fit{name=\"HQ Nestor\" caption=\"Pilots paste")
    );
    let library = plan
        .pages
        .iter()
        .find(|p| p.slug == "reference/fit-library")
        .unwrap();
    assert!(
        library
            .body
            .contains(":::fit{role=\"[Role]\" caption=\"[What this fit is for.]\"}\n[Ship name, Fit name]\nLow slot module\n"),
        "{}",
        library.body
    );
    let sheet = plan
        .pages
        .iter()
        .find(|p| p.slug == "fc-cheatsheets/target-priority")
        .unwrap();
    assert_eq!(sheet.section, CHEATSHEET_SECTION);
    assert_eq!(sheet.visibility, Visibility::Permission);
    assert_eq!(sheet.permission.as_deref(), Some(CHEATSHEET_PERMISSION));
    assert!(sheet.body.starts_with(":::callout{title=\"Template\"}"));
    assert!(sheet.body.contains("| Spawn | Call |"));
}

#[tokio::test]
async fn real_content_imports_and_reimports() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let wiki = Wiki::new(db.pool().clone(), dir.path(), SearchCache::default());
    let plan = plan(&fixtures());

    let dry = wiki.import(&plan, true).await.unwrap();
    assert_eq!((dry.pages_created, dry.sections_created), (16, 6));
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM wiki_page")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0, "a dry run writes nothing");

    let report = wiki.import(&plan, false).await.unwrap();
    assert_eq!(report.errors, vec![]);
    assert_eq!(report.warnings, vec![]);
    assert_eq!((report.pages_created, report.sections_created), (16, 6));

    let index = wiki.index(&Viewer::anonymous(), false).await.unwrap();
    let labels: Vec<_> = index
        .sections
        .iter()
        .map(|s| (s.num.as_str(), s.label.as_str()))
        .collect();
    assert_eq!(
        labels,
        vec![
            ("1", "Start here"),
            ("2", "Roles"),
            ("3", "Policies"),
            ("4", "FC team"),
            ("5", "Reference")
        ]
    );
    let start: Vec<_> = index.sections[0]
        .pages
        .iter()
        .map(|p| (p.num.clone().unwrap(), p.slug.as_str()))
        .collect();
    assert_eq!(
        start,
        vec![
            ("1.1".to_string(), "guides/newbro"),
            ("1.2".to_string(), "guides/ships"),
            ("1.3".to_string(), "guides/implants"),
            ("1.4".to_string(), "guides/alt-policy"),
        ]
    );

    // Cheatsheets are not in the index, and not for anonymous readers.
    let anonymous = wiki
        .page_by_slug(&Viewer::anonymous(), "fc-cheatsheets/invite-priority")
        .await;
    assert!(anonymous.is_err());
    let page = wiki
        .page_by_slug(&Viewer::anonymous(), "fc/fc-training")
        .await
        .unwrap();
    assert_eq!(page.status, Status::Published);
    assert_eq!(page.num.as_deref(), Some("4.2"));
    assert_eq!(
        page.headings
            .iter()
            .map(|h| h.text.as_str())
            .collect::<Vec<_>>(),
        vec!["Training path", "The FC console", "Further reading"]
    );
    // fc-training links to the fit library: a backlink there.
    let library = wiki
        .page_by_slug(&Viewer::anonymous(), "reference/fit-library")
        .await
        .unwrap();
    assert!(library.backlinks.iter().any(|b| b.slug == "fc/fc-training"));
    assert!(library.revision_id.is_some());
    assert!(library.created_by.is_none());

    // Searching works on imported content.
    let hits = wiki
        .search(&Viewer::anonymous(), "training", 10)
        .await
        .unwrap();
    assert_eq!(hits.hits[0].slug, "fc/fc-training");

    // Importing again changes nothing.
    let again = wiki.import(&plan, false).await.unwrap();
    assert_eq!(
        (
            again.pages_unchanged,
            again.pages_created,
            again.pages_updated
        ),
        (16, 0, 0)
    );
    assert_eq!(again.sections_unchanged, 6);
}
