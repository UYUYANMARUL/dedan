#!/bin/sh
# A human in a city measures nodes and submits the metrics to the registry contract (one attest batch):
#   sh deploy/scripts/attest.sh <city> [key index|-] [node ...]     default: the city's attester key, every node
# Round trips are pinged and fetch tokens collected from that city's user container; the nodes' hot keys reveal
# this hour's token key first so the tokens verify. An unverified key is rejected by the contract (no ROLE_ATTESTER).
set -eu
cd "$(dirname "$0")/../.."
. deploy/scripts/lib.sh
city=${1:?city}; index=${2:--}; shift; [ $# -eq 0 ] || shift
[ $# -gt 0 ] || set -- $(cities)
[ "$index" != "-" ] || index=$(node_field "$city" attester)
echo "== human in $city (key index $index, account $(address_of "$index")) measures: $*"
# ponytail: reveal on every submit; the identity cache dedups reveals, and real nodes reveal on their own hourly
echo "hour keys revealed by the nodes' hot keys"
VERBOSE=1 ATTESTER_INDEX=$index attest_from "$city" "$@"
echo "the nameserver rebuilds the map from the chain within about 12 s"
