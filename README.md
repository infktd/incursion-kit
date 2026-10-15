# incursion-kit

The waitlist backend for EVE Online high-sec incursion communities: x-ups with automatic fit
checks, fleet management, pilot skills and history, SRP and a public incursion tracker, behind a
documented HTTP API.

It is backend-only. It is API- and database-compatible with the TDF/TLA waitlist, so existing
communities migrate by pointing it at their current database and keep their current frontend;
new ones bring their own frontend or build one against the API.

## Features

- **Waitlist**: x-up with several fits and alts, fit checking against your doctrine (tags,
  downgrades, missing skills, implants), approve/reject, invites, live updates over server-sent
  events.
- **Fleets**: fleet registration with squads per category, member and composition views, MOTD
  role assignment, fleet history and statistics.
- **Pilots**: skills and skill plans, x-up and skill history, notes, badges, bans, roles.
- **SRP**: loss reports, subscription coverage from a corporation wallet, appraisals, Discord
  notices; prices configurable.
- **Incursion tracker**: active incursions with system roles, influence history, spawn windows and
  a public event stream ([docs/incursions-api.md](docs/incursions-api.md)).
- **Your community's rules as data**: doctrine fits, skills, categories and community rules live
  in editable files with an admin editor; guides and translations included.
- **Operations**: one container, automatic migrations and static-data builds, health checks,
  an OpenAPI 3.1 spec with an interactive reference at `/api/docs`.

## Quick start

```sh
git clone https://github.com/infktd/incursion-kit && cd incursion-kit
cp deploy/.env.example deploy/.env    # set the host name, passwords, session key and ESI app
docker compose -f deploy/compose.prod.yaml up -d
```

Caddy serves `https://<your host>` with an automatic certificate, `/api/*` goes to the backend
and everything else to your frontend. A fresh install starts with an example data set (a small
doctrine) to replace with your own. The full walk-through, including registering the ESI
application and making yourself admin, is in [docs/deploy.md](docs/deploy.md).

For development, `deploy/dev.sh` starts MySQL and Caddy in Docker and runs the backend with
cargo; `deploy/dev.sh --dev-esi` does it without a CCP account, against a local stand-in for EVE
SSO and ESI (see [CONTRIBUTING.md](CONTRIBUTING.md)).

## Documentation

- [docs/deploy.md](docs/deploy.md): production deployment, configuration reference, data
  directory, static data, backups and upgrades.
- [docs/migrating.md](docs/migrating.md): moving an existing waitlist over.
- [docs/api.md](docs/api.md): sessions and login, roles and permission keys, errors, live events.
- [docs/openapi.json](docs/openapi.json): the OpenAPI 3.1 spec of every endpoint (also served at
  `/api/openapi.json`, browsable at `/api/docs`).
- [docs/incursions-api.md](docs/incursions-api.md): the incursion tracker API.
- [CONTRIBUTING.md](CONTRIBUTING.md): development setup, tests, crate map, conventions.

## ESI from Rust

ESI access is built on [eve-esi-client](https://crates.io/crates/eve-esi-client): every ESI
endpoint generated from CCP's spec, with rate-limit, error-limit and cache handling built in.
If you're writing Rust against ESI, use it.

## Licence

MIT (see [LICENSE](LICENSE)). Parts are derived from other MIT-licensed projects; see
[NOTICE](NOTICE).
