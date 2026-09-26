#!/bin/sh
# Adds a CDN node at a chosen location to the running devnet (demo only):
#   sh deploy/scripts/add-node.sh <label> <lat> <lon>        e.g.  paris 48.8566 2.3522
# Starts a mock node and a user container for that city on the simulated WAN (delays from the great-circle
# distance to every other city), registers the operator, the name <label>.dedan.eth with its records and hot
# key, and the city's attester on the chain through the World ID mock, reveals the hour's token key, and has every
# city's human attest round trips to every node. The nameserver lists the
# name within a sync and steers that city's clients to it. `make devnet-down` removes what this created.
set -eu
cd "$(dirname "$0")/../.."
. deploy/scripts/lib.sh
label=${1:?usage: add-node.sh <label> <lat> <lon>}; lat=${2:?lat}; lon=${3:?lon}
echo "$label" | grep -Eq '^[a-z][a-z0-9-]{1,30}$' || { echo "label: lowercase letters, digits, dashes" >&2; exit 1; }
[ -z "$(host_field "$label" 1)" ] || { echo "$label already exists" >&2; exit 1; }
touch "$DYNAMIC_HOSTS"
# slot n: the static cities own /20s 1-5 (10.77.16-80.0); added cities take 6.. (10.77.96.0/20 onward), their
# keys anvil accounts 30-44 (three per city: operator, hot key, attester); clients added with add-client.sh use 45-59
n=$(( 6 + $(grep -c ' node ' "$DYNAMIC_HOSTS" || true) ))
[ "$n" -le 10 ] || { echo "at most five node cities can be added (keys 30-44; clients use 45-59)" >&2; exit 1; }
k=$((n - 6)); node_ip="10.77.$((16 * n)).1"; user_ip="10.77.$((16 * n)).30"
operator=$((30 + 3 * k)); hot=$((31 + 3 * k)); attester=$((32 + 3 * k))
dir=deploy/devnet/dynamic/$label; mkdir -p "$dir"
cell=$(cell_of "$lat" "$lon"); cells=$(cell_of "$lat" "$lon" 1)   # the node joins its own cell and the ring around it

echo "== $label: node $node_ip, user $user_ip, keys $operator/$hot/$attester, cell $cell (joins 7 cells)"
# WAN tables for the two new containers. Existing containers were set up without a band for this /20, so they
# send toward it undelayed; the new containers carry the whole round trip on their side instead (wan.sh applies
# half of the table entry, hence 2x). Round trip: 5 ms + 20 ms per 1000 km great-circle.
{ hosts; printf '%-18s %-10s %-13s %-7s %s %s\n' "$label" "$label" "$node_ip" node "$lat" "$lon" "user-$label" "$label" "$user_ip" user "$lat" "$lon"; } > "$dir/hosts.txt"
hosts | awk -v me="$label" -v lat="$lat" -v lon="$lon" '$4=="node" {
  pi = atan2(0, -1); dlat = ($5 - lat) * pi / 180; dlon = ($6 - lon) * pi / 180
  a = sin(dlat / 2) ^ 2 + cos(lat * pi / 180) * cos($5 * pi / 180) * sin(dlon / 2) ^ 2
  km = 2 * 6371 * atan2(sqrt(a), sqrt(1 - a)); printf "%s %s %d\n", me, $2, 2 * int(5 + 0.02 * km) }' > "$dir/latency.txt"
