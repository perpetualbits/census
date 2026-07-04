#!/bin/bash
# Self-seeding slapd entrypoint.
#
# On first boot (empty data volume): build cn=config from the baked slapd.conf,
# offline-load /seed/data.ldif with `slapadd -q`, start slapd, then apply the
# online SSH modifications (/seed/ssh.ldif). On later boots: just serve.
#
# It waits for the generator to finish writing /seed (marker file /seed/.done),
# so it works whether or not your compose engine honours
# `depends_on: condition: service_completed_successfully`.
set -euo pipefail

BASE_DN="${BASE_DN:-dc=census,dc=test}"
ADMIN_PW="${ADMIN_PW:-census}"
CONF="${SLAPD_CONF:-/etc/census/slapd.conf}"
SEED="${SEED_DIR:-/seed}"
SEED_WAIT="${SEED_WAIT:-3600}"

mkdir -p /var/run/slapd /var/lib/ldap /etc/ldap/slapd.d
chown -R openldap:openldap /var/run/slapd /var/lib/ldap /etc/ldap/slapd.d

# 1. cn=config from slapd.conf (slaptest writes it, then exits non-zero trying to
#    open the empty db — the config is already written, so tolerate that).
if [ -z "$(ls -A /etc/ldap/slapd.d 2>/dev/null || true)" ]; then
    echo "[seed] building cn=config from $CONF ..."
    slaptest -f "$CONF" -F /etc/ldap/slapd.d 2>/dev/null || true
    if [ -z "$(ls -A /etc/ldap/slapd.d 2>/dev/null || true)" ]; then
        echo "[seed] ERROR: cn=config generation produced nothing." >&2
        exit 1
    fi
    chown -R openldap:openldap /etc/ldap/slapd.d
fi

# 2. Offline bulk load (only when the db is empty).
NEED_SSH=0
if [ ! -f /var/lib/ldap/data.mdb ]; then
    if [ ! -f "$SEED/data.ldif" ] && [ ! -f "$SEED/.done" ]; then
        echo "[seed] waiting up to ${SEED_WAIT}s for $SEED/data.ldif (generator) ..."
        for _ in $(seq 1 "$SEED_WAIT"); do
            [ -f "$SEED/.done" ] || [ -f "$SEED/data.ldif" ] && break
            sleep 1
        done
    fi
    if [ -f "$SEED/data.ldif" ]; then
        echo "[seed] slapadd -q loading $SEED/data.ldif ($(du -h "$SEED/data.ldif" | cut -f1)) ..."
        # -q = quick bulk mode; builds the sortRank index sequentially as it goes.
        slapadd -q -F /etc/ldap/slapd.d -b "$BASE_DN" -l "$SEED/data.ldif"
        chown -R openldap:openldap /var/lib/ldap
        NEED_SSH=1
        echo "[seed] bulk load complete."
    else
        echo "[seed] WARNING: no $SEED/data.ldif found; starting with an empty tree." >&2
    fi
fi

# 3. Serve; forward SIGTERM so the container stops cleanly.
echo "[serve] starting slapd on ldap:///"
slapd -d 0 -h "ldap:/// ldapi:///" -u openldap -g openldap -F /etc/ldap/slapd.d &
PID=$!
trap 'kill -TERM "$PID" 2>/dev/null || true' TERM INT

# 4. Apply the online SSH modifications once, after the socket is up.
if [ "$NEED_SSH" = 1 ] && [ -f "$SEED/ssh.ldif" ]; then
    for _ in $(seq 1 120); do
        ldapsearch -x -H ldapi:/// -b "" -s base >/dev/null 2>&1 && break
        sleep 0.5
    done
    echo "[seed] applying $SEED/ssh.ldif (ldapPublicKey + sshPublicKey) ..."
    ldapmodify -x -H ldapi:/// -D "cn=admin,$BASE_DN" -w "$ADMIN_PW" -c -f "$SEED/ssh.ldif" \
        >/dev/null 2>&1 || echo "[seed] WARNING: ssh.ldif apply reported errors" >&2
    echo "[seed] ready."
fi

wait "$PID"
