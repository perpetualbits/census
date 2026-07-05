#!/bin/bash
# Self-seeding wrapper around the stock 389-DS container runtime. Runs
# `dscontainer -r` in the background, waits for the instance to accept binds, then
# (once) creates each suffix in $DOMAINS with `dsconf … --create-suffix` and loads a
# small distinct corpus into it. On later boots the marker file skips re-seeding.
#
#   DOMAINS           space-separated suffixes (one instance can host several)
#   DS_DM_PASSWORD    Directory Manager password (used by dscontainer + our seeding)
set -uo pipefail

DOMAINS="${DOMAINS:-dc=example,dc=test}"
DM_PW="${DS_DM_PASSWORD:-secret389}"
MARKER=/data/.census-seeded

be_of() { printf '%s' "$1" | sed 's/[^a-zA-Z0-9]//g'; }

# Start the real 389-DS runtime; forward termination so the container stops cleanly.
/usr/lib/dirsrv/dscontainer -r &
PID=$!
trap 'kill -TERM "$PID" 2>/dev/null || true' TERM INT

# Wait until the instance is FULLY up — an authenticated read of cn=config only
# succeeds once dscontainer has finished setting the Directory Manager password
# (the plain LDAP port opens well before that; seeding then must not run yet).
ready=0
for _ in $(seq 1 180); do
    if ldapsearch -x -H ldap://localhost:3389 -D "cn=Directory Manager" -w "$DM_PW" \
         -b "cn=config" -s base dn >/dev/null 2>&1; then ready=1; break; fi
    sleep 1
done
sleep 2   # small grace for cn=config to settle after first bind

if [ "$ready" = 1 ] && [ ! -f "$MARKER" ]; then
    uidbase=10000
    for suffix in $DOMAINS; do
        echo "[seed] creating + loading $suffix"
        # Backend creation can lag just after init — retry until the suffix appears
        # as a naming context.
        for _ in $(seq 1 15); do
            dsconf localhost backend create --suffix "$suffix" --be-name "$(be_of "$suffix")" \
                --create-suffix >/dev/null 2>&1 || true
            if ldapsearch -x -H ldap://localhost:3389 -b "" -s base namingContexts 2>/dev/null \
                 | grep -qiF "namingContexts: $suffix"; then break; fi
            sleep 2
        done
        NOAPEX=1 /usr/local/bin/mkseed.sh "$suffix" "$uidbase" \
            | ldapadd -x -H ldap://localhost:3389 -D "cn=Directory Manager" -w "$DM_PW" -c \
            >/dev/null 2>&1 || true
        uidbase=$((uidbase + 1000))
    done
    touch "$MARKER"
    echo "[seed] ready."
fi

wait "$PID"
