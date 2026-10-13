#!/usr/bin/env sh
# Back up the production stack (deploy/compose.prod.yaml): a MySQL dump of the `waitlist`
# database and an archive of the data directory, keeping the newest $BACKUP_KEEP of each.
#
#   deploy/backup.sh                 # e.g. daily from cron: 0 4 * * * /path/to/deploy/backup.sh
#
# Settings come from deploy/.env: BACKUP_DIR (default deploy/backups) and BACKUP_KEEP (14).
#
# Restore (stop the backend first so nothing writes meanwhile):
#   docker compose -f deploy/compose.prod.yaml stop waitlist
#   gunzip -c deploy/backups/waitlist-<stamp>.sql.gz | docker compose -f deploy/compose.prod.yaml \
#     exec -T mysql sh -c 'MYSQL_PWD="$MYSQL_ROOT_PASSWORD" mysql -uroot waitlist'
#   docker compose -f deploy/compose.prod.yaml run --rm --no-deps -T --entrypoint sh waitlist \
#     -c 'tar -C /app -xzf -' < deploy/backups/data-<stamp>.tar.gz
#   docker compose -f deploy/compose.prod.yaml start waitlist
set -eu

here=$(cd "$(dirname "$0")" && pwd)
compose="docker compose -f $here/compose.prod.yaml"

if [ -f "$here/.env" ]; then
	# Only the two backup settings are read here; compose reads the rest itself.
	BACKUP_DIR=${BACKUP_DIR:-$(sed -n 's/^BACKUP_DIR=//p' "$here/.env" | tail -n 1)}
	BACKUP_KEEP=${BACKUP_KEEP:-$(sed -n 's/^BACKUP_KEEP=//p' "$here/.env" | tail -n 1)}
fi
dir=${BACKUP_DIR:-./backups}
keep=${BACKUP_KEEP:-14}
case $dir in /*) ;; *) dir="$here/$dir" ;; esac
mkdir -p "$dir"
stamp=$(date -u +%Y%m%dT%H%M%SZ)

# A consistent dump without locking tables (InnoDB). The password never leaves the container.
$compose exec -T mysql sh -c \
	'MYSQL_PWD="$MYSQL_ROOT_PASSWORD" exec mysqldump -uroot --single-transaction --routines --triggers --events --set-gtid-purged=OFF waitlist' \
	| gzip > "$dir/waitlist-$stamp.sql.gz.tmp"
mv "$dir/waitlist-$stamp.sql.gz.tmp" "$dir/waitlist-$stamp.sql.gz"

# Community data: fits, skills, skill plans, guides, translations (and their edit backups).
$compose exec -T waitlist tar -C /app -czf - data > "$dir/data-$stamp.tar.gz.tmp"
mv "$dir/data-$stamp.tar.gz.tmp" "$dir/data-$stamp.tar.gz"

# Rotation: keep the newest $keep of each kind (names sort by time).
for prefix in waitlist data; do
	ls -1 "$dir" | grep -E "^$prefix-[0-9TZ]+\.(sql|tar)\.gz$" | sort -r | tail -n +"$((keep + 1))" |
		while read -r old; do rm -f "$dir/$old"; done
done

echo "backup written: $dir/waitlist-$stamp.sql.gz, $dir/data-$stamp.tar.gz"
