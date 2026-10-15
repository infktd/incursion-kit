# Contributing

Thanks for helping. This is a Rust workspace (edition 2024, Rust 1.99, pinned in
`rust-toolchain.toml`): one server binary and the library crates it is built from.

## Development setup

You need Rust (rustup picks up the pinned toolchain), Docker and, for the fitting tests, about
100 MB of download once for the EVE static data.

```sh
deploy/dev.sh
```

starts MySQL (on `127.0.0.1:3307`, root password `root`) and Caddy (on
`http://waitlist.localhost:8080`) in Docker, writes a development `config.toml` on first run and
runs the backend on the host with `cargo run`, on `127.0.0.1:5080` (where Caddy looks for it;
the script sets `WAITLIST__SERVER__BIND`, so an older `config.toml` with port 5000 still works).
The production default, 5000, is avoided because on macOS the AirPlay receiver answers on it too,
and Caddy would keep proxying to AirPlay after a backend restart. Put the id and secret of an ESI test application
(callback `http://waitlist.localhost:8080/auth/cb`) into `config.toml` to log in. Your frontend's
dev server runs separately on port 4321; Caddy sends everything except `/api/*` to it. The API
reference is at `http://waitlist.localhost:8080/api/docs`.

Without the script: `docker compose -f deploy/compose.yaml up -d mysql caddy`, then
`cargo run -p waitlist` with a `config.toml` binding `127.0.0.1:5080` (see
[docs/deploy.md](docs/deploy.md#configuration)).

## Develop a frontend without a CCP account

```sh
deploy/dev.sh --dev-esi
```

runs the same stack, but the backend talks to **dev-esi** (`crates/dev-esi`), a local stand-in
for EVE SSO and ESI, instead of CCP. No ESI application or EVE account is needed: "Log in with EVE"
shows a page listing made-up characters, and you pick one. dev-esi runs on the host
(`127.0.0.1:8090`); Caddy also serves it under `http://waitlist.localhost:8080/dev-esi/`, and its
dashboard is at `http://waitlist.localhost:8080/dev-esi/_dev`. Your `config.toml` is not changed:
the script points the backend at dev-esi through `WAITLIST__ESI__*` environment variables
(`client_id` `dev-esi-client`, `client_secret` `dev-esi-secret`, `esi_url`, `sso_authorize_url`,
`sso_token_url`), and the server logs a warning whenever these URLs are overridden.

The default seed (`crates/dev-esi/seed.yaml`, printed by `cargo run -p dev-esi -- seed`) has:

| Character | |
|---|---|
| Dev FC Aurora | waitlist **admin**, boss of a fleet that exists at start; alts Dev FC Aurora Alt (logi) and Dev FC Aurora Scout |
| Dev FC Brann | **fc** role, not in a fleet |
| Dev Logi Mira | logi pilot, passes the Nestor fits |
| Dev Vindi Tarek | DPS with the HYBRID implant set, passes the Vindicator fits |
| Dev Marauder Trainee | all doctrine skills but Marauders: Vindicator passes, Kronos/Paladin/Vargur fail |
| Dev Newbro Pell | basic skills: fails every doctrine fit |
| Dev Banned Rook | corporation banned on the waitlist: login is refused |
| Dev Wartarget Vex | corporation at war: AT-WAR tag, logi blocked |
| Dev SRP Ledger | member of the SRP wallet corporation (log it in as the SRP service account) |

plus three incursions (high-, low- and null-sec), two killmails and an SRP wallet with deposits.
`dev.sh --dev-esi` grants the roles and the ban with `cargo run -p dev-esi -- sql` (SQL for the
waitlist database). Use your own seed with `--seed` (or `DEV_ESI_SEED`).

dev-esi simulates fleets: when an FC invites a pilot, the pilot joins the requested squad after
`fleet.invite_accept_delay_ms` (2 s), so the fleet updater takes them off the waitlist as in game.
Everything else is changed through its control API (JSON under `/_dev/`, listed in
`crates/dev-esi/src/control.rs`): form fleets, move pilots in and out, make someone boss, accept or
decline invites, change skills, implants and ships, add killmails and wallet journal entries, set
incursions and wars, revoke a character's tokens, read the event log (invites, MOTD changes,
windows opened), and `POST /_dev/reset` back to the seed. A frontend's end-to-end tests can drive
scenarios through it.

dev-esi's access tokens are RS256 JWTs with EVE's claims, and it publishes its key at
`/oauth/jwks`. The waitlist does not verify token signatures (it receives tokens directly from
the SSO token endpoint and only reads their claims), so nothing else needs configuring.

To run everything in containers instead: `docker compose -f deploy/compose.yaml -f
deploy/compose.dev-esi.yaml --profile backend up -d --build` (see that file). dev-esi is never part
of the production image or `compose.prod.yaml`; never point a public server at it, since anyone
who reaches it can log in as any of its characters.

## Tests

```sh
cargo run -p wl-eve-data --example build_sde -- deploy/sde/sde.sqlite   # once: static data for the tests
TEST_DATABASE_URL=mysql://root:root@127.0.0.1:3307 cargo test --workspace
```

- Database tests create a fresh database per test on `TEST_DATABASE_URL` and drop it afterwards;
  without the variable they are skipped. Tests that need the static data skip when
  `deploy/sde/sde.sqlite` (or `WL_SDE_PATH`) is missing.
- API tests run requests in-process against the real router (`crates/api/tests/common`).
- `crates/dev-esi/tests/e2e.rs` runs the server over HTTP against dev-esi: SSO login, x-up,
  fleet invite and the fleet updater seeing the pilot join.
- Snapshot tests use [insta](https://insta.rs): review changes with `cargo insta review`.
- The test data set is the bundled example data (`crates/fitting/example-data`), the same files
  a new install starts with.

CI runs, and a pull request must pass:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
TEST_DATABASE_URL=… cargo test --workspace --locked
cargo run -p waitlist -- openapi | diff docs/openapi.json -   # the committed spec is current
```

plus `cargo deny check` (licences and advisories) and a Docker build.

## Crate map

| Crate | |
|---|---|
| `waitlist` (`crates/waitlist`) | The binary: `serve`, `migrate`, `sde`, `init-data`, `wiki import`, `openapi`, `healthcheck`. Wires everything together. |
| `wl-api` (`crates/api`) | The HTTP API (axum): routes, sessions, permissions, error mapping, the OpenAPI spec. Start with its [README](crates/api/README.md). |
| `wl-core` (`crates/core`) | Configuration and wire types shared by several crates. |
| `wl-db` (`crates/db`) | MySQL pool, migrations (`crates/db/migrations`), legacy-database adoption, test databases. |
| `wl-esi` (`crates/esi`) | EVE SSO and ESI on [eve-esi-client](https://crates.io/crates/eve-esi-client): login, stored tokens, the endpoints the waitlist uses. |
| `wl-eve-data` (`crates/eve-data`) | EVE static data: the SDE extract builder and type, dogma and star-map lookups. |
| `wl-fitting` (`crates/fitting`) | Fits and doctrines: parsing, the fit checker, categories, skills, skill plans, community rules, the data store and the bundled example data. |
| `wl-content` (`crates/content`) | Translations, guides and guide images in the data directory. |
| `wl-live` (`crates/live`) | The in-process hub behind the server-sent event streams. |
| `wl-srp` (`crates/srp`) | Ship replacement program: reports, coverage, wallet reconciliation. |
| `wl-incursions` (`crates/incursions`) | The incursion tracker: spawn history, system roles, spawn windows. |
| `wl-wiki` (`crates/wiki`) | The wiki: sections, pages, revisions, images, search, Markdown directive validation and the Markdown/MDX importer ([docs/wiki.md](docs/wiki.md)). |
| `wl-workers` (`crates/workers`) | Background workers: fleet, skill, war and incursion updaters and the tracker. |
| `dev-esi` (`crates/dev-esi`) | Development only: a fake EVE SSO and ESI with a control API, for running the stack (and end-to-end tests) without a CCP account. |

## Conventions

- **Compatibility first.** Existing frontends depend on the API exactly as it is: paths, JSON
  shapes, status codes, plain-text bodies, cookie and event names. Changes are additive; an
  intentional behaviour change needs a good reason and a note in
  [docs/migrating.md](docs/migrating.md).
- **Routes** follow [crates/api/README.md](crates/api/README.md): every handler has a
  `#[utoipa::path]` annotation and is registered with `routes!`, so it is in the spec.
  `crates/api/tests/openapi.rs` enforces this. After an API change, regenerate the spec:
  `cargo run -p waitlist -- openapi > docs/openapi.json`.
- **Database**: runtime-checked `sqlx::query*` (never the `query!` macros: the build must not
  need a database). Schema changes are new files in `crates/db/migrations`; never edit a
  released migration.
- **Dependencies** are declared once in the root `Cargo.toml` (`[workspace.dependencies]`) and
  used with `workspace = true`. Prefer the latest release, and check its licence against
  `deny.toml`.
- **No panics on input**: map failures to errors (`ApiError` in the API). `unsafe` is forbidden.
- **Community-specific behaviour** belongs in data (`rules.yaml`, the data files), not code.
- Code comments explain *why*; module docs describe behaviour and its edge cases.

## Releases

Pushing a `v*` tag builds the multi-architecture image and publishes it to
`ghcr.io/infktd/incursion-kit/waitlist` (`.github/workflows/release.yml`).

## Licence

By contributing you agree that your contributions are licensed under the MIT licence of the
project (see `LICENSE`).
