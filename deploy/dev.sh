#!/usr/bin/env sh
# One-command development stack: MySQL and Caddy in Docker, the backend on the host.
#
#   deploy/dev.sh            # then open http://waitlist.localhost:8080 (API: /api/..., docs: /api/docs)
#
# The first run writes ./config.toml (git-ignored) with a random session key, the dev database
# and placeholder ESI credentials: put your ESI test application's id and secret in it (callback
# http://waitlist.localhost:8080/auth/cb) to log in. The data directory deploy/data is seeded
# with the example data set, and the static data file deploy/sde/sde.sqlite is built if missing.
# Your frontend's dev server runs separately on port 4321; Caddy proxies everything but /api to it.
#
# Tests use the same MySQL:  TEST_DATABASE_URL=mysql://root:root@127.0.0.1:3307 cargo test --workspace
set -eu
cd "$(dirname "$0")/.."

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
	echo "wrote config.toml: set esi.client_id and esi.client_secret to log in"
fi

exec cargo run -p waitlist -- serve
