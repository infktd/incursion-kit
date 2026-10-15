# Incursion tracker API

A public incursion tracker in the spirit of eve-incursions.de (retired) and eve-incursions.space:
active incursions with their systems and roles, influence history, state changes, ended
incursions and spawn windows. It records history from the moment it goes live; nothing is
imported.

Everything here is **new and additive**: no legacy endpoint or live event changed. The schemas
of every response are in the OpenAPI spec ([openapi.json](openapi.json), or `/api/docs` on a
running server); this page explains what the data means and how it is collected.

- No login. Every JSON response carries `Cache-Control: public, no-cache` and a strong `ETag` (a
  hash of the body): browsers and proxies may keep a copy but must revalidate it before reuse, so
  a refetch after an `incursions_update` event always gets the current data. Send the tag back as
  `If-None-Match` (browsers do this on their own) to get an empty `304 Not Modified` when nothing
  changed. Bodies that contain the current time (`generated_at` and the countdowns of
  `/api/incursions`, `/api/incursions/windows` and an active spawn's detail) change every second,
  so for those a `304` is rare; history and log pages revalidate cheaply.
- Times are RFC 3339 UTC strings (`2026-10-11T06:30:20Z`), except the influence series, which are
  `[unix_seconds, influence]` pairs for charting. Durations are seconds.
- Influence is ESI's value, `0.0` to `1.0`.
- Errors are plain text like the rest of the API: `400 <message>` for a bad query parameter,
  `404 incursion not found`.

## Endpoints

### `GET /api/incursions`

The active incursions, high-sec first (by staging security, highest first).

```json
{
  "generated_at": "2026-10-11T06:57:27Z",
  "last_poll_at": "2026-10-11T06:55:25Z",
  "data_modified_at": "2026-10-11T06:55:20Z",
  "incursions": [Spawn, ...]
}
```

`last_poll_at` is when the tracker last read ESI, `data_modified_at` when ESI generated that data
(`null` before the first poll). Each `Spawn` is complete (see below) except `events`, and its
`influence_history` covers the last 72 hours with one sample per hour (the latest of each hour).

### `GET /api/incursions/{id}`

One incursion, active or ended: a `Spawn` with every influence sample (one per ESI refresh, 5
minutes apart) and its `events`. `404 incursion not found` for an unknown or non-numeric id.

### `GET /api/incursions/history`

Ended incursions, most recently ended first.

| Query | Default | |
|---|---|---|
| `page` | 1 | 1-based |
| `per_page` | 20 | 1 to 100 |
| `band` | all | `highsec`, `lowsec` or `nullsec` |
| `constellation_id` | all | |
| `region_id` | all | |

```json
{"items": [Spawn, ...], "page": 1, "per_page": 20, "total": 42}
```

Items are summaries: a `Spawn` without `lifetime`, `systems`, `reference_jumps`,
`influence_history` and `events` (fetch `/api/incursions/{id}` for those).

### `GET /api/incursions/log`

State changes of every incursion, newest first (like eve-incursions.space's log).

| Query | Default | |
|---|---|---|
| `limit` | 50 | 1 to 200 |
| `before` | | an event id: return older events (use `next_before`) |

```json
{
  "items": [
    {"id": 812, "spawn_id": 40, "at": "2026-10-11T06:30:20Z", "kind": "state_changed",
     "from_state": "established", "to_state": "mobilizing",
     "constellation": {"id": 20000396, "name": "Vale"}, "security_band": "highsec",
     "staging_system": {"id": 30002708, "name": "Claysson"}}
  ],
  "next_before": 812
}
```

`next_before` is `null` on the last page.

### `GET /api/incursions/windows`

When the next incursion of each security band can spawn.

```json
{
  "generated_at": "2026-10-11T06:57:27Z",
  "opens_after_hours": 12,
  "closes_after_hours": 36,
  "bands": [
    {"band": "highsec", "slots": 1, "active": 1, "free_slots": 0, "windows": [], "unknown_windows": 0},
    {"band": "nullsec", "slots": 3, "active": 2, "free_slots": 1, "unknown_windows": 0,
     "windows": [
       {"after_spawn_id": 38, "after_constellation": {"id": 20000713, "name": "..."},
        "ended_at": "2026-10-10T13:55:02Z", "opens_at": "2026-10-11T01:55:02Z",
        "closes_at": "2026-10-12T01:55:02Z", "status": "open",
        "seconds_until_open": 0, "seconds_until_close": 68855}
     ]}
  ]
}
```

`status` is `pending` (before `opens_at`), `open`, or `overdue` (past `closes_at`: due any
minute). `unknown_windows` counts free slots whose end the tracker didn't see (it wasn't running
yet), so their window is unknown.

### `GET /api/incursions/stream`

Server-sent events for anonymous clients, on the `incursions` topic only (the authenticated
`/api/sse/stream` is unchanged and does not carry it). `GET /api/public/stream` carries the same
events together with the public waitlist `status_update` (see [api.md](api.md#live-events)), for
pages that want both on one connection. Same framing as the authenticated stream:
an opening `: connected` comment, then `event: <name>\ndata: <json>\n\n` plus keep-alive
comments every 15 seconds; a client that falls behind is
disconnected and should reconnect and refetch.

| Topic | Event | Data |
|---|---|---|
| `incursions` | `incursions_update` | `{"spawn_ids": [i64], "changes": [{"spawn_id", "kind", "from_state", "to_state"}], "influence_changed": [i64]}` |

It is published after every poll that recorded something: a new spawn, a state change, the boss
appearing or leaving, a staging change, an end (all in `changes`) or a new influence value
(`influence_changed`). Clients refetch `/api/incursions` (or the affected `/api/incursions/{id}`);
the responses are never served from a cache without revalidation, so no cache-busting is needed.

## The `Spawn` object

```json
{
  "id": 40,
  "active": true,
  "constellation": {"id": 20000396, "name": "Vale"},
  "region": {"id": 10000032, "name": "Sinq Laison"},
  "faction": {"id": 500019, "name": "Sansha's Nation"},
  "type": "Incursion",
  "security_band": "highsec",
  "staging_system": {"id": 30002708, "name": "Claysson", "security": 0.9, "security_status": 0.937687},
  "state": "established",
  "influence": 0.25,
  "has_boss": false,
  "first_seen_at": "2026-10-11T04:57:27Z",
  "observed_from_start": true,
  "last_seen_at": "2026-10-11T06:57:27Z",
  "state_changed_at": "2026-10-11T04:57:27Z",
  "mobilizing_at": null,
  "withdrawing_at": null,
  "boss_spawned_at": null,
  "ended_at": null,
  "duration_seconds": 7200,
  "lifetime": {
    "state_since": "2026-10-11T04:57:27Z",
    "state_since_observed": true,
    "max_despawn_at": "2026-10-19T04:57:27Z",
    "max_remaining_seconds": 684000
  },
  "systems": [
    {"id": 30002708, "name": "Claysson", "security": 0.9, "security_status": 0.937687,
     "role": "staging", "role_source": "known", "jumps_from_staging": 0,
     "radius_au": 25.1, "longest_warp_au": 37.6,
     "stations": [{"id": 60003565, "name": "Claysson X - Caldari Business Tribunal Accounting",
                   "has_repair": false, "has_cloning": true}]}
  ],
  "reference_jumps": [
    {"system_id": 30000142, "system_name": "Jita", "jumps": 12, "highsec_jumps": 14}
  ],
  "influence_history": [[1791694647, 0.0], [1791698247, 0.12]],
  "events": [{"id": 1, "spawn_id": 40, "at": "...", "kind": "spawned", "from_state": null, "to_state": "established"}]
}
```

- `state`: `mobilizing`, `established`, `withdrawing` while active, `ended` afterwards.
- `security`: as the game shows it (one decimal; 0.0 to 0.05 shows as 0.1);
  `security_status`: unrounded. `security_band` comes from the staging system's displayed
  security: high-sec from 0.5, low-sec from 0.1, null-sec at 0.0 and below.
- `first_seen_at` / `observed_from_start`: when the tracker first saw the spawn, and whether it
  was polling just before (so `first_seen_at` is the spawn time, within 5 minutes). Spawns that
  were already up when the tracker went live (or came back from downtime) have
  `observed_from_start: false`.
- `faction.name` is only known for Sansha's Nation; `region.name` may be `null` when the static
  data has no star map.
- `systems` are ordered staging, vanguard, assault, headquarters, unknown, then by name.
  `jumps_from_staging`, `radius_au`, `longest_warp_au` and `stations` need the star map in the
  static data file (`null` / empty without it).
- `reference_jumps`: from each configured reference system (`incursion_tracker.reference_systems`)
  to the staging system; `highsec_jumps` is the shortest route through high-sec only (`null` when
  there is none, e.g. for a low-sec staging system).
- Event `kind`s: `spawned`, `state_changed` (with `from_state`/`to_state`), `boss_spawned`,
  `boss_gone`, `staging_changed`, `ended` (with the last `from_state`).

## Rules and sources

### System roles

ESI names the staging system and lists the infested systems; it doesn't say which are vanguard,
assault or headquarters systems. eve-incursions.de kept those in its database (the
`solar_systems.systemType` column of its seed data; ESI only ever set the staging system). The
tracker uses, in order:

1. **`known`**: the layout eve-incursions.de recorded for the constellation (137 constellations,
   in `crates/incursions/data/layouts.json`, MIT, see `NOTICE`), when its staging system is the
   one ESI reports and it names every infested system.
2. **`estimated`**: otherwise, from the stargate graph. Jumps are counted from the staging system
   through the constellation. The farthest system is the headquarters (the lowest security when
   several are equally far). Of the other `n` non-staging systems, the nearest `ceil(n / 2)` are
   vanguards and the rest assaults (ties go to lower security first). This is our own heuristic,
   fitted to the recorded layouts: it reproduces 84 % of their systems and 73 % of their
   headquarters (`crates/incursions/tests/real_sde.rs` checks this).
3. **`unknown`**: without the star map.

Roles are stored when a spawn is first seen and when its staging system or infested systems
change.

### Maximum remaining time

Ported from eve-incursions.de (`spawn.tsx`, "Max. remaining"). After entering a state, an
incursion lasts at most:

| State | At most | |
|---|---|---|
| `established` | 8 days | up to 5 days established, 2 mobilizing, 1 withdrawing |
| `mobilizing` | 3 days | 2 days mobilizing, 1 withdrawing |
| `withdrawing` | 1 day | |

`max_despawn_at = state_since + that`. When `state_since_observed` is false the tracker didn't see
the state begin (it went live mid-state), so `state_since` is the first sighting and the real
maximum is earlier.

### Spawn windows

eve-incursions.de showed, after the last high-sec incursion ended: "no spawn possible" for 12
hours, then a spawn "in the next" 24 hours (until 36 hours after the end), then "any minute".
eve-incursions.space shows the same 12 h / 36 h windows for every band. The game keeps a fixed
number of incursions per band (`incursion_tracker.slots`: 1 high-sec, 1 low-sec, 3 null-sec by
default), so a band with `k` free slots gets one window per each of its `k` most recent ends:
`opens_at = ended_at + 12 h`, `closes_at = ended_at + 36 h`.

### System size

From the SDE: `radius_au` is the distance from the sun to the farthest planet, stargate or NPC
station; `longest_warp_au` is the largest distance between two of them (or the sun), roughly the
longest warp in the system (eve-incursions.de showed a similar "size, approximation of the
longest warp"). Moons and belts are left out.

### Stations

NPC stations from the SDE, with their in-game names (`<System> <planet>[ - Moon <n>] - <owner>
[<operation>]`) and whether they offer repair and cloning. Player structures are not listed: ESI
only describes them to authenticated characters with docking access.

### Jumps

Breadth-first search over the SDE stargate graph, in process. `highsec_jumps` only passes through
systems whose displayed security is 0.5 or more. Pochven and wormholes have no stargates to
k-space, so they are unreachable.

## How the data is collected

The incursion tracker worker (`crates/workers/src/incursion_tracker.rs`) polls ESI `/incursions`
right after ESI's cache for it expires (its `Expires` header; every 5 minutes), or every
`incursion_tracker.interval_seconds` when ESI sends none, and backs off after failures. Each poll:

- matches incursions to active spawns by constellation; new ones are stored with names, security
  and roles (from the star map, or ESI when the static data file lacks it);
- records state, boss and staging changes as events, and ends spawns ESI no longer lists (a single
  empty list is not trusted while spawns are active; it must repeat);
- stores one influence sample per spawn and ESI refresh, stamped with ESI's `Last-Modified`;
- maintains the legacy high-sec focus (`incursion_focus`, used by SRP) exactly like the legacy
  incursion updater, which it replaces while enabled;
- publishes `incursions_update` and posts the optional Discord notices.

Tables (migration `0002_incursion_tracker.sql`): `incursion_spawn`, `incursion_spawn_system`,
`incursion_influence`, `incursion_event`, `incursion_tracker_state`.

## Configuration

```toml
[incursion_tracker]
enable = true                 # default; also takes over the high-sec focus from the legacy updater
interval_seconds = 300        # poll interval when ESI sends no Expires header
reference_systems = ["Jita", "Amarr", "Dodixie", "Rens", "Hek"]   # add your home/staging system

[incursion_tracker.slots]     # concurrent incursions per band, for spawn windows
highsec = 1
lowsec = 1
nullsec = 3

[incursion_tracker.discord]   # webhook notices, off by default
enabled = false
webhook_url = ""
ping_role_id = ""             # pinged on new spawns only
bands = ["highsec", "lowsec", "nullsec"]
notify_spawn = true
notify_state = true
notify_boss = true
notify_end = true
```

The high-sec focus keeps following `incursion_updater.enable`: with the tracker enabled the
tracker writes it (every poll instead of hourly), with the tracker disabled the legacy updater
does, as before. The focus rule is unchanged: the first incursion in ESI's list whose staging
system has an unrounded security status of at least 0.5.

The star map comes from the static data file (`data.sde`). Files built before the star map was
added are rebuilt automatically at startup (`waitlist serve`), or by hand with `waitlist sde`;
until then the tracker still works, with names from ESI and without role estimates, sizes,
stations or jumps.
