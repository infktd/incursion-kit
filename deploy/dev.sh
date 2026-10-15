#!/usr/bin/env sh
# One-command development stack: MySQL and Caddy in Docker, the backend on the host.
#
#   deploy/dev.sh            # then open http://waitlist.localhost:8080 (API: /api/..., docs: /api/docs)
#   deploy/dev.sh --dev-esi  # the same, but logging in through dev-esi, a fake EVE SSO and ESI on
#                            # the host: no CCP account or ESI application needed
#
# The first run writes ./config.toml (git-ignored) with a random session key, the dev database
# and placeholder ESI credentials: put your ESI test application's id and secret in it (callback
# http://waitlist.localhost:8080/auth/cb) to log in. The data directory deploy/data is seeded
# with the example data set, and the static data file deploy/sde/sde.sqlite is built if missing.
# Your frontend's dev server runs separately on port 4321; Caddy proxies everything but /api to it.
#
# With --dev-esi the backend's ESI settings come from the environment (config.toml is left as it
# is), dev-esi's characters get their waitlist roles and bans, and dev-esi's login page and
# dashboard are under http://waitlist.localhost:8080/dev-esi/ (dashboard: /dev-esi/_dev). Its
# signing key and refresh tokens are kept in deploy/dev-esi-state, so logins survive restarts.
# See CONTRIBUTING.md ("Develop a frontend without a CCP account").
#
# Tests use the same MySQL:  TEST_DATABASE_URL=mysql://root:root@127.0.0.1:3307 cargo test --workspace
set -eu
cd "$(dirname "$0")/.."

dev_esi=false
case "${1:-}" in
	--dev-esi) dev_esi=true ;;
	"") ;;
	*)
		echo "usage: deploy/dev.sh [--dev-esi]" >&2
		exit 2
		;;
esac

docker compose -f deploy/compose.yaml up -d --wait mysql caddy

if [ ! -f config.toml ]; then
	secret=$(od -An -tx1 -N32 /dev/urandom | tr -d ' \n')
	cat > config.toml <<TOML
# Development configuration written by deploy/dev.sh (git-ignored). See docs/deploy.md.
[database]
url = "mysql://waitlist:waitlist@127.0.0.1:3307/waitlist"

[app]
token_secret = "$secret"

[esi]
client_id = "replace-me"
client_secret = "replace-me"
url = "http://waitlist.localhost:8080/auth/cb"

[server]
bind = "127.0.0.1:5000"

[data]
dir = "deploy/data"
sde = "deploy/sde/sde.sqlite"
TOML
	if [ "$dev_esi" = false ]; then
		echo "wrote config.toml: set esi.client_id and esi.client_secret to log in"
	fi
fi

if [ "$dev_esi" = false ]; then
	exec cargo run -p waitlist -- serve
fi

# --- dev-esi -----------------------------------------------------------------------------------
dev_esi_port=${DEV_ESI_PORT:-8090}
export WAITLIST__ESI__CLIENT_ID=dev-esi-client
export WAITLIST__ESI__CLIENT_SECRET=dev-esi-secret
export WAITLIST__ESI__URL=http://waitlist.localhost:8080/auth/cb
export WAITLIST__ESI__ESI_URL="http://127.0.0.1:$dev_esi_port"
export WAITLIST__ESI__SSO_TOKEN_URL="http://127.0.0.1:$dev_esi_port/v2/oauth/token"
# The browser is sent here: through Caddy, so it works wherever the site does.
export WAITLIST__ESI__SSO_AUTHORIZE_URL=http://waitlist.localhost:8080/dev-esi/v2/oauth/authorize

cargo build -p waitlist -p dev-esi
# dev-esi serves type and system names from the static data file: build it first if missing.
[ -f deploy/sde/sde.sqlite ] || cargo run -q -p waitlist -- sde

cargo run -q -p dev-esi -- --bind "127.0.0.1:$dev_esi_port" --sde deploy/sde/sde.sqlite \
	--state-dir deploy/dev-esi-state &
dev_esi_pid=$!
trap 'kill "$dev_esi_pid" 2>/dev/null || true' EXIT
trap 'exit 130' INT TERM
tries=0
until curl -fsS "http://127.0.0.1:$dev_esi_port/_dev/health" >/dev/null 2>&1; do
	tries=$((tries + 1))
	if [ "$tries" -gt 150 ] || ! kill -0 "$dev_esi_pid" 2>/dev/null; then
		echo "dev-esi did not start" >&2
		exit 1
	fi
	sleep 0.2
done

# Roles (an admin and an FC) and a banned corporation for dev-esi's characters.
cargo run -q -p waitlist -- migrate
cargo run -q -p dev-esi -- sql \
	| docker compose -f deploy/compose.yaml exec -T -e MYSQL_PWD="${MYSQL_ROOT_PASSWORD:-root}" \
		mysql mysql -uroot waitlist

echo "dev-esi: log in at http://waitlist.localhost:8080 (any character, no password);" \
	"dashboard at http://waitlist.localhost:8080/dev-esi/_dev"
cargo run -q -p waitlist -- serve
