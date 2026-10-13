# Deploying incursion-kit

This guide runs the waitlist in production with Docker Compose: MySQL, the `waitlist` backend
and Caddy, which serves HTTPS, sends `/api/*` to the backend and everything else to your
frontend. Moving an existing waitlist over? Read [migrating.md](migrating.md) as well.

You need:

- A Linux host with Docker (Compose v2.24 or later), ports 80 and 443 reachable from the internet,
  and a DNS name pointing at it (below: `waitlist.example.org`).
- An EVE developer application (next section).
- A frontend: static files (a built single-page app) or a server Caddy can proxy to. The backend
  serves only the API.

## 1. Register the ESI application

At <https://developers.eveonline.com/applications>, create an application with
**Authentication & API Access** and:

- **Callback URL**: `https://waitlist.example.org/auth/cb` (your host, path `/auth/cb`; it is a
  frontend page, which hands the code to `POST /api/auth/cb`). It must match `esi.url` exactly.
- **Scopes**:
  `publicData`, `esi-skills.read_skills.v1`, `esi-clones.read_implants.v1`,
  `esi-fleets.read_fleet.v1`, `esi-fleets.write_fleet.v1`, `esi-ui.open_window.v1`,
  `esi-search.search_structures.v1`, and `esi-wallet.read_corporation_wallets.v1` if you use SRP.

Keep the client id and secret for the next step. For local development, register a second
application with the callback `http://waitlist.localhost:8080/auth/cb`.

## 2. Configure and start

Copy the deployment files to the host (the `deploy/` directory of the repository is enough) and
create the settings file:

```sh
cp deploy/.env.example deploy/.env
openssl rand -hex 24   # MYSQL_ROOT_PASSWORD, and again for MYSQL_PASSWORD
openssl rand -hex 32   # TOKEN_SECRET
$EDITOR deploy/.env
```

| Setting | |
|---|---|
| `SITE_HOST` | The public host name. Caddy obtains its certificate; the SSO callback is `https://$SITE_HOST/auth/cb`. |
| `ACME_EMAIL` | Contact address for the certificate authority. |
| `WAITLIST_IMAGE` | `ghcr.io/infktd/incursion-kit/waitlist:<version>`. Pin a version (`0.1`, `0.1.0`) rather than `latest`. |
| `MYSQL_ROOT_PASSWORD`, `MYSQL_PASSWORD` | Database passwords (the backend connects as user `waitlist`). |
| `TOKEN_SECRET` | 64 hex characters that encrypt session cookies. Changing it logs everyone out. |
| `ESI_CLIENT_ID`, `ESI_CLIENT_SECRET` | From step 1. |
| `FRONTEND_MODE` | `static` (serve `FRONTEND_DIR`) or `upstream` (proxy to `FRONTEND_UPSTREAM`). |
| `FRONTEND_DIR` | Directory with the built frontend, relative to `deploy/`. Default `./frontend`. |
| `FRONTEND_UPSTREAM` | `host:port` of a frontend server, e.g. a container you add to the compose file. |
| `RUST_LOG` | Log filter, default `info`. |
| `BACKUP_DIR`, `BACKUP_KEEP` | For `deploy/backup.sh`. |

`deploy/.env` holds secrets: it is git-ignored; keep it readable by root only.

```sh
docker compose -f deploy/compose.prod.yaml up -d
docker compose -f deploy/compose.prod.yaml logs -f waitlist
```

On the first start the backend:

