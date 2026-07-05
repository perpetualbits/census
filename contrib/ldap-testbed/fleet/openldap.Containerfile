# A small, self-seeding OpenLDAP (slapd) fleet server that hosts ONE OR MORE
# suffixes (domains), set via the DOMAINS env var. Unlike the scale server in the
# parent directory (a single hard-wired suffix + 2M corpus), this one generates its
# slapd.conf at first boot from DOMAINS — one `database mdb` per suffix, each with
# the sssvlv overlay — and seeds a tiny distinct corpus into each.
#
#   docker build -t census-fleet-openldap -f openldap.Containerfile .
FROM debian:trixie-slim

RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      slapd ldap-utils \
 && rm -rf /var/lib/apt/lists/* \
 && rm -rf /etc/ldap/slapd.d/* /var/lib/ldap/*

COPY openldap-entrypoint.sh /usr/local/bin/entrypoint.sh
COPY mkseed.sh /usr/local/bin/mkseed.sh
RUN chmod +x /usr/local/bin/entrypoint.sh /usr/local/bin/mkseed.sh

EXPOSE 389
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
