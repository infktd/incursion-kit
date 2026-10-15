The HTTP API of the incursion-kit waitlist server. It is wire-compatible with the legacy
community waitlist, so existing frontends keep working.

- **Authentication**: the `authToken` session cookie, set by `POST /api/auth/cb` after an EVE SSO
  login (`GET /api/auth/login_url`). Operations that need a login list the `session` scheme;
  its scopes are the permission keys the operation requires.
- **Errors** are plain text (`text/plain`), not JSON: `401 Authorization Required` without a
  session, `access denied` without a permission, `404 Not Found` for unknown paths, unsupported
  methods and malformed parameters.
- **Request bodies** are JSON; the `Content-Type` header is optional.
- **Live events** are server-sent events: `GET /api/sse/stream` (logged in; `?public=1` adds the
  public topics), `GET /api/incursions/stream` (public) and `GET /api/public/stream` (public:
  waitlist status and incursions).
- **Extensions** beyond the legacy API (waitlist administration, gates, recorded invites, public
  status) say so in their descriptions; they add endpoints and fields without changing existing
  ones.

The guide to the API (sessions, roles and permission keys, live event topics) is `docs/api.md`
in the repository.
