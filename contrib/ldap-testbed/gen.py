#!/usr/bin/env python3
"""Generate an RFC 2307 / POSIX LDIF corpus for slapadd.

Streams user entries to disk, accumulates group memberships in compact
integer arrays, then writes group entries. Also generates real SSH keys for
a few hundred users and gathers them into cn=ssh-users for easy discovery.
"""
import argparse, base64, os, random, re, subprocess, sys, tempfile, unicodedata
from array import array
from faker import Faker


def line(attr: str, val: str) -> str:
    """One LDIF attribute line, base64-encoding values that aren't LDIF-safe."""
    if val and ord(max(val)) < 128 and val[0] not in " :<" and "\n" not in val:
        return f"{attr}: {val}"
    b = base64.b64encode(val.encode("utf-8")).decode("ascii")
    return f"{attr}:: {b}"

BASE_DN = "dc=census,dc=test"
UID_BASE = 100000          # first uidNumber
GID_PRIMARY = 10000        # shared primary group ("domain-users")
GID_GROUP_BASE = 20000     # gidNumber base for the membership groups
GID_SSH = 19999            # gidNumber for cn=ssh-users

DEPARTMENTS = [
    "engineering", "sales", "marketing", "finance", "hr", "legal", "operations",
    "research", "support", "security", "design", "product", "data", "infra",
    "qa", "it", "facilities", "procurement", "compliance", "logistics",
    "analytics", "platform", "mobile", "network", "devops", "payroll",
    "recruiting", "training", "audit", "communications",
]
TEAMS = [
    "core", "backend", "frontend", "platform", "ops", "tooling", "growth",
    "billing", "identity", "search", "storage", "runtime", "release",
    "reliability", "onboarding", "insights", "gateway", "mesh", "edge", "labs",
]
REGIONS = [
    "emea", "apac", "amer", "benelux", "nordics", "dach", "iberia", "latam",
    "mena", "anz", "global", "uk", "us-east", "us-west", "eu-central",
]
PROJECTS = [
    "aurora", "borealis", "cascade", "delta", "everest", "falcon", "garnet",
    "helios", "indigo", "jupiter", "kraken", "lumen", "meridian", "nimbus",
    "onyx", "pegasus", "quartz", "raven", "sierra", "titan", "umbra", "vertex",
    "willow", "xenon", "yonder", "zephyr", "atlas", "cobalt", "ember", "flux",
]


def slug(s: str) -> str:
    s = unicodedata.normalize("NFKD", s).encode("ascii", "ignore").decode()
    return re.sub(r"[^a-z0-9]+", "", s.lower())


def build_name_pools(fake, draws=200_000):
    """Sample a bounded number of names and keep the uniques (the combined
    locale pool tops out at a few thousand distinct names)."""
    first = {fake.first_name() for _ in range(draws)}
    last = {fake.last_name() for _ in range(draws)}
    return sorted(first), sorted(last)


def build_group_names(rng, n):
    """Synthesize n unique, plausible group names."""
    names, seen = [], set()

    def add(name):
        base, k = name, name
        while k in seen:
            k = f"{base}-{rng.randint(2, 999)}"
        seen.add(k)
        names.append(k)

    # Structured combos first (department[-team][-region] / project-*).
    combos = []
    for d in DEPARTMENTS:
        combos.append(d)
        for t in TEAMS:
            combos.append(f"{d}-{t}")
        for r in REGIONS:
            combos.append(f"{d}-{r}")
    for p in PROJECTS:
        combos.append(f"project-{p}")
        for t in TEAMS:
            combos.append(f"{p}-{t}")
        for r in REGIONS:
            combos.append(f"{p}-{r}")
    rng.shuffle(combos)
    for c in combos:
        if len(names) >= n:
            break
        add(c)
    # Pad with dept-team-region triples if we still need more.
    while len(names) < n:
        add(f"{rng.choice(DEPARTMENTS)}-{rng.choice(TEAMS)}-{rng.choice(REGIONS)}")
    return names[:n]


