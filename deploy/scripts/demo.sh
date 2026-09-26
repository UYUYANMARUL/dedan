#!/bin/sh
# Devnet demo. Needs a running devnet (`make devnet`). Cities added with add-node.sh take part like the static five.
#   1. measured round trips between the city nodes
#   2. the node names and records as the nameserver resolved them from ENS
#   3. verified humans (city users and added clients) attest round trips to every node, to the registry contract, with the fetch
#      tokens the nodes minted; the map is rebuilt from those events; a spoofed batch is rejected
#   4. what the nameserver answers for a client in each city (ECS), and the customer onboarding chain
#   5. the Frankfurt operator changes its node's IP record on chain; the DNS answer follows
#   6. every city's user resolving www.images.com and fetching an image by name: different nodes, same bytes as
#      the origin (deploy/scripts/routing-check.sh, also `make routing-check`)
set -eu
cd "$(dirname "$0")/../.."
. deploy/scripts/lib.sh
all=$(cities)

echo "== round trip between nodes, ms (ping avg of 3; deploy/wan/latency.txt, distance-based for added cities)"
printf '%-12s' ""
for c in $all; do printf '%12s' "$c"; done
echo
for a in $all; do
  printf '%-12s' "$a"
  for b in $all; do
    if [ "$a" = "$b" ]; then printf '%12s' "-"; else printf '%12s' "$(ping_rtt "$a" "$(node_ip "$b")")"; fi
  done
  echo
done

echo
echo "== node names and records, as the nameserver resolved them from ENS (GET /nodes)"
curl -s "$NS_API/nodes" | python3 -c "
import json,sys
for n in json.load(sys.stdin)['nodes']:
    print('%-22s ipv4=%-12s class=%s location=%s,%s owner=%s signers=%d active=%s' % (n['name'], n['ipv4'], n['class'], n['location']['lat'], n['location']['lon'], n['owner'][:10], len(n['signers']), n['active']))"

echo
echo "== hot keys reveal this hour's token key on chain, then a verified human in each city attests round trips"
for c in $all; do reveal_key "$c"; done
for h in $(users); do attest_from "$h" $all || true; done   # an unverified client's batch is rejected: that is the point
# a spoofed batch: an Istanbul human claims the Tokyo cell; the token still binds its Istanbul network
token=$(fetch_token istanbul "$(node_ip tokyo)")
cast send "$(registry)" "\"attest((uint64,bytes32,uint32,uint32,bytes,bytes)[])\"" \
  "\"[($(node_field tokyo cell),$(cast keccak tokyo | tr -d '\r\n'),1,0,$(hex "$token"),$(hex "$(user_ip istanbul | awk -F. '{printf "%s.%s.%s.0/24", $1, $2, $3}')"))]\"" \
  --private-key "$(private_key "$(node_field istanbul attester)")" --rpc-url http://anvil:8545 >/dev/null
echo "istanbul: also claimed a 1 ms round trip to tokyo from the Tokyo cell (spoofed)"
echo "waiting for the nameserver to rebuild the map from the chain..."; sleep 12
curl -s "$NS_API/map" | python3 -c "
import json,sys
m=json.load(sys.stdin)
print('attestations on chain: %d, key reveals: %d, covered cells: %d' % (m['attestations'], m['reveals'], m['covered']))
for r in m['ranks']:
    if r['rings']==0: print('  cell %d: ' % r['cell'] + ', '.join('%s %sms' % (n,v) for n,v in r['nodes']))
for e in m['rejected']: print('  rejected: %s by %s (%s)' % (e['reason'], e['auditor'][:10], e.get('node')))"

echo
echo "== the nameserver's answer for www.images.com by client city (dig +subnet, as recursive resolvers send it)"
for c in $(users); do
  answer=$(user istanbul dig +short +subnet="$(user_ip "$c")/24" @10.77.0.53 www.images.com | grep -vE 'dedan-cdn' | head -1)
  printf '%-12s -> %-12s %s\n' "$c" "$answer" "$(hosts | awk -v ip="$answer" '$3==ip && $4=="node" {print $1 ".dedan.eth"}')"
done
echo "-- the customer's own zone (deploy/resolver/zones/images.com.zone), chased in one answer:"
user istanbul dig +noall +answer www.images.com

echo
echo "== the Frankfurt operator changes its node's A record on chain; DNS follows within one sync"
$COMPOSE run --rm -T -e LABEL=frankfurt -e IP=10.77.16.99 -e OPERATOR_INDEX=2 deployer "forge script script/SetNodeIp.s.sol:SetNodeIp --rpc-url http://anvil:8545 --broadcast -q" | grep -E "record set" || true
sleep 12
echo "frankfurt.node.dedan-cdn.test -> $(user istanbul dig +short frankfurt.node.dedan-cdn.test)"
$COMPOSE run --rm -T -e LABEL=frankfurt -e IP=10.77.16.1 -e OPERATOR_INDEX=2 deployer "forge script script/SetNodeIp.s.sol:SetNodeIp --rpc-url http://anvil:8545 --broadcast -q" >/dev/null
for _ in $(seq 1 15); do  # the nameserver syncs every 10 s; the routing check below needs the restored record
  [ "$(user istanbul dig +short frankfurt.node.dedan-cdn.test)" = "10.77.16.1" ] && break; sleep 2
done
echo "(restored to 10.77.16.1)"

echo
echo "== every city's user resolves www.images.com through the nameserver (its ordinary DNS, no ECS) and fetches the same image"
sh deploy/scripts/routing-check.sh
