#!/bin/bash
# Self-seeding multi-suffix slapd entrypoint. On first boot: generate a slapd.conf
# with one `database mdb` per suffix in $DOMAINS, convert it to cn=config, and
# offline-load a small distinct corpus into each suffix. On later boots: just serve.
#
#   DOMAINS   space-separated suffixes, e.g. "dc=alpha,dc=test"
#             or a multi-domain server: "dc=north,dc=example dc=south,dc=example"
#   ADMIN_PW  rootpw for every suffix's cn=admin,<suffix>  (default: secret)
set -euo pipefail

DOMAINS="${DOMAINS:-dc=example,dc=test}"
ADMIN_PW="${ADMIN_PW:-secret}"
CONF=/etc/census/slapd.conf

be_of() { printf '%s' "$1" | sed 's/[^a-zA-Z0-9]/_/g'; }   # suffix → safe dir/name

mkdir -p /var/run/slapd /var/lib/ldap /etc/ldap/slapd.d /etc/census
chown -R openldap:openldap /var/run/slapd /var/lib/ldap /etc/ldap/slapd.d

# 1. Generate cn=config from a slapd.conf built for these suffixes (once).
if [ -z "$(ls -A /etc/ldap/slapd.d 2>/dev/null || true)" ]; then
    echo "[seed] generating slapd.conf for suffixes: $DOMAINS"
    {
        echo "include /etc/ldap/schema/core.schema"
        echo "include /etc/ldap/schema/cosine.schema"
        echo "include /etc/ldap/schema/inetorgperson.schema"
        echo "include /etc/ldap/schema/nis.schema"
        echo "pidfile  /var/run/slapd/slapd.pid"
        echo "argsfile /var/run/slapd/slapd.args"
        echo "modulepath /usr/lib/ldap"
        echo "moduleload back_mdb"
        echo "moduleload sssvlv"
        echo "sizelimit unlimited"
        echo "timelimit unlimited"
        # A cn=config admin so census can manage schema (olcAttributeTypes /
        # olcObjectClasses) over LDAP by binding as cn=admin,cn=config.
        echo "database config"
        echo "rootdn   \"cn=admin,cn=config\""
        echo "rootpw   $ADMIN_PW"
        for suffix in $DOMAINS; do
            dir="/var/lib/ldap/$(be_of "$suffix")"
            mkdir -p "$dir"; chown openldap:openldap "$dir"
            echo "database mdb"
            echo "maxsize  1073741824"
            echo "suffix   \"$suffix\""
            echo "rootdn   \"cn=admin,$suffix\""
            echo "rootpw   $ADMIN_PW"
            echo "directory $dir"
            echo "index objectClass eq"
            echo "index uid eq"
            echo "index uidNumber eq"
            echo "index gidNumber eq"
            echo "index memberUid eq"
            echo "index cn eq,sub"
            echo "index sn eq,sub"
            echo "access to * by * read"
            echo "overlay sssvlv"
            echo "sssvlv-max 100"
            echo "sssvlv-maxkeys 5"
        done
    } > "$CONF"
    slaptest -f "$CONF" -F /etc/ldap/slapd.d 2>/dev/null || true
    if [ -z "$(ls -A /etc/ldap/slapd.d 2>/dev/null || true)" ]; then
        echo "[seed] ERROR: cn=config generation produced nothing." >&2
        exit 1
    fi
    chown -R openldap:openldap /etc/ldap/slapd.d
fi

# 2. Offline-seed each suffix whose backend is empty.
uidbase=10000
for suffix in $DOMAINS; do
    dir="/var/lib/ldap/$(be_of "$suffix")"
    if [ ! -f "$dir/data.mdb" ]; then
        echo "[seed] loading $suffix"
        /usr/local/bin/mkseed.sh "$suffix" "$uidbase" > "/tmp/seed.ldif"
        slapadd -q -F /etc/ldap/slapd.d -b "$suffix" -l "/tmp/seed.ldif"
    fi
    uidbase=$((uidbase + 1000))
done
chown -R openldap:openldap /var/lib/ldap

echo "[serve] starting slapd on ldap:///"
exec slapd -d 0 -h "ldap:/// ldapi:///" -u openldap -g openldap -F /etc/ldap/slapd.d