1. writes the **example data set** into the empty data directory (a small doctrine with fits,
   skills, skill plans and categories; replace it with your community's, see
   [Data directory](#data-directory));
2. builds the **EVE static data file**: it downloads CCP's static data export (about 100 MB) and
   keeps a 7 MB extract, which takes a minute or two;
3. creates the database schema (migrations), starts the background workers and listens on port
   5000.

Then open `https://waitlist.example.org`, log in with your main character, and make it the
first administrator:

```sh
docker compose -f deploy/compose.prod.yaml exec mysql sh -c 'MYSQL_PWD="$MYSQL_ROOT_PASSWORD" mysql -uroot waitlist' <<'SQL'
INSERT INTO admin (character_id, role, granted_at, granted_by_id)
SELECT id, 'admin', UNIX_TIMESTAMP(), id FROM `character` WHERE name = 'Your Main';
SQL
```

Other roles are granted from the frontend (`/api/commanders`). Roles and what they allow:
[api.md](api.md#roles-and-permission-keys).

### The frontend

`deploy/Caddyfile.prod` routes `/api/*` to the backend and everything else to the frontend:

- `FRONTEND_MODE=static`: files from `FRONTEND_DIR` (mounted at `/srv/frontend`). Unknown paths
  fall back to `/index.html`, as single-page apps need.
- `FRONTEND_MODE=upstream`: a reverse proxy to `FRONTEND_UPSTREAM`. Add the frontend's service
  to `compose.prod.yaml` (or an override file) and set e.g. `FRONTEND_UPSTREAM=frontend:3000`.

The frontend must be on the same origin as the API (the session cookie is `SameSite=Strict`, and
there is no CORS). A server-rendered frontend that calls the API from the server must forward
the `authToken` cookie.

The interactive API reference is at `https://waitlist.example.org/api/docs`.

## The Docker image

`ghcr.io/infktd/incursion-kit/waitlist` is published for `linux/amd64` and `linux/arm64` from
every release tag (`0.1.0`, `0.1`, `latest`). It runs as user `waitlist` (uid 10001) in `/app`:

| Path | |
|---|---|
| `/app/data` (volume) | The data directory (`WAITLIST__DATA__DIR`). Writable: admin endpoints write here. |
| `/app/sde` (volume) | The static data file (`WAITLIST__DATA__SDE=/app/sde/sde.sqlite`). |
| `/app/config.toml` | Optional config file (`WAITLIST_CONFIG`). |
| port 5000 | HTTP (`server.bind`). Put it behind a TLS-terminating proxy. |

The entrypoint is the `waitlist` binary:

| Command | |
|---|---|
| `serve` (default) | Seed an empty data directory, build the static data if missing, migrate, run workers and serve. |
| `migrate [--adopt]` | Apply migrations and exit. `--adopt` takes over a legacy database ([migrating.md](migrating.md)). |
| `sde [--out PATH] [--build N] [--archive ZIP]` | (Re)build the static data file. |
| `init-data [--dir PATH] [--force]` | Write the example data set; `--force` overwrites example files in a non-empty directory. |
| `openapi` | Print the OpenAPI spec. |
| `healthcheck [--port N]` | Exit 0 when `GET /healthz` answers 200 (the image's `HEALTHCHECK`). |

A bind mount works instead of the `waitlist-data` volume when you prefer editing files on the
host, but the directory must be writable by uid 10001: `mkdir data && sudo chown 10001 data`.

Without Compose:

```sh
docker run -d --name waitlist -p 127.0.0.1:5000:5000 \
  -e WAITLIST__DATABASE__URL=mysql://waitlist:…@db-host:3306/waitlist \
  -e WAITLIST__APP__TOKEN_SECRET=… \
  -e WAITLIST__ESI__CLIENT_ID=… -e WAITLIST__ESI__CLIENT_SECRET=… \
  -e WAITLIST__ESI__URL=https://waitlist.example.org/auth/cb \
  -v waitlist-data:/app/data -v waitlist-sde:/app/sde \
  ghcr.io/infktd/incursion-kit/waitlist:0.1
```

## Configuration

Settings come from a TOML file (`WAITLIST_CONFIG`, default `./config.toml`; optional) and are
overridden by environment variables named `WAITLIST__<SECTION>__<KEY>` (upper case, double
underscores): `[esi] client_secret` is `WAITLIST__ESI__CLIENT_SECRET`, and
`[incursion_tracker.slots] highsec` is `WAITLIST__INCURSION_TRACKER__SLOTS__HIGHSEC`. Values are
parsed as numbers and booleans where the key expects them. Lists (prices, reference systems,
bands, roles) can only be set in the file.

The file format is a superset of the original waitlist's `config.toml`: an existing file works
unchanged, and sections this server doesn't use (such as `[sse]`) are ignored. Required:
`database.url`, `app.token_secret` and the `[esi]` keys `client_id`, `client_secret` and `url`.

```toml
[database]
url = "mysql://waitlist:password@mysql:3306/waitlist"

[app]
token_secret = "…64 hex characters…"

[esi]
client_id = "…"
client_secret = "…"
url = "https://waitlist.example.org/auth/cb"
```

### `[server]`

| Key | Default | |
|---|---|---|
| `bind` | `0.0.0.0:5000` | Listen address. |

### `[database]`

| Key | Default | |
|---|---|---|
| `url` | required | `mysql://user:password@host:3306/database`. MySQL 8.0 or later (MariaDB can't load the schema). |
| `min_connections` | `1` | Pool size. |
| `max_connections` | `10` | |
| `idle_timeout` | `60` | Seconds before an idle connection is closed. |
| `connect_timeout` | `5` | Seconds. |

### `[app]`

| Key | Default | |
|---|---|---|
| `token_secret` | required | 64 hex characters (32 bytes) encrypting session cookies. Keep it to keep sessions. |

### `[esi]`

| Key | Default | |
|---|---|---|
| `client_id`, `client_secret` | required | The ESI application. |
| `url` | required | The SSO callback URL, `https://<host>/auth/cb`. An `https:` URL also makes cookies `Secure`. |
| `user_agent` | `incursion-kit/<version> (+<origin of url>)` | `User-Agent` for ESI and SSO. CCP asks for an app name, a contact and a URL. |
| `esi_url` | CCP's ESI | ESI base URL override (testing). |
| `sso_authorize_url`, `sso_token_url` | CCP's SSO | SSO URL overrides (testing). |

### `[data]`

| Key | Default | |
|---|---|---|
| `dir` | `./data` | The [data directory](#data-directory). Must be writable. |
| `sde` | `./sde/sde.sqlite` | The [static data file](#eve-static-data). |

### Background workers

| Section | Keys (defaults) | |
|---|---|---|
| `[fleet_updater]` | `enable` (`true`), `min_in_fleet` (`8`) | Polls registered fleets: removes pilots who joined from the waitlist, publishes composition changes, and records fleet activity while a fleet has at least `min_in_fleet` members. |
| `[skill_updater]` | `enable` (`true`), `runtime` (`86400`) | Refreshes every pilot's skills, spread over `runtime` seconds. |
| `[war_updater]` | `enable` (`true`), `interval_seconds` (`3600`) | Wars; pilots at war can't x-up in blocked categories (`rules.yaml`). |
| `[incursion_updater]` | `enable` (`true`), `interval_seconds` (`3600`) | Keeps the high-sec incursion focus (SRP uses it). While the tracker runs, the tracker does this instead. |
| `[incursion_tracker]` | `enable` (`true`), `interval_seconds` (`300`), `reference_systems` (`["Jita", "Amarr", "Dodixie", "Rens", "Hek"]`) | The public incursion tracker ([incursions-api.md](incursions-api.md)). |
| `[incursion_tracker.slots]` | `highsec` (`1`), `lowsec` (`1`), `nullsec` (`3`) | Concurrent incursions per band, for spawn windows. |
| `[incursion_tracker.discord]` | `enabled` (`false`), `webhook_url`, `ping_role_id`, `bands` (all), `notify_spawn`, `notify_state`, `notify_boss`, `notify_end` (`true`) | Discord notices about incursions. |
| `[srp_updater]` | `enable` (`false`), `interval_seconds` (`300`), `fc_srp_price_factor` (`0.5`) | Reads the SRP service account's corporation wallet. FCs may pay `factor` × the price. |

### SRP

| Section | Keys (defaults) | |
|---|---|---|
| `[srp]` | `daily_prices` (`[20, 35, 45, 50, 55, 60, 65, 70, 75, 80]`), `per_focus_prices` (`[125, 225, 295, 330, 365, 400, 435, 470, 505, 540, 600, 680]`) | Coverage prices in millions of ISK; entry *n* covers *n* characters. A deposit of exactly a price buys that coverage. |
| | `daily_reset_hour_utc` (`11`), `per_focus_days` (`8`) | Daily coverage ends at this hour; per-focus coverage lasts this long or until the focus ends. |
| | `fc_discount_roles` (`["fc", "fc+", "admin"]`) | Roles that may pay the FC price. |
| | `journal_days` (`8`), `payout_match_days` (`7`) | Wallet journal window per run; how far back a payout matches approved reports. |
| `[janice]` | `api_key` (empty) | [Janice](https://janice.e-351.com) key for SRP loss appraisals. |
| `[discord]` | `enabled` (`false`), `srp_webhook_url`, `site_url` (from `esi.url`), `srp_ping_role_id` | Discord messages about SRP reports. |

SRP is optional: without a service account and `srp_updater.enable`, its pages show no coverage.
The service account is set up from the SRP admin page (an administrator logs in the corporation
character that reads the wallet).

## Data directory

The data directory (`data.dir`, `/app/data` in the image) holds everything your community
customises. Its layout is the original waitlist's, so an existing directory works as is.

| File | |
|---|---|
| `fits.dat` | Doctrine fits: an in-game fitting link dump (copy the fitting folder's links). |
| `fitnotes.yaml` | Notes shown next to fits and tiers. |
| `categories.yaml` | Waitlist categories and which hulls or modules put a fit in each. |
| `skills.yaml` | Skill categories and per-hull skill tiers (`min`, `elite`, …). |
| `skillplan.yaml` | Skill plans. |
| `modules.yaml` | Module alternatives, downgrades and fit-check exceptions. |
| `implants.yaml` | The implant guide. |
| `tags.yaml` | Tags every pilot may see. |
| `rules.yaml` | Optional community rules (below). |
| `locales/` | UI translations: `<page>.<locale>.json` and `languages.json`. |
| `guides/<slug>/` | Guides (`<slug>.<locale>.json`, Markdown body) and their images. |

**First run.** An empty (or missing) data directory gets the bundled example data set, so the
server starts with something to show. `waitlist init-data` writes it explicitly; `--force` writes
it into a non-empty directory, overwriting files of the same name. Replace the example files with
your own: through the admin data-file editor (`/api/admin/data-files`, which applies changes
immediately), or by hand followed by a restart. Translations and guides are read on every
request, so hand edits to them show up at once.

**Edits are safe.** The admin endpoints validate a community data file against the whole data set
before writing anything, write it atomically and keep the previous versions as
`<file>.backup.<unix time>`. A file edited by hand that doesn't load stops the server from
starting, with an error naming the file.

**`rules.yaml`** holds what the original waitlist hard-coded for its community: tags granted by
roles and badges, badge auto-approval, implant/module combinations, hours-in-fleet limits, the
tag for pilots at war, hidden fits, skill-plan variants and fleet MOTD roles. Without the file the
built-in defaults apply, which reproduce the original behaviour. To change them, copy
[`crates/fitting/defaults/rules.yaml`](../crates/fitting/defaults/rules.yaml) (from the release you
run) into the data directory and edit it; its comments describe each setting.

## EVE static data

The backend reads types, attributes and the star map from a SQLite extract of CCP's static data
export (SDE). `serve` builds it when the file is missing or older than the server's format.
Rebuild it after a game update that adds ships or modules, then restart:

```sh
docker compose -f deploy/compose.prod.yaml exec waitlist waitlist sde
docker compose -f deploy/compose.prod.yaml restart waitlist
```

`waitlist sde --build <number>` pins an SDE build; `--archive <file.zip>` converts an
already-downloaded `*-jsonl.zip` (for hosts without internet access). Without network access,
`serve` keeps an existing older file and logs the failure.

## Backups

`deploy/backup.sh` writes a consistent `mysqldump` of the database and an archive of the data
directory to `BACKUP_DIR` (default `deploy/backups`), keeping the newest `BACKUP_KEEP` (14) of
each. Run it daily from cron and copy the directory off the host:

```cron
0 4 * * * /opt/incursion-kit/deploy/backup.sh >> /var/log/waitlist-backup.log 2>&1
```

To restore, stop the backend, load the dump and unpack the data archive (the commands are at the
top of `backup.sh`), then start it again. The static data file and the Caddy volumes don't need
backups: they are rebuilt or re-issued.

## Upgrades

1. Read the release notes.
2. Back up (`deploy/backup.sh`).
3. Set the new version in `WAITLIST_IMAGE`, then
   `docker compose -f deploy/compose.prod.yaml pull waitlist && docker compose -f deploy/compose.prod.yaml up -d`.

The server applies new database migrations on start and rebuilds the static data file when its
format changed. Releases keep the API compatible; additions show up in the OpenAPI spec.

## Logs and monitoring

The backend logs to stdout (`docker compose logs waitlist`); `RUST_LOG` sets the filter (e.g.
`info,wl_api=debug`). Each request is logged with an `x-request-id`, which is also sent back as a
response header. `GET /healthz` answers `OK` when the server and the database are up; the
container reports it as its health status.

## Without Docker

Build with the Rust toolchain from `rust-toolchain.toml` (`cargo build --release -p waitlist`),
write a `config.toml`, and run `target/release/waitlist serve` behind a reverse proxy that sends
`/api/*` to it on the same origin as the frontend. A systemd unit only needs the working
directory, `WAITLIST_CONFIG` and a restart policy.

## Development

`deploy/dev.sh` starts MySQL and Caddy in Docker and runs the backend on the host at
`http://waitlist.localhost:8080`. See [CONTRIBUTING.md](../CONTRIBUTING.md).
