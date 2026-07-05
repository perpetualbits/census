# A self-seeding 389 Directory Server fleet image: wraps the stock dscontainer
# runtime so the instance creates + seeds one OR MORE suffixes from the DOMAINS env
# var on first boot — symmetric with the OpenLDAP fleet image, so both brands are
# plain declarative services (compose / kube) keyed on DOMAINS.
#
#   docker build -t census-fleet-ds389 -f ds389.Containerfile .
FROM docker.io/389ds/dirsrv:latest

COPY ds389-entrypoint.sh /usr/local/bin/ds389-entrypoint.sh
COPY mkseed.sh /usr/local/bin/mkseed.sh
RUN chmod +x /usr/local/bin/ds389-entrypoint.sh /usr/local/bin/mkseed.sh

ENTRYPOINT ["/usr/local/bin/ds389-entrypoint.sh"]
