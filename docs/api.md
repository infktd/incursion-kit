# The waitlist API

incursion-kit is a backend: everything a waitlist frontend needs is an HTTP endpoint under `/api`.
This guide covers what the reference doesn't: sessions and login, roles and permission keys, the
error model and live events. Every endpoint, with its parameters, bodies, responses and required
permission, is in the OpenAPI reference.

## Reference

- **OpenAPI 3.1 spec**: [`docs/openapi.json`](openapi.json) in the repository, and
  `GET /api/openapi.json` on a running server.
- **Interactive reference**: `GET /api/docs` on a running server (Scalar; the page loads its
  script from the jsDelivr CDN).
- The spec is generated from the code (`waitlist openapi` prints it), so it always matches the
  server it came from. Operations that need a login list the `session` security scheme; its
  scopes are the [permission keys](#roles-and-permission-keys) the operation requires.
- The public incursion tracker (`/api/incursions*`) has its own guide:
  [incursions-api.md](incursions-api.md).

## Conventions

- **Same origin.** The API is meant to be served on the same origin as the frontend, behind a
  reverse proxy that sends `/api/*` to the backend (see [deploy.md](deploy.md)). There is no CORS
  support; the session cookie is `SameSite=Strict`.
- **Requests.** Bodies are JSON. The `Content-Type` header is optional (old clients send JSON
  without it). Some admin endpoints take raw text or bytes (data files, guide images); the spec
  says which.
- **Responses.** JSON, except where the spec says `text/plain`: several endpoints answer with a
  plain-text `OK` (or a short message), and every error is plain text.
- **Ids.** Accounts are identified by their main character's id. Character, corporation,
  alliance and type ids are EVE ids. Times are Unix seconds unless a field says otherwise (the
  incursion tracker uses RFC 3339 strings).
- **Waitlists.** The schema supports several waitlists; the default one has id `1`.

## Sessions and login

The session is the `authToken` cookie:
`authToken=<token>; Path=/; HttpOnly; SameSite=Strict; Max-Age=2678400`, plus `Secure` when the
site is served over HTTPS (when `esi.url` starts with `https:`). The token is a Branca token
(XChaCha20-Poly1305 keyed by `app.token_secret`) over `{version, account_id,
window_character_id}`; it expires after 31 days and is only issued at login. Keeping the same
`app.token_secret` keeps sessions valid across restarts, upgrades and migrations.

`GET /api/auth/whoami` returns the account (`account_id`, its `access` keys and its
`characters`: the main first, then alts), or `401` when not logged in. Frontends use it on load.

### EVE SSO login

1. The frontend calls `GET /api/auth/login_url` (flags: `alt=true` to link an alt, `fc=true` for
   the fleet-command scopes, `srp_admin=true` for the SRP administrator login). The answer is the
   EVE SSO URL as text, and a `wlSsoNonce` cookie (`HttpOnly`, `Path=/api/auth`, 15 minutes).
2. The browser goes to that URL. EVE redirects back to the **frontend page** `/auth/cb?code=…&state=…`
   (the callback URL registered with the ESI application and configured as `esi.url`).
3. That page calls `POST /api/auth/cb` with `{"code": "…", "state": "…"}`. The backend checks the
   state against the nonce cookie, exchanges the code for tokens, refreshes the character's
   corporation and alliance, and sets `authToken`.
   - `403` with a JSON ban object as text (`{"category", "expires_at", "reason"}`): the character
     is banned.
   - `400` `Login state mismatch: …`: the login expired or was started in another browser.
4. For the SRP service account (`GET /api/admin/srp/setup` hands out a login URL whose state
   starts with `srp_setup`), the page calls `GET /api/auth/cb?code=…&state=…` instead; it stores
   the character as the service account and redirects to `/fc/srp`.

**The CSRF nonce.** The OAuth `state` is `<kind>:<nonce>`, where kind is `normal`, `alt`,
`srp_admin` or `srp_setup`, and the callback accepts it only when the nonce matches this
browser's `wlSsoNonce` cookie. This stops another site from completing *its* login in a
victim's browser (login CSRF) or linking its character as the victim's alt. Frontends must pass
`state` through unchanged and compare only its prefix (`state.split(":")[0]`) when they branch on
the kind.

Scopes requested: every login asks for `publicData`, `esi-skills.read_skills.v1` and
`esi-clones.read_implants.v1`; `fc=true` adds `esi-fleets.read_fleet.v1`,
`esi-fleets.write_fleet.v1`, `esi-ui.open_window.v1` and `esi-search.search_structures.v1`; the
SRP administrator login adds `esi-ui.open_window.v1`; the SRP service account asks for
`publicData` and `esi-wallet.read_corporation_wallets.v1` only. When a character logs in again,
the scopes it granted before are kept where EVE allows it.

**Alts.** An `alt` login while logged in links the character to the current account (a character
that holds a role cannot become an alt). Endpoints that act for a character accept the main and
its alts. `GET /api/auth/logout` clears the cookie and, as the original waitlist did, unlinks all
of the account's alts.

## Roles and permission keys

Each account has one role (stored for its main character; no role means `user`). Roles are
cumulative: each has every key of the roles before it, plus its own.

| Role | Adds the keys |
|---|---|
| `user` | (none) |
| `trainee` | `fleet-configure`, `fleet-invite`, `fleet-view`, `pilot-view`, `waitlist-view`, `waitlist-tag:TRAINEE` |
| `trainee-advanced` | `fit-view`, `skill-view`, `waitlist-manage` |
| `fc` | `bans-manage`, `badges-manage`, `commanders-view`, `fleet-activity-view`, `fleet-history-view`, `fit-history-view`, `search`, `skill-history-view`, `waitlist-edit`, `stats-view`, `waitlist-tag:HQ-FC`, `notes-view`, `notes-add` |
| `fc+` | `commanders-manage`, `commanders-manage:trainee`, `commanders-manage:trainee-advanced`, `commanders-manage:fc` |
| `admin` | `commanders-manage:admin`, `commanders-manage:fc+` |

What the keys allow:

| Key | Allows |
|---|---|
| `fleet-view` | Fleet status, members and composition; the `fleet_comp` live topic; the FC SRP pages |
| `fleet-configure` | Registering and closing fleets, MOTD role assignment |
| `fleet-invite` | Inviting pilots from the waitlist; ESI name search |
| `pilot-view`, `skill-view` | Other pilots' information and skills (everyone sees their own characters) |
| `waitlist-view`, `fit-view` | Seeing other pilots' entries and their fits on the waitlist |
| `waitlist-manage` | Approving, rejecting and removing fits |
| `waitlist-edit` | Opening, closing and emptying the waitlist |
| `waitlist-tag:TRAINEE`, `waitlist-tag:HQ-FC` | The tag added to the holder's own fits (see `rules.yaml`); `waitlist-tag:HQ-FC` also manages announcements and lists a pilot's linked characters |
| `fleet-activity-view`, `fleet-history-view` | Pilots' fleet activity; fleet composition history |
| `fit-history-view`, `skill-history-view` | Pilots' x-up and skill-change history |
| `bans-manage`, `badges-manage` | Bans; badges and their members |
| `notes-view`, `notes-add` | Pilot notes |
| `search` | Pilot search |
| `stats-view` | Statistics |
| `commanders-view` | The list of role holders |
| `commanders-manage`, `commanders-manage:<role>` | Granting and revoking roles; the role-specific key is needed to grant or revoke that role |
| `commanders-manage:admin` | Site administration: data files, guides and translations, skill plans, SRP administration |

The OpenAPI spec lists the key each operation needs. Missing it gives `401 access denied`.

**The first administrator** has no one to grant them the role. Log in once, then insert the role
by hand:

```sql
INSERT INTO admin (character_id, role, granted_at, granted_by_id)
SELECT id, 'admin', UNIX_TIMESTAMP(), id FROM `character` WHERE name = 'Your Main';
```

From then on, roles are managed with `/api/commanders`.

## Errors

Every error is a plain-text body with one of these statuses:

| Status | Body | When |
|---|---|---|
| 400 | a message | invalid input (`type error: …`, `fit error: …`, …), or a failed check |
| 401 | `401 Authorization Required` | no valid session on an endpoint that needs one |
| 401 | `access denied` | missing permission key, or the character isn't yours |
| 401 | `ESI error: missing ESI scope` / `ESI error: no ESI token found` | the character must log in again (with the needed scopes) |
| 403 | a message | forbidden action (a ban at login carries a JSON object as text) |
| 404 | `404 Not Found` | unknown path, unsupported method (there is no 405), or a missing or malformed path or query parameter |
| 404 | a message | a resource that doesn't exist (`incursion not found`, …) |
| 422 | `invalid JSON body: …` | a body that doesn't match the expected JSON |
| 4xx/5xx | `ESI error: …` | an ESI call failed (ESI's status when it is meaningful) |
| 500 | `database error` / `internal error` | server errors; the details are only logged |

Every response carries an `x-request-id` header (generated unless the request sent one); the
server's log lines for that request carry the same id.

## Live events

Two server-sent event (SSE) streams carry live updates. Each event has a name (`event:`) and data
(`data:`, JSON or plain text). A stream opens with a `: connected` comment, sent at once so that
proxies which buffer pass the response on immediately; keep-alive comments follow every 15
seconds.

- `GET /api/sse/stream` (logged in): the topics `announcments`, `waitlist` and
  `account;<account_id>`, plus `fleet_comp` with `fleet-view`. The topic set is fixed when the
  stream opens; reconnect after a role change.
- `GET /api/incursions/stream` (public): the topic `incursions`.

| Topic | Event | Data | Clients |
|---|---|---|---|
| `waitlist` | `waitlist_update` | `{"waitlist_id": 1}` | refetch `/api/waitlist` |
| `fleet_comp` | `comp_update` | `{"fleet_id": …}` | refetch fleet members |
| `announcments` | `announcment;new`, `announcment;updated` | JSON list of all active announcements | replace their list |
| `account;<id>` | `wakeup` | plain text, e.g. "… has invited your … to fleet." | notify the pilot |
| `account;<id>` | `message` | `{"message": "New x-up in waitlist"}` (to fleet bosses) | notify |
| `incursions` | `incursions_update` | `{"spawn_ids", "changes", "influence_changed"}` | refetch `/api/incursions` |

The topic `announcments` and the events `announcment;new` / `announcment;updated` are
misspelled on purpose: the names are part of the protocol existing frontends use.

Delivery is best-effort: no persistence, no replay, no event ids. A client that falls too far
behind is disconnected; browsers' `EventSource` reconnects by itself, and clients should refetch
what they show when the stream (re)opens. Proxies must not buffer the stream (the backend sends
`X-Accel-Buffering: no`; Caddy needs no configuration).

## Health

`GET /healthz` (outside `/api`) answers `OK` when the server is up and the database answers. The
Docker image's health check (`waitlist healthcheck`) uses it.
