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
runs the backend on the host with `cargo run`. Put the id and secret of an ESI test application
(callback `http://waitlist.localhost:8080/auth/cb`) into `config.toml` to log in. Your frontend's
dev server runs separately on port 4321; Caddy sends everything except `/api/*` to it. The API
reference is at `http://waitlist.localhost:8080/api/docs`.

Without the script: `docker compose -f deploy/compose.yaml up -d mysql caddy`, then
`cargo run -p waitlist` with a `config.toml` (see [docs/deploy.md](docs/deploy.md#configuration)).

## Tests

```sh
cargo run -p wl-eve-data --example build_sde -- deploy/sde/sde.sqlite   # once: static data for the tests
TEST_DATABASE_URL=mysql://root:root@127.0.0.1:3307 cargo test --workspace
```

- Database tests create a fresh database per test on `TEST_DATABASE_URL` and drop it afterwards;
  without the variable they are skipped. Tests that need the static data skip when
  `deploy/sde/sde.sqlite` (or `WL_SDE_PATH`) is missing.
- API tests run requests in-process against the real router (`crates/api/tests/common`).
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
| `waitlist` (`crates/waitlist`) | The binary: `serve`, `migrate`, `sde`, `init-data`, `openapi`, `healthcheck`. Wires everything together. |
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
| `wl-workers` (`crates/workers`) | Background workers: fleet, skill, war and incursion updaters and the tracker. |

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
