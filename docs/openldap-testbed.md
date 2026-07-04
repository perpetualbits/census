# Testing census against a huge OpenLDAP directory

This explains how to stand up a local **OpenLDAP** directory seeded with a
large, realistic corpus — millions of users, thousands of groups, hundreds of
SSH-key users — so census can be exercised at scale, and how it's tuned so an
**ordered million-entry browse is fast**.

The runnable artifacts live in [`../contrib/ldap-testbed/`](../contrib/ldap-testbed/):
a generator, container images, a `setup.sh` script, a Compose file, and a
`podman kube play` manifest. This document is the reasoning behind them.

The reference build used **2,000,000 users**, **3,000 groups**, and **300 SSH
users**, but every scale is a parameter.

---

## 1. What the directory contains

RFC 2307 / POSIX schema — a sane default that census speaks out of the box:

```
dc=census,dc=test
├── ou=users     posixAccount + inetOrgPerson + shadowAccount + sortable
└── ou=groups    posixGroup (memberUid) + domain-users + ssh-users
```

- **Users** — `uid` = `first.last[.n]`, `uidNumber` from 100000, primary
  `gidNumber` 10000, `cn`/`sn`/`givenName`/`mail`/home/shell. Plausible
  multi-locale names from Faker pools (US/GB/DE/FR/ES/NL/IT/PL/SE/BR); accented
  names are base64-encoded per RFC 2849.
- **Groups** — names synthesized from department/team/region/project word lists
  (`engineering-platform-emea`, `project-aurora`, `sales-benelux`, …). Every
  user is a `memberUid` of **1–50** random groups (~51M memberships at full
  scale; groups average ~17k members).
- **SSH users** — a few hundred users get 1–3 real ed25519 `sshPublicKey`s
  (`ldapPublicKey` class) and are all collected into **`cn=ssh-users`**, so
  they're trivial to find: `(memberUid=*)` on that group, or `(sshPublicKey=*)`.
- **Fast ordered browse** — every user carries a precomputed integer
  **`sortRank`** (see §4) plus a human-readable `sortKey`, on a custom
  `sortable` auxiliary class.

Connection: `ldap://localhost:3389`, base `dc=census,dc=test`, admin
`cn=admin,dc=census,dc=test` / `census`, anonymous read allowed. A matching
census config is in `contrib/ldap-testbed/census-test.toml`.

---

## 2. How it's built (and why that way)

Loading millions of entries over the wire with `ldapadd` would take days. The
only sane path is an **offline bulk load** with `slapadd -q` (quick mode, no
per-entry consistency checks) directly into the mdb backend while slapd is
stopped — a few minutes for 2M entries.

The pipeline (see `contrib/ldap-testbed/entrypoint.sh`):

1. **Generate** `data.ldif` + `ssh.ldif` with `gen.py`.
2. **Build cn=config** from a classic `slapd.conf` via `slaptest -f … -F …`.
3. **`slapadd -q`** the bulk `data.ldif` offline (this also builds the indexes,
   sequentially).
4. **Start slapd**, then **apply `ssh.ldif` online** with `ldapmodify`.

Why SSH keys are applied in a **second, online** phase rather than in the bulk
file: `slapadd -q` rejects entries carrying the `ldapPublicKey` auxiliary class
("no structural object class provided"). It's only a few hundred users, so an
online `ldapmodify` after startup is instant. Group membership (`memberUid`)
stays in the bulk load either way.

---

## 3. Ordered browse: SSS + VLV

The **`sssvlv`** overlay is enabled, giving clients Server-Side Sorting
(RFC 2891) and Virtual List View. VLV lets census jump to "user N of 2,000,000
in sorted order" and page from there without pulling the whole set:

```bash
# window of 5 users at name-order offset 1,000,000 of 2,000,000
ldapsearch -xLLL -o ldif-wrap=no -z 5 -H ldap://localhost:3389 \
  -D cn=admin,dc=census,dc=test -w census -b ou=users,dc=census,dc=test \
  -E 'sss=sortRank' -E 'vlv=0/4/1000000/2000000' '(objectClass=posixAccount)' sn givenName
#  -> # sortResult: (0) Success
#     # vlvResult: pos=1000000 count=2000000 (0) Success
```

census pays the sort **once** per browse (the server returns a VLV *context*);
every subsequent scroll reuses it and is instant.

> `ldapsearch` itself will keep auto-issuing VLV requests and appear to "hang" —
> that's the CLI paging the whole list, not the server. The single-sort cost is
> what matters (measure it with `sss=…` + `-z 1`).

---

## 4. Why the browse is fast — the non-obvious part

**OpenLDAP has no persistent VLV/browse index.** (389 Directory Server does;
OpenLDAP does not.) `sssvlv` re-gathers and re-sorts the candidate set on the
first request every time. So "fast" means driving that one in-memory sort down
to the bare cost of reading 2M entries (~4.5 s). Getting there took **three**
things, each of which independently mattered — measured on the 2M corpus:

| sort key | first-sort of 2M | why |
|---|---|---|
| `uid` / `cn` / `sn` (string) | ~8 s | copying 2M string values dominates |
| `sortRank`, unindexed, arbitrary load order | ~11 s | random entry fetches |
| `sortRank`, indexed, arbitrary load order | ~9 s | still random fetches |
| **`sortRank`, indexed, name-ordered load** | **~4.5 s** | sequential reads |

1. **Integer key, not string.** Sorting 2M *strings* costs ~8 s regardless of
   the matching rule — the cost is copying the values, not comparing them. So
   each user gets an integer `sortRank` (its position in global
   surname→given order). (A string `sortKey` is also stored, for readability and
   as a fallback, but it's the slow path.)

2. **An `eq` index on the sort attribute** — `index sortRank eq`, built
   **during** `slapadd -q` (sequential, cheap). Do **not** add it afterward with
   `slapindex`: LMDB's random-write reindex thrashes on ZFS (the process sits in
   uninterruptible I/O wait — 40 minutes and climbing). If you change indexes,
   do a fresh bulk load so `slapadd` builds them sequentially.

3. **Physical entry order == sort order.** `gen.py` writes users pre-sorted by
   name, so `sortRank`'s index order matches on-disk order and the sort becomes
   a **sequential scan**. Loading in any other order makes the same query ~2×
   slower (random fetches, which ZFS punishes). This is exactly why plain
   `uidNumber` was already fast — its order happened to match load order.

Point census at it with (this is in `census-test.toml`):

```toml
[browse]
sort_attr     = "sortRank"
sort_ordering = "2.5.13.15"   # integerOrderingMatch — matches the built index
```

> Want genuinely **sub-second** first-browse of millions? That needs a real
> persistent VLV index, which means **389 Directory Server**, not OpenLDAP. The
> `sortRank` approach gets OpenLDAP to its ~4.5 s floor; 389-ds would read a
> prebuilt sorted list.

---

## 5. Gotchas worth knowing

- **`slaptest -u` is dry-run** — it validates but writes nothing. Use `slaptest
  -f slapd.conf -F slapd.d` (no `-u`) to actually convert; it then exits
  non-zero trying to open the empty db, which is expected — the config is
  already written.
- **Classic `slapd.conf` has no inline comments.** `sssvlv-max 100 # ...` fails
  with "extra cruft after <num>". Comments go on their own lines.
- **String sorts need an ordering rule named in the control** for `uid`/`cn`/`sn`
  (`-E 'sss=cn:caseIgnoreOrderingMatch'`) — those attributes have no default
  ORDERING rule. `sortRank`/`uidNumber` carry one natively.
- **`sizelimit`/`timelimit unlimited`** in `slapd.conf`, or non-admin searches
  cap at 500 entries.
- **LMDB `maxsize`** must be set large up front (64 GiB here) — it's the mmap
  ceiling for the whole database.

---

## 6. Running it

All three methods live in `contrib/ldap-testbed/` and self-seed on first boot.

```bash
cd contrib/ldap-testbed

# A) script (podman preferred, docker fallback)
./setup.sh up            # quick 100k corpus;  ./setup.sh --full up  for 2,000,000
./setup.sh status
./setup.sh destroy

# B) compose
cp .env.example .env     # set USERS/GROUPS/SSH_USERS/PORT
podman compose up -d     # or docker compose up -d

# C) podman kube play (build images first — see contrib README)
podman kube play podman-kube.yaml
```

Then:

```bash
cp census-test.toml ~/.config/census/census-test.toml
census --config ~/.config/census/census-test.toml --ping
census --config ~/.config/census/census-test.toml group members ssh-users
```

Scale is a parameter (`USERS`/`GROUPS`/`SSH_USERS`). The full 2M corpus needs a
few GB of disk and RAM and a few minutes to generate + load; the 100k default is
seconds.

---

## 7. Kubernetes / Helm

`podman-kube.yaml` is a `podman kube play` manifest and a starting point for real
Kubernetes. To run it on a cluster:

- Push the two images to a registry the cluster can pull, and replace the
  `localhost/…` image names.
- Promote the bare **Pod** to a **StatefulSet** (stable identity + the mdb
  `data` PVC as a `volumeClaimTemplate`), and add a **Service** for `:389`.
- Keep the **generator as an initContainer** (or a one-shot Job that populates
  the seed volume) so slapd self-seeds exactly as it does locally.
- Size the PVCs to your corpus; note **LMDB + networked/`mmap`-unfriendly
  storage** can be slow (the same random-I/O caveat as ZFS in §4) — prefer local
  or block storage for the `data` volume.

A **Helm chart** would template the knobs that already exist here — `USERS` /
`GROUPS` / `SSH_USERS`, image refs, `PORT`/Service type, PVC sizes and storage
class, and the `dc=census,dc=test` suffix / admin password — over these same
manifests. That's the natural next step if you want to parameterize deployments,
but it isn't required to run the testbed.
