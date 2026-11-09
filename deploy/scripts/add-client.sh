#!/bin/sh
# Adds a client, a human's device at a chosen location, to the running devnet (demo only):
#   sh deploy/scripts/add-client.sh <label> <lat> <lon>        e.g.  berlin 52.52 13.405
# Starts a user container on the simulated WAN in its own /24 (10.77.<n>.0/24 for n = 1..15, then 10.77.<160+n>.0/24
# past the node cities' /20s, up to 95 clients: the address is what tells the nameserver where the client is), with round trips to every node derived from the great-circle distance,
# randomised by ±20 % per node plus per-packet jitter. Its DNS is the nameserver, so `dig`/`curl www.images.com`
# from it are steered like a real user's. The human behind it holds anvil key 44+n (funded here past anvil's 60
# accounts) and starts unverified: the
# dashboard's "verify with World ID (mock)" registers it as an attester, then it can submit node metrics.
set -eu
cd "$(dirname "$0")/../.."
. deploy/scripts/lib.sh
label=${1:?usage: add-client.sh <label> <lat> <lon>}; lat=${2:?lat}; lon=${3:?lon}
echo "$label" | grep -Eq '^[a-z][a-z0-9-]{1,30}$' || { echo "label: lowercase letters, digits, dashes" >&2; exit 1; }
[ -z "$(host_field "user-$label" 1)" ] && [ -z "$(host_field "$label" 1)" ] || { echo "$label already exists" >&2; exit 1; }
touch "$DYNAMIC_HOSTS"
n=$(( 1 + $(ls deploy/devnet/clients 2>/dev/null | wc -l) ))
[ "$n" -le 95 ] || { echo "at most 95 clients" >&2; exit 1; }
octet=$n; [ "$n" -le 15 ] || octet=$((160 + n))
ip="10.77.$octet.30"; attester=$((44 + n))
[ "$attester" -le 59 ] || cast rpc anvil_setBalance "$(address_of "$attester")" 0x3635c9adc5dea00000 --rpc-url http://anvil:8545 >/dev/null
dir=deploy/devnet/clients/$label; mkdir -p "$dir"
cell=$(curl -sf "$NS_API/cell?lat=$lat&lon=$lon" | python3 -c "import json,sys; print(json.load(sys.stdin)['cell'])")
echo "== client $label at $lat,$lon: address $ip (prefix 10.77.$octet.0/24), cell $cell, human key index $attester"

# WAN tables for the container: existing containers send toward this /24 undelayed, so the client carries the whole
# round trip on its side (wan.sh applies half of the table entry, hence 2x). Round trip: (5 ms + 20 ms per 1000 km)
# scaled by a random factor in [0.8, 1.2] per node, plus a per-packet jitter of about 8 %.
{ hosts; printf '%-18s %-10s %-13s %-7s %s %s\n' "user-$label" "$label" "$ip" user "$lat" "$lon"; } > "$dir/hosts.txt"
hosts | awk -v me="$label" -v lat="$lat" -v lon="$lon" 'BEGIN { srand() } $4=="node" {
  pi = atan2(0, -1); dlat = ($5 - lat) * pi / 180; dlon = ($6 - lon) * pi / 180
  a = sin(dlat / 2) ^ 2 + cos(lat * pi / 180) * cos($5 * pi / 180) * sin(dlon / 2) ^ 2
  km = 2 * 6371 * atan2(sqrt(a), sqrt(1 - a)); rtt = (5 + 0.02 * km) * (0.8 + 0.4 * rand())
  printf "%s %s %d %d\n", me, $2, 2 * int(rtt), 1 + int(rtt * 0.08) }' > "$dir/latency.txt"
echo "round trips (ms, with jitter):"; awk '{printf "  %s %s ±%s\n", $2, int($3 / 2), $4}' "$dir/latency.txt"
docker run -d --name "dedan-devnet-user-$label-1" --label "dedan.dynamic=$label" --network dedan-devnet_wan --ip "$ip" \
  --cap-add NET_ADMIN --dns 10.77.0.53 -e CITY="$label" \
  -v "$PWD/$dir/hosts.txt:/wan/hosts.txt:ro" -v "$PWD/$dir/latency.txt:/wan/latency.txt:ro" dedan-wan:local \
  sh -c "/docker-entrypoint.d/05-wan.sh && exec sleep infinity" >/dev/null
tail -n 1 "$dir/hosts.txt" >> "$DYNAMIC_HOSTS"   # the nameserver's geo table picks it up on its next sync
cat > "$dir/client.json" <<JSON
{ "count": 1, "nodes": [ { "label": "$label", "ip": "$ip", "location": "$lat,$lon", "cell": "$cell", "attester": $attester, "user": "user-$label", "kind": "client" } ] }
JSON
sleep 11  # one nameserver sync, so the client's prefix is known before anyone digs from it
echo "== container dedan-devnet-user-$label-1 started; the human is not verified yet. Try it:"
echo "  docker exec dedan-devnet-user-$label-1 dig +short www.images.com"
echo "  docker exec dedan-devnet-user-$label-1 curl -s -D - -o /dev/null http://www.images.com/images/photo-1.png"
echo "  docker exec dedan-devnet-user-$label-1 ping -c 3 $(node_ip frankfurt)   # frankfurt"
echo "  sh deploy/scripts/human.sh $label && sh deploy/scripts/attest.sh $label   # verify (World ID mock), then submit metrics"
