//! Public tags (`tags.yaml`): the waitlist tags every pilot may see.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{Edited, Problems, Rejection, check_name, write_yaml};
use crate::{DataError, DataFile, yaml};

const MAX_TAG: usize = 64;

/// `tags.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TagsDocument {
    /// Tags shown to pilots without a role (other tags are only shown to FCs). Order is kept.
    pub public_tags: Vec<String>,
}

/// Parses `tags.yaml`.
pub fn read_tags(content: &str) -> Result<TagsDocument, DataError> {
    yaml::parse(DataFile::Tags, content)
}

/// Replaces `content` (`tags.yaml`) with `document`.
pub fn write_tags(content: &str, document: &TagsDocument) -> Edited {
    let mut problems = Problems::default();
    let mut seen = BTreeSet::new();
    for (index, tag) in document.public_tags.iter().enumerate() {
        let path = format!("public_tags[{index}]");
        if !check_name(&mut problems, &path, tag, MAX_TAG) {
            continue;
        }
        if tag.contains(',') {
            // Fit tags are stored comma-separated.
            problems.error(&path, "must not contain commas");
        } else if !seen.insert(tag.as_str()) {
            problems.error(&path, format!("tag {tag:?} is listed twice"));
        }
    }
    if problems.has_errors() {
        return Err(problems.into());
    }
    let yaml = write_yaml(DataFile::Tags, content, document)
        .map_err(|error| Rejection::invalid("", error.to_string()))?;
    problems.finish(yaml)
}
