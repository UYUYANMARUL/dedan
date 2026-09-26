#!/bin/sh
# Issues a certificate from Pebble for the customer hostname www.images.com plus a node name (the SAN shape every
# Dedan node uses) with the "shortlived" profile. lego follows the customer's _acme-challenge CNAME into Dedan's zone
# (deploy/resolver/zones/images.com.zone), publishes the TXT through the nameserver, and Pebble validates the same
# chain. Proves the CA + resolver + customer-DNS path end to end. lego 5: every option lives under `run`.
set -eu
/lego run \
  --server https://pebble:14000/dir \
  --email devnet@dedan.test --accept-tos \
  --domains www.images.com --domains istanbul.node.dedan-cdn.test \
  --profile shortlived \
  --dns exec --dns.resolvers resolver:53 --dns.propagation.wait 3s \
  --path /lego-data
ls -l /lego-data/certificates/www.images.com.crt
echo "certificate issued for www.images.com + istanbul.node.dedan-cdn.test (profile shortlived) via DNS-01 through the nameserver"
