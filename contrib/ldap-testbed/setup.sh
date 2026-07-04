#!/usr/bin/env bash
# Stand up the census OpenLDAP test directory exactly as documented in
# docs/openldap-testbed.md — engine-agnostic (podman preferred, docker fallback).
#
#   ./setup.sh up          build images, generate the corpus, run the server
#   ./setup.sh --full up    ... at the full 2,000,000-user scale we built
#   ./setup.sh status      container state + live entry counts
#   ./setup.sh logs        follow the slapd / seeding logs
#   ./setup.sh down        stop & remove the container (keep data)
#   ./setup.sh destroy     remove container + volumes (full teardown)
#
# Scale / engine via env (or flags): USERS, GROUPS, SSH_USERS, PORT, CENSUS_ENGINE
set -euo pipefail
cd "$(dirname "$0")"

ENGINE="${CENSUS_ENGINE:-$(command -v podman >/dev/null 2>&1 && echo podman || echo docker)}"
USERS="${USERS:-100000}"
GROUPS="${GROUPS:-1000}"
SSH_USERS="${SSH_USERS:-200}"
PORT="${PORT:-3389}"
BASE_DN="dc=census,dc=test"
ADMIN_DN="cn=admin,${BASE_DN}"
ADMIN_PW="census"

NAME=census-ldap
IMG_SRV=census-ldap:latest
IMG_GEN=census-ldap-generator:latest
V_SEED=census-seed
V_CONF=census-conf
V_DATA=census-data

log() { printf '\033[1;36m[setup]\033[0m %s\n' "$*"; }

# Consume a leading --full before the subcommand.
if [ "${1:-}" = "--full" ]; then
    USERS=2000000; GROUPS=3000; SSH_USERS=300; shift
fi
CMD="${1:-up}"

build() {
    log "engine: $ENGINE — building images"
    $ENGINE build -t "$IMG_SRV" -f Containerfile .
    $ENGINE build -t "$IMG_GEN" -f Containerfile.generator .
}

ensure_volumes() {
    for v in "$V_SEED" "$V_CONF" "$V_DATA"; do
        $ENGINE volume inspect "$v" >/dev/null 2>&1 || $ENGINE volume create "$v" >/dev/null
    done
}

generate() {
    log "generating corpus: ${USERS} users / ${GROUPS} groups / ${SSH_USERS} ssh users"
    $ENGINE run --rm -v "$V_SEED:/seed" "$IMG_GEN" \
        --users "$USERS" --groups "$GROUPS" --ssh-users "$SSH_USERS" \
        --out /seed/data.ldif --ssh-out /seed/ssh.ldif
}

serve() {
    $ENGINE rm -f "$NAME" >/dev/null 2>&1 || true
    log "starting slapd (self-seeds on first boot) on ldap://localhost:${PORT}"
    $ENGINE run -d --name "$NAME" -p "${PORT}:389" \
        -e "BASE_DN=${BASE_DN}" -e "ADMIN_PW=${ADMIN_PW}" \
        -v "$V_SEED:/seed" -v "$V_CONF:/etc/ldap/slapd.d" -v "$V_DATA:/var/lib/ldap" \
        "$IMG_SRV" >/dev/null
}

case "$CMD" in
  up)
    build
    ensure_volumes
    # (re)generate only if the seed volume has no finished corpus yet.
    # (Override the entrypoint — the server image's entrypoint ignores CMD.)
    if $ENGINE run --rm --entrypoint sh -v "$V_SEED:/seed" "$IMG_SRV" -c '[ -f /seed/.done ]' 2>/dev/null; then
        log "corpus already present in $V_SEED — skipping generation"
    else
        generate
    fi
    serve
    log "done. Bind: $ADMIN_DN / $ADMIN_PW"
    log "watch it seed:  $ENGINE logs -f $NAME"
    log "point census:   census --config <cfg> --ping   (see docs/openldap-testbed.md)"
    ;;
  build)     build ;;
  generate)  build; ensure_volumes; generate ;;
  serve)     serve ;;
  status)
    $ENGINE ps --filter "name=$NAME" --format 'container: {{.Names}} {{.Status}} {{.Ports}}' || true
    exec_ldap() { $ENGINE exec "$NAME" ldapsearch -x -H ldapi:/// -D "$ADMIN_DN" -w "$ADMIN_PW" "$@" 2>/dev/null; }
    printf 'users:      %s\n' "$(exec_ldap -b "ou=users,$BASE_DN"  '(objectClass=posixAccount)' 1.1 | grep -c '^dn:')"
    printf 'groups:     %s\n' "$(exec_ldap -b "ou=groups,$BASE_DN" '(objectClass=posixGroup)'  1.1 | grep -c '^dn:')"
    printf 'ssh users:  %s\n' "$(exec_ldap -b "ou=users,$BASE_DN"  '(sshPublicKey=*)'          1.1 | grep -c '^dn:')"
    ;;
  logs)      $ENGINE logs -f "$NAME" ;;
  down)      $ENGINE rm -f "$NAME" >/dev/null 2>&1 || true; log "stopped (volumes kept)" ;;
  destroy)
    $ENGINE rm -f "$NAME" >/dev/null 2>&1 || true
    $ENGINE volume rm "$V_SEED" "$V_CONF" "$V_DATA" >/dev/null 2>&1 || true
    log "container + volumes removed"
    ;;
  *)
    grep -E '^#( |$)' "$0" | sed 's/^# \{0,1\}//'
    ;;
esac
