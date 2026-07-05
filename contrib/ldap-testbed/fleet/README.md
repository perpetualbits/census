# census multi-brand test fleet

A small fleet of **both** open-source LDAP brands — **OpenLDAP** (slapd) and
**389 Directory Server** — some hosting **several domains (suffixes) in one instance**,
for exercising census's multi-connection features: the connections rail, per-connection
read-only / write / dry-run mode, and (later) migration between directories.

This is the *breadth* counterpart to the parent directory's single 2M-user OpenLDAP
**scale** server (`../`). Each fleet server holds a tiny, distinct corpus (3 users +
1 group per domain, named after the domain — `ada.alpha`, `bela.north`, …) so it's
obvious which directory you're looking at in census.

| container              | brand    | port | domains (suffixes)                          |
|------------------------|----------|------|---------------------------------------------|
| `fleet-openldap-a`     | OpenLDAP | 3390 | `dc=alpha,dc=test`                          |
| `fleet-openldap-b`     | OpenLDAP | 3391 | `dc=bravo,dc=test`                          |
| `fleet-openldap-multi` | OpenLDAP | 3392 | `dc=north,dc=example` + `dc=south,dc=example` |
| `fleet-ds389-a`        | 389-DS   | 3393 | `dc=gamma,dc=test`                          |
| `fleet-ds389-b`        | 389-DS   | 3394 | `dc=delta,dc=test`                          |
| `fleet-ds389-multi`    | 389-DS   | 3395 | `dc=east,dc=example` + `dc=west,dc=example`  |

Binds: OpenLDAP `cn=admin,<suffix>` / `secret`; 389-DS `cn=Directory Manager` /
`secret389` (one Directory Manager per instance covers all its suffixes). Anonymous
read is allowed on the OpenLDAP servers. Both brands advertise **SSS + VLV**, so
census's windowed browse path is exercised on each (389-DS even has a real persistent
VLV index — the sub-second case OpenLDAP can't do; see `../../../docs/openldap-testbed.md`).

## Run it

```bash
./fleet.sh up          # build the OpenLDAP image, start + seed all six servers
./fleet.sh status      # container state + naming contexts
./fleet.sh down        # stop & remove (these servers keep no named volumes)
```

Engine: podman if present, else docker — override with `CENSUS_ENGINE=docker`.
(The images are `docker.io/389ds/dirsrv` and a locally-built `census-fleet-openldap`.)

## Point census at the fleet

```bash
./fleet.sh confd                                  # writes example configs into ./conf.d/
./fleet.sh confd --dest ~/.config/census/conf.d   # install into your live config
census                                            # bare launch → the rail shows them all
```

`census` auto-loads every `~/.config/census/conf.d/*.toml`, so once installed a bare
launch shows the whole fleet in the connections rail (backtick toggles rail focus;
`Enter` focuses a domain, `m` marks, `M` cycles its mode). The generated `conf.d/`
files are also committed here as **reference** for the multi-domain config shape (a
server with a `[[domain]]` block per extra suffix).

> Installing the fleet into your live `conf.d` means a bare `census` connects to all of
> them (plus anything else in `conf.d`, e.g. LOFAR over its tunnel). Use `--config FILE`
> for a single connection, or keep the fleet in a separate config dir you point at with
> `XDG_CONFIG_HOME` when testing.

## How each brand is built

- **OpenLDAP** (`openldap.Containerfile` + `openldap-entrypoint.sh`): a debian-slapd
  image whose entrypoint generates a `slapd.conf` at first boot from the `DOMAINS` env
  var — one `database mdb` per suffix, each with the `sssvlv` overlay — converts it to
  `cn=config` with `slaptest`, and offline-loads a per-suffix seed with `slapadd -q`.
  Multiple suffixes in one slapd = multiple `database` sections, each with its own
  `cn=admin,<suffix>` rootdn.
- **389-DS** (stock `389ds/dirsrv` image): after the instance is up, `fleet.sh` runs
  `dsconf … backend create --create-suffix` for each suffix and seeds it over LDAP.
  Multiple suffixes in one instance = multiple backends under one Directory Manager.

The seed for every domain comes from the shared `mkseed.sh` (`NOAPEX=1` for 389-DS,
whose `--create-suffix` already makes the apex entry).

## Files
| file | what |
|---|---|
| `fleet.sh` | orchestrator: build, up/down/status, `confd` generator |
| `mkseed.sh` | per-suffix seed LDIF (3 users + 1 group, named after the domain) |
| `openldap.Containerfile` / `openldap-entrypoint.sh` | multi-suffix OpenLDAP image |
| `conf.d/` | generated census configs (reference / installable) |

## Not yet here
A declarative `compose.yaml` / `podman kube play` manifest for the fleet (the 389-DS
suffix creation needs a post-start step, so it wants an init container). `fleet.sh` is
the imperative equivalent for now.
