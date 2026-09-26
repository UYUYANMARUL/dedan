#!/bin/sh
# The traffic path end to end (`make e2e`), on a running devnet:
#   1. the customer's account exists on chain and is funded; the control plane's signed config for acme (domains,
#      origin, tier, the account key) reaches the nameserver and every node
#   2. the assignment gives acme its slot set; each member obtained a certificate for www.images.com and its own
#      name through the control plane (ACME at Pebble, DNS-01 through the nameserver's TXT records)
#   3. what each city is steered to; `curl https://www.images.com` from a city user: MISS then HIT, a fetch token,
#      the chain trusted through Pebble's root
#   4. a purge signed by the account key: the next fetch is a MISS
#   5. the origin stopped: an expired object is served stale; the origin back
#   6. an unknown Host gets 421
#   7. the Frankfurt operator points its record at an address nobody answers on: the endpoint challenge fails and
#      the nameserver stops answering the name; the record restored, it is answered again
set -eu
cd "$(dirname "$0")/../.."
. deploy/scripts/lib.sh
CP=${CP_API:-http://127.0.0.1:8070}
rpc="--rpc-url http://anvil:8545"
S=$(deployed settlement); USDC=$(deployed usdc)
ACME=0x61636d6500000000000000000000000000000000000000000000000000000000   # bytes32("acme")
json() { python3 -c "import json,sys; d=json.load(sys.stdin); $1"; }
want() { case "$1" in *"$2"*) ;; *) echo "FAIL: expected '$2'"; exit 1;; esac; }
fetch() { user "$1" curl -s --cacert /tmp/pebble-root.pem -o /dev/null -D - "https://www.images.com$2" | tr -d '\r' | awk 'tolower($1)=="http/1.1"||tolower($1)=="http/2" {code=$2} tolower($1)=="x-cache:" {cache=$2} tolower($1)=="x-dedan-node:" {tok=$2} END {printf "%s %s token=%s\n", code, cache, tok}'; }

echo "== 1. the customer's account on chain and the control plane's signed config"
key=$(cast call "$S" "\"accounts(bytes32)(address,uint256,uint256,uint256,uint64)\"" "$ACME" $rpc | head -1 | tr -d '\r\n')
if [ "$key" = "0x0000000000000000000000000000000000000000" ]; then
  cast send "$S" "\"createAccount(bytes32,address)\"" "$ACME" "$(address_of 1)" --private-key "$(private_key 1)" $rpc >/dev/null
  cast send "$USDC" "\"approve(address,uint256)\"" "$S" 10000000000 --private-key "$(private_key 1)" $rpc >/dev/null
  cast send "$S" "\"deposit(bytes32,uint256)\"" "$ACME" 10000000000 --private-key "$(private_key 1)" $rpc >/dev/null
  echo "acme created and funded with 10,000 USDC by the customer key $(address_of 1)"
else
  echo "acme exists; account key $key"
fi
for _ in $(seq 1 15); do   # the control plane republishes the config once it sees the key on chain
  k=$(curl -s "$CP/v1/configs" | json "print([c['payload']['account_key'] for c in d['configs'] if c['payload']['account_id']=='acme'][0])")
  [ "$k" != "0x0000000000000000000000000000000000000000" ] && break; sleep 2
done
curl -s "$CP/v1/configs" | json "
for c in d['configs']:
    p=c['payload']; print('config %s v%d seq %d signed by %s: domains %s, origin %s, tier %d nodes %s %s, account key %s' % (p['account_id'], p['version'], p['seq'], c['signer'], p['domains'], p['origins'][0]['url'], p['tier']['nodes'], p['tier']['class'], p['tier']['availability'], p['account_key'][:10]))"

