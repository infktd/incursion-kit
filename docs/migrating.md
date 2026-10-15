# Migrating an existing waitlist

## Migrating from the TLA/TDF waitlist

incursion-kit is API- and database-compatible with the
[TDF waitlist](https://github.com/the-ditanian-fleet/waitlist) and its
[TLA fork](https://github.com/luna-duclos/tla-waitlist). You keep your database, your data files,
your configuration, your users' sessions and your frontend: the new backend takes the old one's
place behind the same `/api` path.

What carries over unchanged:

- **The database.** The schema is the TLA waitlist's. `waitlist migrate --adopt` takes it over
  once; nothing is converted.
- **Sessions.** The `authToken` cookie format is identical. Keep `app.token_secret` and nobody
  has to log in again.
- **`config.toml`.** The file format is a superset of the old one.
- **The data directory.** Fits, skills, skill plans, categories, modules, implants, notes, tags,
  translations and guides, in the same layout.
- **The frontend.** Same paths, request and response JSON, status codes, plain-text bodies,
  cookie and live events (see [What behaves differently](#what-behaves-differently) for the one
  frontend change the SRP service-account setup needs).

### Before you start

- A **TLA waitlist database** must have all of its own SQL migrations applied (up to
  `migration_mysql_0012.sql`). A database of the **original TDF waitlist** needs the TLA fork's
  later migrations applied first. `waitlist migrate --adopt` checks for the tables and columns
  those migrations add and names anything missing, without touching the database.
- MySQL 8.0 or later. Upgrading MySQL itself is a separate step; do it first if needed.
- Set up the new deployment as described in [deploy.md](deploy.md), but don't start it yet.

### Steps

1. **Stop the old backend** and its SSE server, so nothing writes to the database.

2. **Back up** the database and the data directory:

   ```sh
   mysqldump --single-transaction --routines --triggers waitlist | gzip > waitlist-before.sql.gz
   tar -czf data-before.tar.gz data/
   ```

3. **Point the new server at the database and adopt it.** With the production compose file, load
   the dump into its MySQL (or set `WAITLIST__DATABASE__URL` to your existing server instead), then:

   ```sh
   docker compose -f deploy/compose.prod.yaml up -d mysql
   gunzip -c waitlist-before.sql.gz | docker compose -f deploy/compose.prod.yaml exec -T mysql \
     sh -c 'MYSQL_PWD="$MYSQL_ROOT_PASSWORD" mysql -uroot waitlist'
   docker compose -f deploy/compose.prod.yaml run --rm waitlist migrate --adopt
   ```

   `--adopt` records the baseline migration (which *is* the old schema) as applied without running
   it, then applies the newer migrations. Those only add tables (the incursion tracker's), so the
   old backend can still run against the database. `serve` and a plain `migrate` refuse to touch
   an unadopted legacy database, so this can't happen by accident.

4. **Carry over the configuration.** Either mount the old `config.toml` at `/app/config.toml`
   (uncomment the volume in `compose.prod.yaml`), or move its values into `deploy/.env`:

   | Old `config.toml` | New |
   |---|---|
   | `[database] url` | `MYSQL_PASSWORD` (compose) or `WAITLIST__DATABASE__URL` |
   | `[app] token_secret` | `TOKEN_SECRET`. **Use the same value** to keep sessions. |
   | `[esi] client_id`, `client_secret`, `url` | `ESI_CLIENT_ID`, `ESI_CLIENT_SECRET`, `SITE_HOST` (the callback URL stays `https://<host>/auth/cb`, so the ESI application needs no change) |
   | `[sse] url`, `secret` | Not needed: live events are served by the backend itself. The section is ignored. |
   | `[fleet_updater]`, `[skill_updater]`, `[srp_updater]`, `[incursion_updater]`, `[war_updater]`, `[janice]`, `[discord]` | Same keys, same meaning. Put them in a mounted `config.toml` or set them as `WAITLIST__<SECTION>__<KEY>`. |
   | `Rocket.toml` (port) | `[server] bind` (default `0.0.0.0:5000`) |

   New, optional sections: `[srp]` (SRP prices and periods; the defaults are the old hard-coded
   values), `[incursion_tracker]` and `[data]`. See the
   [configuration reference](deploy.md#configuration).

5. **Copy the data directory** into the data volume (or bind-mount it, writable by uid 10001):

   ```sh
   docker compose -f deploy/compose.prod.yaml run --rm --no-deps -T --entrypoint sh waitlist \
     -c 'tar -C /app -xzf -' < data-before.tar.gz
   ```

   The archive must unpack to `data/…`. Leftover `*.example` files and old `*.backup.*` files are
   harmless. A non-empty data directory is never overwritten by the example data set.

   The old `sqlite-shrunk.sqlite` isn't needed: the server builds its own static data file on
   first start (it also reads the old layout, but needs the star map the old file lacks).

6. **Route `/api/*` to the new backend.** With the provided Caddyfile that's done; serve your
   existing frontend build with `FRONTEND_MODE=static`. If you keep your own proxy, send `/api/*`
   to port 5000 without stripping the prefix and remove the old `/sse` route.

7. **Start** (`docker compose -f deploy/compose.prod.yaml up -d`) and check: users are still
   logged in, the waitlist and fits pages load, and x-ups appear live for FCs.

**Rolling back**: stop the new backend and start the old one against the same database. It
ignores the `_sqlx_migrations` table and the incursion tracker's tables.

### Community rules

The old waitlist hard-coded rules for its own community: tags from roles and badges, VINDI badge
auto-approval, implant/module combinations, the hours-in-fleet limit, the AT-WAR tag, hidden
`ALTERNATIVE` fits, Kronos skill variants and the fleet MOTD roles. They now live in an optional
`rules.yaml` in the data directory. Without it, the built-in defaults reproduce the old behaviour
exactly; copy [`crates/fitting/defaults/rules.yaml`](../crates/fitting/defaults/rules.yaml) to
change them.

### What behaves differently

Everything the frontend relies on is unchanged. These are the intentional differences.

**Login**

- **SSO CSRF protection.** The old OAuth `state` was a fixed word (`normal`, `alt`, `srp_admin`,
  `srp_setup`), so another site could complete its own login in a user's browser, or link its
  character as the user's alt. The state is now `<kind>:<nonce>`, and the callback accepts it only
  with the matching `wlSsoNonce` cookie set by `GET /api/auth/login_url`. Every login, including
  the SRP service-account setup, works with the old frontend unchanged: its callback page
  compares `state === "srp_setup"`, which no longer matches, so it sends the setup login to
  `POST /api/auth/cb`; the server recognises the state there too, stores the service account and
  keeps the admin's own session.

- **`GET /api/auth/cb`** (the SRP service-account callback) requires `commanders-manage:admin`;
  it was open to anyone. For other states it redirects to the site origin instead of a hard-coded
  `http://localhost:3000`.

**Live events**

- `GET /api/sse/stream` streams the events itself instead of redirecting (307) to a separate SSE
  server. `EventSource` follows redirects, so clients don't notice. Topics, event names and
  payloads are unchanged, misspellings included.
- The x-up notification (`message`, "New x-up in waitlist") goes to the fleet boss's *account*.
  The old server addressed the boss character, which nobody received when the boss was an alt.
- `waitlist_update` is also sent where the old server was silent: after `POST /api/waitlist/empty`
  and `POST /api/waitlist/invite`, when a fleet registered for a waitlist is registered again or
  unregistered, and for the new waitlist administration. Old frontends just refetch.

**Waitlists**

- `POST /api/waitlist/set_open` and `POST /api/waitlist/empty` answer `404 Waitlist not found`
  for a waitlist that doesn't exist or is archived. The old server answered `set_open` with `OK`
  and changed nothing, and `empty` with `500 database error`. This is the only change to an
  existing endpoint's answer for a valid request.
- The fleet updater unregisters a fleet whose boss's access token ESI rejects (401) and can't be
  refreshed (or is rejected again after a refresh), like one whose boss has no token or scope.
  The old server kept retrying such a fleet forever.

**Data files**

- The admin data-file editor validates a file against the whole data set *before* writing it (the
  old one wrote first, so a bad edit broke the site until fixed), then writes it atomically and
  keeps backups.
- Skill plans are read from the loaded data set instead of re-reading `skillplan.yaml` on every
  request. Edits through the API apply immediately; after editing the file by hand, restart.
- A data file that doesn't load stops the server at startup with an error naming the file,
  instead of failing requests later. `POST /api/admin/data-files/{file}/reload` reloads the whole
  data set, and a reload that fails keeps the data currently in use.

**SRP**

- Prices, coverage periods and the FC discount roles are configurable (`[srp]`); the defaults are
  the old values. `GET /api/srp/status` and `GET /api/admin/srp/all-statuses` also return the
  configured prices as `srp_tiers` (an added field).
- Wallet reconciliation runs the wipe-and-rebuild of payments in one transaction, reads every
  journal page of the window (the old code read the first page only), de-duplicates deposits per
  time, amount *and* character, and doesn't credit one payout to two reports.

**Incursions**

- The high-sec incursion focus (used by SRP) is maintained by the new incursion tracker while it
  runs, on every ESI refresh instead of hourly; the rule that picks the focus is unchanged.
  Disable the tracker (`[incursion_tracker] enable = false`) to get the old updater back.

**Robustness**

- `GET /api/fleet/members` and `GET /api/fleet/fleetcomp` name a ship missing from the static
  data `Unknown type <id>` instead of failing the whole view.
- `GET /api/waitlist` answers `500 internal error` for an entry whose category was removed from
  `categories.yaml` (the old server panicked).
- `GET /api/commanders/roles` lists the roles you may grant in role order, lowest first.

**Additions** (nothing existing changed): the public incursion tracker (`/api/incursions*`,
[incursions-api.md](incursions-api.md)), the API reference (`/api/docs`, `/api/openapi.json`),
`GET /healthz`, and the waitlist extensions listed in [api.md](api.md#extensions-beyond-the-legacy-api):
waitlist administration, gate data, recorded invites, closed lists for FCs and the public status.
They add two tables (`fleet_waitlist`, `waitlist_invite`, migration 3); the old tables are
unchanged.