def gen_ssh_keys(count, workdir):
    """Generate `count` real ed25519 public keys, return list of pubkey strings."""
    keys = []
    for i in range(count):
        kp = os.path.join(workdir, f"k{i}")
        subprocess.run(
            ["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", f"testkey{i}", "-f", kp],
            check=True,
        )
        with open(kp + ".pub") as fh:
            keys.append(fh.read().strip())
    return keys


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--users", type=int, default=2_000_000)
    ap.add_argument("--groups", type=int, default=3000)
    ap.add_argument("--ssh-users", type=int, default=300)
    ap.add_argument("--max-groups-per-user", type=int, default=50)
    ap.add_argument("--out", default="seed/data.ldif")
    ap.add_argument("--ssh-out", default="seed/ssh.ldif")
    ap.add_argument("--seed", type=int, default=1234)
    args = ap.parse_args()

    rng = random.Random(args.seed)
    Faker.seed(args.seed)
    fake = Faker(["en_US", "en_GB", "de_DE", "fr_FR", "es_ES", "nl_NL",
                  "it_IT", "pl_PL", "sv_SE", "pt_BR"])

    print(f"[gen] building name pools ...", file=sys.stderr)
    firsts, lasts = build_name_pools(fake)
    print(f"[gen] {len(firsts)} first names, {len(lasts)} last names", file=sys.stderr)

    print(f"[gen] building {args.groups} group names ...", file=sys.stderr)
    group_names = build_group_names(rng, args.groups)

    # Pre-select SSH users and generate their keys up front.
    print(f"[gen] generating SSH keys for {args.ssh_users} users ...", file=sys.stderr)
    ssh_idx = set(rng.sample(range(args.users), args.ssh_users))
    with tempfile.TemporaryDirectory() as tmp:
        rng2 = random.Random(args.seed + 1)
        key_counts = {i: rng2.randint(1, 3) for i in sorted(ssh_idx)}
        total_keys = sum(key_counts.values())
        keypool = gen_ssh_keys(total_keys, tmp)
    # assign slices of the keypool to each ssh user
    ssh_keys = {}
    cur = 0
    for i in sorted(ssh_idx):
        c = key_counts[i]
        ssh_keys[i] = keypool[cur:cur + c]
        cur += c

    os.makedirs(os.path.dirname(args.out) or ".", exist_ok=True)

    # Per-group member lists (indices into the user sequence), compact ints.
    group_members = [array("I") for _ in range(args.groups)]
    ssh_group_members = array("I")
    uids = []  # uid string per user index (needed to render memberUid)
    uid_seen = {}

    maxg = args.max_groups_per_user
    ngroups = args.groups

    # Pass 1: generate per-user data (names, uids, sort keys, memberships).
    # We can't stream yet: sortRank needs every user's sort key first.
    print(f"[gen] pass 1/2: generating {args.users} users ...", file=sys.stderr)
    firsts_l = []
    lasts_l = []
    sort_keys = []
    for i in range(args.users):
        first = rng.choice(firsts)
        last = rng.choice(lasts)
        sl_first = slug(first)
        sl_last = slug(last)
        base_uid = f"{sl_first}.{sl_last}" if (sl_first or sl_last) else f"user{i}"
        n = uid_seen.get(base_uid, 0)
        uid_seen[base_uid] = n + 1
        uid = base_uid if n == 0 else f"{base_uid}.{n+1}"
        uids.append(uid)
        firsts_l.append(first)
        lasts_l.append(last)
        # Pre-normalized, ASCII-folded sort key: surname, then given, then uid
        # (the uid tiebreaker makes the order total & stable).
        sort_keys.append(f"{sl_last} {sl_first} {uid}")

        if i in ssh_keys:
            ssh_group_members.append(i)

        # membership: 1..maxg random groups
        k = rng.randint(1, maxg)
        for g in rng.sample(range(ngroups), k):
            group_members[g].append(i)
        if (i + 1) % 400000 == 0:
            print(f"[gen]   {i+1:,} users", file=sys.stderr)

    # Global name order -> integer rank (so census can browse by name FAST via
    # an integer sort instead of an 8s string sort).
    print("[gen] computing global name-order rank ...", file=sys.stderr)
    order = sorted(range(args.users), key=sort_keys.__getitem__)

    # Pass 2: write entries in NAME order, so physical DB layout == sortRank
    # order. sssvlv then reads them sequentially when sorting by sortRank
    # (~4.6s, the gather floor) instead of random-fetching (~9s). Loading in an
    # arbitrary order is what made sortRank slow despite the index.
    print(f"[gen] pass 2/2: writing users (name order) -> {args.out} ...", file=sys.stderr)
    out = open(args.out, "w", buffering=1 << 20)
    out.write(
        f"dn: {BASE_DN}\n"
        "objectClass: top\nobjectClass: dcObject\nobjectClass: organization\n"
        "o: Census Test Org\ndc: census\n\n"
        f"dn: ou=users,{BASE_DN}\n"
        "objectClass: organizationalUnit\nou: users\n\n"
        f"dn: ou=groups,{BASE_DN}\n"
        "objectClass: organizationalUnit\nou: groups\n\n"
    )
    buf = []
    for p in range(args.users):
        i = order[p]                       # i = original index; p = name-order position
        uid = uids[i]
        first = firsts_l[i]
        last = lasts_l[i]
        rec = [
            f"dn: uid={uid},ou=users,{BASE_DN}",
            "objectClass: top",
            "objectClass: posixAccount",
            "objectClass: inetOrgPerson",
            "objectClass: shadowAccount",
            "objectClass: sortable",
            f"uid: {uid}",
            line("cn", f"{first} {last}"),
            line("sn", last),
            line("givenName", first),
            f"uidNumber: {UID_BASE + i}",
            f"gidNumber: {GID_PRIMARY}",
            f"homeDirectory: /home/{uid}",
            "loginShell: /bin/bash",
            f"mail: {uid}@census.test",
            line("sortKey", sort_keys[i]),
            f"sortRank: {p}",              # == name-order position == physical order
        ]
        # SSH keys are added online after load (seed/ssh.ldif); slapadd -q
        # rejects the ldapPublicKey aux class. Group membership stays here.
        buf.append("\n".join(rec))
        if len(buf) >= 2000:
            out.write("\n\n".join(buf))
            out.write("\n\n")
            buf.clear()
        if (p + 1) % 400000 == 0:
            print(f"[gen]   {p+1:,} users", file=sys.stderr)
    if buf:
        out.write("\n\n".join(buf))
        out.write("\n\n")
        buf.clear()

    # primary group everyone belongs to (posixGroup, no memberUid needed for primary)
    out.write(
        f"dn: cn=domain-users,ou=groups,{BASE_DN}\n"
        "objectClass: top\nobjectClass: posixGroup\n"
        f"cn: domain-users\ngidNumber: {GID_PRIMARY}\n\n"
    )

    print(f"[gen] writing {args.groups} groups ...", file=sys.stderr)
    for g, name in enumerate(group_names):
        rec = [
            f"dn: cn={name},ou=groups,{BASE_DN}",
            "objectClass: top",
            "objectClass: posixGroup",
            f"cn: {name}",
            f"gidNumber: {GID_GROUP_BASE + g}",
        ]
        mem = group_members[g]
        if mem:
            rec.append("\n".join(f"memberUid: {uids[j]}" for j in mem))
        out.write("\n".join(rec))
        out.write("\n\n")

    # the special ssh-users group
    rec = [
        f"dn: cn=ssh-users,ou=groups,{BASE_DN}",
        "objectClass: top",
        "objectClass: posixGroup",
        "cn: ssh-users",
        f"gidNumber: {GID_SSH}",
    ]
    if ssh_group_members:
        rec.append("\n".join(f"memberUid: {uids[j]}" for j in ssh_group_members))
    out.write("\n".join(rec))
    out.write("\n\n")
    out.close()

    # Phase 2: online ldapmodify to attach ldapPublicKey + sshPublicKey to the
    # ssh users (applied after slapd is serving).
    print(f"[gen] writing ssh modify -> {args.ssh_out} ...", file=sys.stderr)
    with open(args.ssh_out, "w") as sf:
        for i in sorted(ssh_keys):
            uid = uids[i]
            sf.write(
                f"dn: uid={uid},ou=users,{BASE_DN}\n"
                "changetype: modify\n"
                "add: objectClass\n"
                "objectClass: ldapPublicKey\n"
                "-\n"
                "add: sshPublicKey\n"
            )
            for k in ssh_keys[i]:
                sf.write(f"sshPublicKey: {k}\n")
            sf.write("\n")

    total_mem = sum(len(m) for m in group_members)
    print(f"[gen] DONE: {args.users:,} users, {args.groups:,} groups, "
          f"{total_mem:,} memberships, {len(ssh_group_members)} ssh users",
          file=sys.stderr)


if __name__ == "__main__":
    main()