echo
echo "== 2. the slot set and the members' certificates (ACME DNS-01 at Pebble through the nameserver)"
curl -s "$NS_API/assignment" | json "print('acme slot set:', ', '.join(d['sets']['acme']))"
curl -sk https://127.0.0.1:15000/roots/0 > /tmp/dedan-pebble-root.pem
for c in $(users); do docker cp /tmp/dedan-pebble-root.pem "dedan-devnet-user-$c-1:/tmp/pebble-root.pem" >/dev/null; done
for _ in $(seq 1 60); do
  code=$(user frankfurt curl -s --cacert /tmp/pebble-root.pem -o /dev/null -w '%{http_code}' https://www.images.com/images/photo-1.png 2>/dev/null || true)
  [ "$code" = "200" ] && break; sleep 3
done
[ "$code" = "200" ] || { echo "FAIL: no TLS answer for www.images.com yet (control plane logs: docker logs dedan-devnet-cp-1)"; exit 1; }
docker logs dedan-devnet-cp-1 2>&1 | sed -E 's/\x1b\[[0-9;]*m//g' | grep -E 'certificate issued|certificate order' | cut -c1-160 | tail -5
echo "-- the certificate presented to a Frankfurt client:"
user frankfurt curl -s --cacert /tmp/pebble-root.pem -o /dev/null -v https://www.images.com/ 2>&1 | grep -E 'subject:|subjectAltName|issuer:|SSL connection' | sed 's/^\*/  /'

echo
echo "== 3. steering per city (dig +subnet) and HTTPS fetches: miss, then hit, with the node's token"
for c in $(users); do
  answer=$(user istanbul dig +short +subnet="$(user_ip "$c")/24" @10.77.0.53 www.images.com | grep -vE 'dedan-cdn' | head -1)
  printf '%-12s -> %-12s %s\n' "$c" "$answer" "$(hosts | awk -v ip="$answer" '$3==ip && $4=="node" {print $1 ".dedan.eth"}')"
done
r=$(fetch frankfurt /images/photo-2.png); echo "-- frankfurt: $r"; want "$r" "200 MISS"
r=$(fetch frankfurt /images/photo-2.png); echo "-- frankfurt: $r"; want "$r" "200 HIT"
r=$(fetch tokyo /images/photo-2.png); echo "-- tokyo:     $r"; want "$r" "200 MISS"

echo
echo "== 4. a purge signed by the account key, relayed by the control plane, applied by every node"
$COMPOSE exec -T -e DEDAN_KEY_INDEX=1 cp dedan-cp purge acme all '*'
sleep 2
r=$(fetch frankfurt /images/photo-2.png); echo "-- frankfurt after the purge: $r"; want "$r" "200 MISS"

echo
echo "== 5. the origin stopped: an expired object is served stale (stale-if-error)"
r=$(fetch frankfurt /images/photo-3.png); echo "-- frankfurt: $r"; want "$r" "200 MISS"
echo "   waiting 65 s for the object to expire (the devnet nodes' default TTL is 60 s)..."; sleep 65
docker stop dedan-devnet-origin-1 >/dev/null
r=$(fetch frankfurt /images/photo-3.png); docker start dedan-devnet-origin-1 >/dev/null
echo "-- origin down, frankfurt: $r"; want "$r" "200 STALE"

echo
echo "== 6. an unknown Host"
r=$(user frankfurt curl -s -o /dev/null -w '%{http_code}' -H 'Host: nobody.example' http://10.77.16.1/)
echo "-- $r for nobody.example at frankfurt (421 Misdirected Request)"; want "$r" 421

echo
echo "== 7. a forged endpoint: the Frankfurt operator's record points at 10.77.16.99, where nothing answers the challenge"
$COMPOSE run --rm -T -e LABEL=frankfurt -e IP=10.77.16.99 -e OPERATOR_INDEX=2 deployer "forge script script/SetNodeIp.s.sol:SetNodeIp --rpc-url http://anvil:8545 --broadcast -q" | grep -E "record set" || true
for _ in $(seq 1 20); do
  v=$(curl -s "$NS_API/nodes" | json "print([n['verified'] for n in d['nodes'] if n['label']=='frankfurt'][0])")
  [ "$v" = "False" ] && break; sleep 2
done
echo "-- nameserver: frankfurt verified=$v; frankfurt.node.dedan-cdn.test -> [$(user istanbul dig +short frankfurt.node.dedan-cdn.test | tr '\n' ' ')] (nothing: the record is not answered)"
echo "-- acme.dedan-cdn.test from Frankfurt -> $(user istanbul dig +short +subnet=10.77.16.30/24 @10.77.0.53 acme.dedan-cdn.test | grep -vE dedan-cdn | head -1) (another member)"
$COMPOSE run --rm -T -e LABEL=frankfurt -e IP=10.77.16.1 -e OPERATOR_INDEX=2 deployer "forge script script/SetNodeIp.s.sol:SetNodeIp --rpc-url http://anvil:8545 --broadcast -q" >/dev/null
for _ in $(seq 1 20); do
  [ "$(user istanbul dig +short frankfurt.node.dedan-cdn.test)" = "10.77.16.1" ] && break; sleep 2
done
r=$(user istanbul dig +short frankfurt.node.dedan-cdn.test); echo "-- record restored: frankfurt.node.dedan-cdn.test -> $r (challenge passed again)"
want "$v" False; want "$r" 10.77.16.1

echo
echo "E2E OK"
