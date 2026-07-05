#!/usr/bin/env bash
# Emit a small, self-contained LDIF for ONE suffix: the domain apex (unless
# NOAPEX=1), ou=users, ou=groups, three posixAccount users and one posixGroup.
# Names are derived from the suffix's leading label (e.g. dc=alpha,dc=test → alpha)
# so every domain is visually distinct in census.
#
#   mkseed.sh dc=alpha,dc=test            # full tree
#   NOAPEX=1 mkseed.sh dc=gamma,dc=test   # skip the apex (389-DS creates it itself)
set -euo pipefail
SUFFIX="${1:?usage: mkseed.sh <suffix> [base_uid]}"
BASEUID="${2:-10000}"
LABEL="$(printf '%s' "$SUFFIX" | sed 's/^[a-zA-Z]*=//; s/,.*//')"   # first RDN value

cap() { printf '%s' "$1" | sed 's/^./\U&/'; }   # capitalise first letter

if [ "${NOAPEX:-0}" != "1" ]; then
cat <<EOF
dn: $SUFFIX
objectClass: top
objectClass: domain
dc: $LABEL

EOF
fi

cat <<EOF
dn: ou=users,$SUFFIX
objectClass: organizationalUnit
ou: users

dn: ou=groups,$SUFFIX
objectClass: organizationalUnit
ou: groups
EOF

i=1
for name in ada bela chen; do
    uidn=$((BASEUID + i))
    cat <<EOF

dn: uid=$name.$LABEL,ou=users,$SUFFIX
objectClass: inetOrgPerson
objectClass: posixAccount
uid: $name.$LABEL
cn: $(cap "$name") $(cap "$LABEL")
sn: $(cap "$LABEL")
givenName: $(cap "$name")
uidNumber: $uidn
gidNumber: $BASEUID
homeDirectory: /home/$name.$LABEL
loginShell: /bin/bash
EOF
    i=$((i + 1))
done

cat <<EOF

dn: cn=$LABEL-team,ou=groups,$SUFFIX
objectClass: posixGroup
cn: $LABEL-team
gidNumber: $BASEUID
memberUid: ada.$LABEL
memberUid: bela.$LABEL
memberUid: chen.$LABEL
EOF
