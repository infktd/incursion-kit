# The wiki

A community's wiki is edited in the app: editors log in with EVE SSO and write pages in the
browser, gated by roles. No Git and no deploys. Pages live in the database with every revision
kept. The server stores Markdown and validates it; the frontend renders it. Everything is under
`/api/wiki*` (tag `wiki` in the [OpenAPI reference](openapi.json)), an extension beyond the
legacy API. The domain code is the `wl-wiki` crate (`crates/wiki`).

## Model

- **Sections** group pages in the numbered index. Each has a `key` (`start`, fixed once
  created), a `label` ("Start here"), a position, and `listed`. An unlisted section stays out of
  the index; it holds pages a frontend shows elsewhere (see [FC cheatsheets](#fc-cheatsheets)).
- **Pages** have a path (`slug`, e.g. `guides/newbro`: 1 to 6 parts of lowercase letters,
  digits, `-` and `_`), a title, a `summary`, `for` (who the page is for, e.g. "All new
  pilots"), a section and a position in it, a Markdown `body`, a `status` and a `visibility`.
  - `status`: `draft` (only editors see it) or `published`.
  - `visibility`: `public` (everyone, logged in or not), `login` (any logged-in account) or
    `permission`, with a `permission` key (e.g. `fleet-view` for FCs and trainees). You can only
    restrict a page to a key you hold, unless you hold `wiki-admin`.
  - Deleting is soft: the page leaves reading, the index and search but keeps its revisions, and
    `wiki-admin` can restore it. Its path is free for a new page while it is deleted.
- **Revisions** hold the full content (title, summary, `for`, body) of every save, with its
  author (the account's main character), time and message. Moves, publishing and visibility
  changes don't create revisions.
- **Images** are uploaded files: metadata in the database, bytes in the data directory
  (`<data>/wiki/assets/<id>.<ext>`, so they are in the data-directory backup next to guides).
- The **index numbering** is computed per reader: listed sections that have at least one page
  the reader may see are numbered 1, 2, 3… in order, and their pages 1.1, 1.2… by position
  (ties by title). Hidden pages don't leave gaps.

The tables are `wiki_section`, `wiki_page` (the current content, denormalized),
`wiki_revision`, `wiki_link` (the `/wiki/<slug>` links of each page, for backlinks),
`wiki_asset` and `wiki_state` (a generation counter, see [Search](#search)); migration
`crates/db/migrations/0004_wiki.sql`.

## Permissions

| Key | Default roles | Allows |
|---|---|---|
| (none) | everyone | reading what the page's visibility allows; public pages need no login |
| `wiki-edit` | `fc` and above | creating, editing, moving, publishing and unpublishing pages; revisions, diffs and reverts; uploading and deleting images; drafts in the index and search |
| `wiki-admin` | `fc+` and above | sections (create, rename, list/unlist, reorder, delete), deleting and restoring pages, changing the visibility of pages created by others, and reading every page whatever its visibility |

An editor without a page's permission key can't see or edit that page (it is a `404`).

## Writing pages

The body is Markdown (CommonMark/GFM) plus **directives**, the block form of the
[generic directives proposal](https://talk.commonmark.org/t/generic-directives-plugins-syntax/444)
as `remark-directive` implements it, so a frontend can render pages with
`remark-parse` + `remark-gfm` + `remark-directive` and a handler per directive name.

- A **leaf** directive is one line: `::name{attributes}`.
- A **container** opens with a line `:::name{attributes}` (three or more colons) and closes
  with a line of at least as many colons. A container inside another must use **fewer** colons
  than its parent, so each fence closes exactly one directive (`::::steps` around `:::step`).
- Attributes: `key="value"`, `key='value'`, `key=value` (no spaces or quotes) or a bare `key`.
  In quoted values write `"` as `&quot;` and `&` as `&amp;`. `[labels]` are not used.
- Lines in fenced code blocks are never directives. Inline (`:name[...]`) directives are not
  used.

### Components

| Directive | Form | Attributes | Renders |
|---|---|---|---|
| `callout` | container | `kind` (`note` default, `tip`, `warning`, `danger`), `title` | a boxed note with an optional tag |
| `steps` | container | | a numbered step list; may contain only `step` directives |
| `step` | container, inside `steps` | `title` (required) | one step |
| `details` | container | `summary` (required) | a collapsible block |
| `fit` | leaf | `name` (required), `caption` | a doctrine fit by its exact name, as `GET /api/fittings` lists it, so a wiki fit always matches what the waitlist checks |
| `fit` | container | `role`, `caption` | a pasted EFT fit (not a doctrine fit); the content is the EFT text, first line `[Ship, Fit name]` |

```markdown
:::callout{kind=warning title="Before your first fleet"}
Anything in \[square brackets] is a placeholder.
:::

::::steps
:::step{title="Join comms"}
Join the in-game channel and the voice server.
:::

:::step{title="X-up"}
Paste your fit on the [waitlist](/waitlist). Read the [new pilot guide](/wiki/guides/newbro).
:::
::::

::fit{name="ARMOR_NESTOR" caption="Paste this into the waitlist."}

:::fit{role=DPS caption="A non-doctrine fit"}
[Vindicator, Example]
Large Hybrid Turret
:::

![The staging system](/api/wiki/assets/12/staging.jpg)
```

- **Links** between pages are written `/wiki/<slug>` (optionally with `#anchor`); they make
  the backlinks of the target page. Links are not rewritten when a page moves.
- **Images** are uploaded with `POST /api/wiki/assets?filename=…` and used with the `url` it
  answers.
- **Headings** get GitHub-style ids (lowercase, punctuation removed, spaces to `-`, `-1`, `-2`
  for repeats), the same as `rehype-slug`; the page response lists them for "on this page".

### Validation

Every save (create, update, revert) validates the body; nothing invalid is stored. Problems are
a `400` with `"error": "invalid_markdown"` and every problem with its line:

```json
{"error": "invalid_markdown", "message": "the page has 2 problems: line 1: …",
 "problems": [{"line": 1, "code": "invalid_attribute", "message": "callout kind `loud` is not one of note, tip, warning, danger"},
              {"line": 5, "code": "unknown_directive", "message": "unknown directive `tabs` (known: callout, steps, step, details, fit)"}]}
```

Codes: `unknown_directive`, `wrong_form`, `unknown_attribute`, `missing_attribute`,
`invalid_attribute`, `invalid_directive` (unparseable line), `misplaced_directive` (`step`
outside `steps`, text directly in `steps`), `nesting`, `unclosed_directive`,
`unexpected_close`, `unknown_fit` (no doctrine fit of that name; checked when the server has
its fit data loaded), `invalid_eft`. `POST /api/wiki/validate` `{"body"}` runs the same checks
without saving and also answers the headings, links and read time, for an editor's preview.

## API

Reads (login optional; what the reader may see):

| Endpoint | |
|---|---|
| `GET /api/wiki` | The numbered index: `{sections: [{key, label, num, pages: [{id, slug, title, summary, for, num, position, status, visibility, permission, revision_id, updated_at}]}], can_edit, can_admin}`. `?drafts=true` adds drafts for editors. |
| `GET /api/wiki/page?slug=guides/newbro` | A page: the index fields plus `section {key, label}`, `body`, `created_at/by`, `updated_at/by` (`{id, name}`, `null` for imports), `published_at`, `read_minutes` (200 words a minute), `words`, `headings [{depth, text, id}]`, `backlinks [{slug, title}]`, `can_edit`. `ETag: "<revision id>"`. |
| `GET /api/wiki/pages/{id}` | The same, by id. |
| `GET /api/wiki/sections` | Every section with `listed` and the number of pages the reader may see. |
| `GET /api/wiki/sections/{key}/pages?body=true` | One section's pages in order (listed or not), with bodies on request. |
| `GET /api/wiki/search?q=…&limit=20` | Ranked search, see [Search](#search). |
| `GET /api/wiki/assets/{id}/{filename}` | An image (public, immutable cache, sandboxed). |

Writes (JSON bodies; `wiki-edit` unless noted):

| Endpoint | |
|---|---|
| `POST /api/wiki/pages` | `{slug, title, section, summary?, for?, body?, position?, status?, visibility?, permission?, message?}` → `201` the page. |
| `PUT /api/wiki/pages/{id}` | The fields to change: `title`, `summary`, `for`, `body`, `visibility`, `permission`, `message`. Content changes need `base_revision` or `If-Match`, see below. |
| `POST /api/wiki/pages/{id}/move` | `{slug?, section?, position?}`: rename, move, reorder (1-based; the section is renumbered). |
| `POST /api/wiki/pages/{id}/publish`, `/unpublish` | Status. |
| `DELETE /api/wiki/pages/{id}` | Soft delete (`wiki-admin`). |
| `GET /api/wiki/deleted`, `POST /api/wiki/pages/{id}/restore` `{section?}` | Deleted pages; restore (`wiki-admin`). |
| `GET /api/wiki/pages/{id}/revisions`, `/revisions/{revision_id}` | History (newest first) and one revision. |
| `GET /api/wiki/pages/{id}/diff?from=&to=` | Line diff of the body (`lines [{op, old_line, new_line, text}]`, `unified`, counts), and both sides' `from_title`/`to_title`, `from_summary`/`to_summary` and `from_for`/`to_for` (with `summary_changed` when the summary or `for` differ); `to` defaults to the current revision. |
| `POST /api/wiki/pages/{id}/revert` | `{revision, base_revision?, message?}`: the old content as a new revision. |
| `POST /api/wiki/validate` | `{body}`: validation without saving. |
| `GET /api/wiki/assets`, `POST /api/wiki/assets?filename=x.png` (raw bytes), `DELETE /api/wiki/assets/{id}?force=` | Images; the list has `used_by` per image, and deleting one that pages show is a `409` unless `force=true` (see Images below). |
| `POST /api/wiki/sections`, `PUT /api/wiki/sections/{key}`, `DELETE /api/wiki/sections/{key}`, `POST /api/wiki/sections/order` `{keys}` | Sections (`wiki-admin`); only an empty section can be deleted. |

**Errors are JSON** on every wiki endpoint (unlike the legacy API's plain text):
`{"error": code, "message": text, "problems"?: […], "current_revision"?: id, "used_by"?: [{slug, title}]}`.

| Status | `error` | When |
|---|---|---|
| 400 | `bad_request` | an invalid field, path, section or permission key |
| 400 | `invalid_markdown` | body problems, with `problems` |
| 401 | `unauthenticated` / `access_denied` | not logged in / missing `wiki-edit` or `wiki-admin` (as elsewhere, a missing key is a 401) |
| 404 | `not_found` | no such page, revision, section or image, or one the reader may not see |
| 409 | `conflict` | a stale base revision (with `current_revision`), a taken path, a non-empty section, an image that pages show (with `used_by`) |
| 413 | `too_large` | a body over 512 KiB, an image over 10 MiB |
| 422 | `invalid_json` | a body that isn't the expected JSON |
| 428 | `precondition_required` | a content change without a base revision |

A malformed path or query parameter (`/api/wiki/pages/abc`) is still the API's plain-text
`404 Not Found`.

**Concurrent edits.** A page carries its current `revision_id` (also its `ETag`). An edit sends
it back as `base_revision` (or `If-Match: "<id>"`); if someone saved in between, the answer is
`409 conflict` with `current_revision`, and nothing is overwritten: reload, merge, save again.
Saving unchanged content creates no revision.

**Images.** PNG, JPEG, GIF, WebP or SVG, at most 10 MiB; over 1 MiB they are resized (longest
edge 1600 px) and re-encoded, an opaque PNG becoming a JPEG (the answer has the final
`filename` and `url`). They are served to anyone who has the URL, with
`Cache-Control: public, max-age=31536000, immutable` and
`Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; sandbox`.
`GET /api/wiki/assets` lists each image's `used_by: [{slug, title}]`: the live pages whose
current body references its URL (`/api/wiki/assets/<id>/…`, relative or absolute, in an image,
a link or a directive), ordered by title, limited to pages the editor may see. Old revisions
and deleted pages don't count. `DELETE /api/wiki/assets/{id}` refuses an image in use with
`409 conflict`, `used_by` and a message counting every page; `?force=true` deletes it anyway,
and those pages then show a broken image.

## Search

`GET /api/wiki/search?q=fc training` answers
`{query, hits: [{id, slug, title, summary, section {key, label}, status, score, snippet}]}`,
best first, at most 50. Every query word must match a word of the page, exactly or as its
beginning (so results appear while typing); words are lowercased and simple plurals folded
(`fleets` → `fleet`). Matches weigh more in the title, then headings and summary, then the body
(BM25). `snippet` is plain text around the first match in the body, as parts
`[{text, highlight}]` to render without HTML. Only pages the reader may see are searched;
editors also find drafts.

The index is kept in memory and rebuilt (milliseconds for hundreds of pages) when the
`wiki_state` generation, bumped by every write, changes; so edits are searchable at once, also
after an import from the command line. MySQL `FULLTEXT` isn't used: InnoDB skips words shorter
than three letters by default, and "FC" is what pilots search for.

## Live updates

Every change publishes `wiki_update` with
`{"slug": "guides/newbro" | null, "section": "start" | null, "change": "created" | "updated" |
"moved" | "published" | "unpublished" | "deleted" | "restored" | "section" | "imported"}`.
Changes to pages anonymous visitors could see (before or after the change) and section changes
go to the topic `wiki` (on `GET /api/sse/stream` and `GET /api/public/stream`); others
(drafts, gated pages) to `wiki_restricted` (on `GET /api/sse/stream` only). A page showing
`slug` refetches it; the index refetches on any event. A move sends the event for the old and
the new path.

## FC cheatsheets

Quick-reference sheets an FC console shows beside the queue are pages in the unlisted section
`fc-cheatsheets` (paths `fc-cheatsheets/<name>`), readable with `fleet-view`:
`GET /api/wiki/sections/fc-cheatsheets/pages?body=true` returns all of them, in order, with
their bodies. They are edited like any page. The same pattern works for other snippets: an
unlisted section, fetched as a whole.

## Importing a static-site wiki

```sh
waitlist wiki import --dry-run ./content   # check and report, write nothing
waitlist wiki import ./content
```

The directory may contain:

```text
wiki/<path>.md or .mdx          pages; the path without extension is the slug (wiki/guides/newbro.mdx → guides/newbro)
wiki-sections/<key>.yaml        sections: label, order, and optionally listed
fc-cheatsheets/<name>.md(x)     FC cheatsheets → fc-cheatsheets/<name> (see above)
```

Page frontmatter:

```yaml
---
title: New pilot guide        # required
section: start                # required for wiki pages (a wiki-sections file name)
order: 1                      # position in the section
summary: The first fleet, step by step.
for: All new pilots
status: published             # optional (default published)
visibility: public            # optional: public, login or permission
permission: fleet-view        # with visibility: permission
updated: 2026-10-01           # optional: the revision's date
editor: Some Pilot            # optional: named in the revision message
---
```

MDX components are converted to directives:

| MDX | Markdown |
|---|---|
| `<Callout title="T" kind="tip">…</Callout>` | `:::callout{title="T" kind="tip"}` … `:::` |
| `<Steps><Step title="T">…</Step></Steps>` | `::::steps` / `:::step{title="T"}` … `:::` / `::::` |
| `<Details summary="S">…</Details>` | `:::details{summary="S"}` … `:::` |
| `<Fit name="N" caption="C" />` | `::fit{name="N" caption="C"}` |
| `<Fit eft="[Ship, Name]…" role="R" caption="C" />` | `:::fit{role="R" caption="C"}` + the EFT lines + `:::` |

Component tags must stand on their own lines (a tag may span several, as a long `eft` does);
indented children are de-indented; `{/* comments */}` lines are dropped. Anything else only MDX
understands (other components, `import`/`export`, inline JSX, JavaScript expressions as
attribute values) is an error with its file and line.

All files are read and checked first; if any has an error, nothing is written. Otherwise
everything is written in one transaction: new pages are created (published, unless `status`
says otherwise), existing ones (same path) get a new revision "Imported from <file>" only when
their content changed, and sections are created or updated. A section a page names without a
`wiki-sections` file is created with a warning. Importing the same files again changes nothing.
Doctrine fit names are not checked by the import (it doesn't load the fit data); the editor
checks them on the next save. The import doesn't reach a running server's live stream: open
pages see the change on their next load.
