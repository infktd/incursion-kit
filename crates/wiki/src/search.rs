//! Full-text search: an in-memory index over every (non-deleted) page.
//!
//! Why not MySQL `FULLTEXT`: InnoDB ignores words shorter than `innodb_ft_min_token_size` (3 by
//! default, a server setting), and "FC", "DD" or "HQ" are exactly what pilots search for; it
//! also has stopwords and no snippets. A wiki has hundreds of pages, not millions, so the index
//! is rebuilt in memory (milliseconds) whenever the `wiki_state` generation, bumped by every
//! write in any process, has moved.
//!
//! Ranking is BM25 over four fields with weights (title 4, headings and summary 2, body 1).
//! Every query term must match (as a word, or as the start of a word with a lower weight, so
//! results appear while typing). Words are lowercased and lightly stemmed (`fleets` → `fleet`).
//! Results are filtered by what the viewer may see.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::model::{SearchHit, SectionRef, SnippetPart, Status, Visibility};

const WEIGHTS: [f64; 4] = [4.0, 2.0, 2.0, 1.0];
const PREFIX_FACTOR: f64 = 0.5;
const K1: f64 = 1.2;
const B: f64 = 0.75;
/// Characters of context in a snippet.
const SNIPPET_CHARS: usize = 200;

/// A page as the index sees it.
#[derive(Debug, Clone)]
pub struct Document {
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub summary: String,
    pub section: SectionRef,
    pub status: Status,
    pub visibility: Visibility,
    pub permission: Option<String>,
    pub headings: String,
    pub body_text: String,
}

#[derive(Debug, Default)]
struct Field {
    terms: BTreeMap<String, u32>,
    len: u32,
}

#[derive(Debug)]
struct Entry {
    doc: Document,
    fields: [Field; 4],
}

/// The built index.
#[derive(Debug, Default)]
pub struct SearchIndex {
    entries: Vec<Entry>,
    /// term → number of pages containing it (any field).
    vocabulary: BTreeMap<String, usize>,
    avg_len: [f64; 4],
}

/// Lowercase words with their byte ranges in `text`.
fn words(text: &str) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        if c.is_alphanumeric() {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            out.push((s, i, normalize(&text[s..i])));
        }
    }
    if let Some(s) = start {
        out.push((s, text.len(), normalize(&text[s..])));
    }
    out
}

/// Lowercase and strip simple English plurals.
pub fn normalize(word: &str) -> String {
    let w = word.to_lowercase();
    let n = w.chars().count();
    if n > 4 && w.ends_with("ies") {
        return format!("{}y", &w[..w.len() - 3]);
    }
    if n > 3 && w.ends_with('s') && !w.ends_with("ss") && !w.ends_with("us") {
        return w[..w.len() - 1].to_string();
    }
    w
}

fn field(text: &str) -> Field {
    let mut f = Field::default();
    for (_, _, w) in words(text) {
        *f.terms.entry(w).or_default() += 1;
        f.len += 1;
    }
    f
}