docker run -d --name "dedan-devnet-$label-1" --label "dedan.dynamic=$label" --network dedan-devnet_wan --ip "$node_ip" \
  --cap-add NET_ADMIN -e CITY="$label" -e NODE_ID="$label" -e DEDAN_KEY_INDEX="$hot" -e DEDAN_MNEMONIC="$MNEMONIC" \
  -e DEDAN_RPC_URL=http://anvil:8545 -e DEDAN_DEPLOYMENT=/deployments/devnet.json -e DEDAN_ACCOUNTS=/devnet/accounts.json \
  -e DEDAN_RESOLVERS=http://10.77.0.53:8055 -e DEDAN_CP_URL=http://cp:8070 -e DEDAN_ZONE=dedan-cdn.test -e DEDAN_REVEAL_IMMEDIATELY=1 -e DEDAN_DEFAULT_TTL_SECS=60 -e RUST_LOG=info \
  -v "$PWD/contracts/deployments:/deployments:ro" -v "$PWD/deploy/devnet:/devnet:ro" -v "$PWD/deploy/wan/wan.sh:/wan/wan.sh:ro" \
  -v "$PWD/$dir/hosts.txt:/wan/hosts.txt:ro" -v "$PWD/$dir/latency.txt:/wan/latency.txt:ro" dedan-services:local \
  sh -c "/wan/wan.sh && exec dedan-node" >/dev/null
docker network connect dedan-devnet_default "dedan-devnet-$label-1"   # anvil and the origin by name; the WAN interface came first for wan.sh
docker run -d --name "dedan-devnet-user-$label-1" --label "dedan.dynamic=$label" --network dedan-devnet_wan --ip "$user_ip" \
  --cap-add NET_ADMIN --dns 10.77.0.53 -e CITY="$label" \
  -v "$PWD/$dir/hosts.txt:/wan/hosts.txt:ro" -v "$PWD/$dir/latency.txt:/wan/latency.txt:ro" dedan-wan:local \
  sh -c "/docker-entrypoint.d/05-wan.sh && exec sleep infinity" >/dev/null
tail -n 2 "$dir/hosts.txt" >> "$DYNAMIC_HOSTS"   # the nameserver's geo table picks these up on its next sync
echo "containers dedan-devnet-$label-1 and dedan-devnet-user-$label-1 started"

echo "== registering $label.dedan.eth on chain (operator, name, records, hot key, attester) and joining its cells"
cat > "$dir/node.json" <<JSON
{ "count": 1, "nodes": [ { "label": "$label", "ip": "$node_ip", "port": 80, "class": "100M", "location": "$lat,$lon", "cell": "$cell",
  "operator": $operator, "hotKey": $hot, "attester": $attester, "user": "user-$label", "cells": $cells } ] }
JSON
cp "$dir/node.json" "contracts/deployments/nodes-$label.json"
$COMPOSE run --rm -T -e NODES_JSON="deployments/nodes-$label.json" deployer \
  "forge script script/RegisterNodes.s.sol:RegisterNodes --rpc-url http://anvil:8545 --broadcast -q" | grep -iE "error|revert" || true
for _ in $(seq 1 30); do
  curl -sf "$NS_API/nodes" | grep -q "\"$label.dedan.eth\"" && break; sleep 2
done
curl -sf "$NS_API/nodes" | grep -q "\"$label.dedan.eth\"" || { echo "the nameserver did not list $label.dedan.eth" >&2; exit 1; }
echo "$label.dedan.eth resolved by the nameserver: $(curl -sf "$NS_API/nodes" | python3 -c "
import json,sys; n=[x for x in json.load(sys.stdin)['nodes'] if x['label']=='$label'][0]; print('ipv4', n['ipv4'], 'owner', n['owner'][:10], 'active', n['active'])")"

echo "== every human attests the members of its own cell (one vote per node; the new node's cell now has a member)"
for h in $(users); do attest_from "$h" || true; done   # the nodes reveal their own token keys on chain
sleep 12
echo "== done. Try it:"
echo "  docker exec dedan-devnet-user-$label-1 dig +short www.images.com"
echo "  docker exec dedan-devnet-user-$label-1 curl -s -D - -o /dev/null http://www.images.com/images/photo-1.png"
echo "  curl -s $NS_API/cells | python3 -m json.tool | grep -B 3 -A 12 '\"$cell\"'"
echo "  make routing-check"
