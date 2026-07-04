# census OpenLDAP test directory

A disposable, self-seeding OpenLDAP (slapd) directory for exercising census
against a large, plausible user base — millions of `posixAccount`/`inetOrgPerson`
users, thousands of `posixGroup` groups, and a few hundred users with real SSH
keys gathered into `cn=ssh-users`. It also ships a precomputed `sortRank` +
VLV index so an **ordered million-entry browse is fast**.

**The full story — architecture, the corpus, why the fast browse works, and every
gotcha — is in [`../../docs/openldap-testbed.md`](../../docs/openldap-testbed.md).**
This directory is just the runnable bits.

## Three ways to run it

### 1. Script (podman or docker)
```bash
./setup.sh up                 # quick 100k-user corpus on ldap://localhost:3389
./setup.sh --full up          # the full 2,000,000-user corpus
./setup.sh status             # counts
./setup.sh logs               # watch it seed
./setup.sh destroy            # remove everything
```

### 2. Compose (podman-compose / docker compose)
```bash
cp .env.example .env          # edit USERS/GROUPS/SSH_USERS/PORT
podman compose up -d          # or: docker compose up -d
podman compose logs -f ldap
podman compose down -v        # -v also drops the data volumes
```

### 3. Pod (`podman kube play`, also a k8s starting point)
```bash
podman build -t localhost/census-ldap:latest -f Containerfile .
podman build -t localhost/census-ldap-generator:latest -f Containerfile.generator .
podman kube play podman-kube.yaml
podman kube down podman-kube.yaml
```

## Point census at it
```bash
cp census-test.toml ~/.config/census/census-test.toml
census --config ~/.config/census/census-test.toml --ping
```

## Files
| file | what |
|---|---|
| `gen.py` | corpus generator (Faker names, real ssh-keygen keys, name-ordered LDIF) |
| `slapd.conf` | classic config → cn=config: indexes, `sssvlv` overlay, `index sortRank eq` |
| `schema/` | `sshPublicKey`/`ldapPublicKey` and `sortKey`/`sortRank` schema |
| `entrypoint.sh` | self-seeding: offline `slapadd -q`, then online SSH keys, then serve |
| `Containerfile` / `Containerfile.generator` | slapd server / generator images |
| `setup.sh` | engine-agnostic orchestrator |
| `compose.yaml` / `podman-kube.yaml` | compose and pod manifests |
| `census-test.toml` | matching census config |

Admin bind: `cn=admin,dc=census,dc=test` / `census`. Anonymous read is allowed.