impl SearchIndex {
    pub fn build(docs: Vec<Document>) -> Self {
        let mut vocabulary: BTreeMap<String, usize> = BTreeMap::new();
        let mut totals = [0f64; 4];
        let entries: Vec<Entry> = docs
            .into_iter()
            .map(|doc| {
                let fields = [
                    field(&doc.title),
                    field(&doc.headings),
                    field(&doc.summary),
                    field(&doc.body_text),
                ];
                let mut seen: Vec<&String> = fields.iter().flat_map(|f| f.terms.keys()).collect();
                seen.sort();
                seen.dedup();
                for term in seen {
                    *vocabulary.entry(term.clone()).or_default() += 1;
                }
                for (total, f) in totals.iter_mut().zip(&fields) {
                    *total += f64::from(f.len);
                }
                Entry { doc, fields }
            })
            .collect();
        let n = entries.len().max(1) as f64;
        let avg_len = totals.map(|t| (t / n).max(1.0));
        Self {
            entries,
            vocabulary,
            avg_len,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn idf(&self, df: usize) -> f64 {
        let n = self.entries.len() as f64;
        let df = df as f64;
        ((n - df + 0.5) / (df + 0.5) + 1.0).ln()
    }

    /// Pages matching every word of `query` that `visible` accepts, best first.
    pub fn search(
        &self,
        query: &str,
        limit: usize,
        visible: impl Fn(&Document) -> bool,
    ) -> Vec<SearchHit> {
        let mut terms: Vec<String> = words(query).into_iter().map(|(_, _, w)| w).collect();
        terms.dedup();
        if terms.is_empty() {
            return Vec::new();
        }
        // Per query term: the exact form and the vocabulary words it is a prefix of.
        let expansions: Vec<Vec<(String, f64, f64)>> = terms
            .iter()
            .map(|t| {
                self.vocabulary
                    .range(t.clone()..)
                    .take_while(|(w, _)| w.starts_with(t.as_str()))
                    .map(|(w, df)| {
                        let factor = if w == t { 1.0 } else { PREFIX_FACTOR };
                        (w.clone(), factor, self.idf(*df))
                    })
                    .collect()
            })
            .collect();

        let mut hits: Vec<(f64, &Entry, Vec<String>)> = Vec::new();
        'docs: for entry in &self.entries {
            if !visible(&entry.doc) {
                continue;
            }
            let mut score = 0.0;
            let mut matched = Vec::new();
            for expansion in &expansions {
                let mut term_score = 0.0;
                for (word, factor, idf) in expansion {
                    for (i, f) in entry.fields.iter().enumerate() {
                        let Some(&tf) = f.terms.get(word) else {
                            continue;
                        };
                        let tf = f64::from(tf);
                        let norm = 1.0 - B + B * f64::from(f.len) / self.avg_len[i];
                        term_score +=
                            WEIGHTS[i] * factor * idf * tf * (K1 + 1.0) / (tf + K1 * norm);
                        matched.push(word.clone());
                    }
                }
                if term_score == 0.0 {
                    continue 'docs;
                }
                score += term_score;
            }
            hits.push((score, entry, matched));
        }
        hits.sort_by(|a, b| {
            b.0.total_cmp(&a.0)
                .then_with(|| a.1.doc.title.cmp(&b.1.doc.title))
        });
        hits.into_iter()
            .take(limit)
            .map(|(score, entry, matched)| SearchHit {
                id: entry.doc.id,
                slug: entry.doc.slug.clone(),
                title: entry.doc.title.clone(),
                summary: entry.doc.summary.clone(),
                section: entry.doc.section.clone(),
                status: entry.doc.status,
                score: (score * 1000.0).round() / 1000.0,
                snippet: snippet(&entry.doc.body_text, &matched),
            })
            .collect()
    }
}

/// Text around the first matched word of `text`, split into highlighted and plain parts.
pub fn snippet(text: &str, matched: &[String]) -> Vec<SnippetPart> {
    let tokens = words(text);
    let is_match = |w: &str| matched.iter().any(|m| m == w);
    let first = tokens.iter().find(|(_, _, w)| is_match(w));
    let start = match first {
        Some((s, _, _)) => {
            // Some context before the match, starting at a word.
            let back = s.saturating_sub(SNIPPET_CHARS / 4);
            tokens
                .iter()
                .map(|(ts, _, _)| *ts)
                .find(|ts| *ts >= back)
                .unwrap_or(*s)
        }
        None => 0,
    };
    let mut end = (start + SNIPPET_CHARS).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    // End at a word boundary.
    if end < text.len()
        && let Some(space) = text[start..end].rfind(' ')
        && space > SNIPPET_CHARS / 2
    {
        end = start + space;
    }
    let mut parts: Vec<SnippetPart> = Vec::new();
    let mut push = |s: &str, highlight: bool| {
        if s.is_empty() {
            return;
        }
        match parts.last_mut() {
            Some(last) if last.highlight == highlight => last.text.push_str(s),
            _ => parts.push(SnippetPart {
                text: s.to_string(),
                highlight,
            }),
        }
    };
    if start > 0 {
        push("…", false);
    }
    let mut pos = start;
    for (s, e, w) in tokens.iter().filter(|(s, e, _)| *s >= start && *e <= end) {
        if is_match(w) {
            push(&text[pos..*s], false);
            push(&text[*s..*e], true);
            pos = *e;
        }
    }
    push(&text[pos..end], false);
    if end < text.len() {
        push("…", false);
    }
    parts
}

/// An index and the `wiki_state` generation it was built from.
type Generation = (i64, Arc<SearchIndex>);

/// The index of the current generation, shared by all requests of a process. Cheap to clone.
#[derive(Clone, Default)]
pub struct SearchCache {
    inner: Arc<Mutex<Option<Generation>>>,
}

impl std::fmt::Debug for SearchCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchCache").finish_non_exhaustive()
    }
}

impl SearchCache {
    pub fn get(&self, generation: i64) -> Option<Arc<SearchIndex>> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .as_ref()
            .filter(|(g, _)| *g == generation)
            .map(|(_, index)| index.clone())
    }

    pub fn put(&self, generation: i64, index: Arc<SearchIndex>) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some((generation, index));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: i64, title: &str, body: &str) -> Document {
        Document {
            id,
            slug: format!("p{id}"),
            title: title.into(),
            summary: String::new(),
            section: SectionRef {
                key: "s".into(),
                label: "S".into(),
            },
            status: Status::Published,
            visibility: Visibility::Public,
            permission: None,
            headings: String::new(),
            body_text: body.into(),
        }
    }

    #[test]
    fn ranks_title_matches_first_and_requires_all_terms() {
        let index = SearchIndex::build(vec![
            doc(
                1,
                "Logistics",
                "Logi pilots repair the fleet. FC calls targets.",
            ),
            doc(2, "FC training", "How to become an FC."),
            doc(
                3,
                "Ships",
                "Which fleets to join. Nothing about commanders.",
            ),
        ]);
        let hits = index.search("fc", 10, |_| true);
        let ids: Vec<_> = hits.iter().map(|h| h.id).collect();
        assert_eq!(ids, vec![2, 1]);
        assert_eq!(index.search("fc training", 10, |_| true).len(), 1);
        // Plurals and prefixes.
        assert_eq!(index.search("fleets", 10, |_| true).len(), 2);
        assert_eq!(index.search("logis", 10, |_| true)[0].id, 1);
        assert!(index.search("", 10, |_| true).is_empty());
        assert!(
            index
                .search("fc", 10, |d| d.id != 2)
                .iter()
                .all(|h| h.id != 2)
        );
    }

    #[test]
    fn snippets_highlight_matches() {
        let text = format!(
            "{} the FC calls the primary target. {}",
            "intro ".repeat(30),
            "outro ".repeat(40)
        );
        let parts = snippet(&text, &["fc".to_string()]);
        assert!(parts.first().unwrap().text.starts_with('…'));
        let highlighted: Vec<_> = parts
            .iter()
            .filter(|p| p.highlight)
            .map(|p| p.text.as_str())
            .collect();
        assert_eq!(highlighted, vec!["FC"]);
        assert!(parts.last().unwrap().text.ends_with('…'));
        // No match in the body: the start.
        let parts = snippet("short body", &["x".to_string()]);
        assert_eq!(
            parts,
            vec![SnippetPart {
                text: "short body".into(),
                highlight: false
            }]
        );
    }
}
