#!/bin/sh
# The traffic path end to end (`make e2e`), on a running devnet:
#   1. the customer's account exists on chain (the deploy script creates and funds it); its config (domains, origin,
#      tier, cache rules), signed by the account key with `dedan-cp config acme`, is at the control plane, which
#      relays it to the nameserver and every node; the key recovered from the signature is the account key on chain
#   2. the assignment gives acme its slot set; each member obtained a certificate for www.images.com and its own
#      name through the control plane (ACME at Pebble, DNS-01 through the nameserver's TXT records)
#   3. what each city is steered to; `curl https://www.images.com` from a city user: MISS then HIT, a fetch token,
#      the chain trusted through Pebble's root
#   4. a purge signed by the account key: the next fetch is a MISS
#   5. expired objects are served stale: with the origin stopped (stale-if-error), and with the origin back while a
#      background fetch revalidates them (stale-while-revalidate), then a hit
#   6. an unknown Host gets 421
#   7. the Frankfurt operator points its record at an address nobody answers on: the endpoint challenge fails and
#      the nameserver stops answering the name; the record restored, it is answered again
set -eu
cd "$(dirname "$0")/../.."
. deploy/scripts/lib.sh
CP=${CP_API:-http://127.0.0.1:8070}
rpc="--rpc-url http://anvil:8545"
S=$(deployed settlement)
ACME=0x61636d6500000000000000000000000000000000000000000000000000000000   # bytes32("acme")
json() { python3 -c "import json,sys; d=json.load(sys.stdin); $1"; }
want() { case "$1" in *"$2"*) ;; *) echo "FAIL: expected '$2'"; exit 1;; esac; }
RUN=$(date +%s)   # a query string per run: acme keeps query strings in its cache key, so a rerun starts from misses
fetch() { user "$1" curl -s --cacert /tmp/pebble-root.pem -o /dev/null -D - "https://www.images.com$2" | tr -d '\r' | awk 'tolower($1)=="http/1.1"||tolower($1)=="http/2" {code=$2} tolower($1)=="x-cache:" {cache=$2} tolower($1)=="x-dedan-node:" {tok=$2} END {printf "%s %s token=%s\n", code, cache, tok}'; }

echo "== 1. the customer's account on chain, and its config signed by the account key, relayed by the control plane"
key=$(cast call "$S" "\"accounts(bytes32)(address,uint256,uint256,uint256,uint64)\"" "$ACME" $rpc | head -1 | tr -d '\r\n')
[ "$key" != "0x0000000000000000000000000000000000000000" ] || { echo "FAIL: accounts(acme) has no key on chain; the deploy script creates and funds acme (make devnet-reset)"; exit 1; }
echo "acme on chain; account key $key"
have=$(curl -s "$CP/v1/configs" | json "print(any(c['payload']['account_id']=='acme' for c in d['configs']))")
[ "$have" = True ] || $COMPOSE exec -T -e DEDAN_KEY_INDEX=1 cp dedan-cp config acme
curl -s "$CP/v1/configs" | json "
for c in d['configs']:
    p=c['payload']; print('config %s v%d signed as %s: domains %s, origin %s, tier %d nodes %s %s, cache rules %s' % (p['account_id'], p['version'], c['signer'], p['domains'], p['origins'][0]['url'], p['tier']['nodes'], p['tier']['class'], p['tier']['availability'], p['cache_rules']))"
# the key that signed it, recovered from the signature the way every node and the nameserver check it: ecrecover
# over the EIP-191 hash of keccak256(signer || 0x00 || ts || nonce || canonical JSON of the payload)
set -- $(curl -s "$CP/v1/configs" | json "
import json
c=[c for c in d['configs'] if c['payload']['account_id']=='acme'][0]
m=c['signer'].encode()+b'\0'+c['ts'].to_bytes(8,'big')+c['nonce'].to_bytes(8,'big')+json.dumps(c['payload'],sort_keys=True,separators=(',',':')).encode()
s=c['sig'][2:]; print('0x'+m.hex(), '0x%064x' % int(s[128:],16), '0x'+s[:64], '0x'+s[64:128])")
signer=$($COMPOSE run --rm -T deployer "h=\$(cast keccak $1); e=\$(cast keccak \$(cast concat-hex 0x19457468657265756d205369676e6564204d6573736167653a0a3332 \$h)); cast call 0x0000000000000000000000000000000000000001 \$(cast concat-hex \$e $2 $3 $4) --rpc-url http://anvil:8545" | tail -1 | tr -d '\r\n')
signer=0x${signer#0x000000000000000000000000}
echo "-- the key recovered from the signature: $signer (the account key on chain: $key)"
want "$(echo "$signer" | tr 'A-F' 'a-f')" "$(echo "$key" | tr 'A-F' 'a-f')"

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
r=$(fetch frankfurt "/images/photo-2.png?r=$RUN"); echo "-- frankfurt: $r"; want "$r" "200 MISS"
r=$(fetch frankfurt "/images/photo-2.png?r=$RUN"); echo "-- frankfurt: $r"; want "$r" "200 HIT"
r=$(fetch tokyo "/images/photo-2.png?r=$RUN"); echo "-- tokyo:     $r"; want "$r" "200 MISS"

echo
echo "== 4. a purge signed by the account key, relayed by the control plane, applied by every node"
$COMPOSE exec -T -e DEDAN_KEY_INDEX=1 cp dedan-cp purge acme all '*'
sleep 2
r=$(fetch frankfurt "/images/photo-2.png?r=$RUN"); echo "-- frankfurt after the purge: $r"; want "$r" "200 MISS"

echo
echo "== 5. expired objects served stale: while the origin is asked again in the background, and with the origin down"
r=$(fetch frankfurt "/images/photo-3.png?r=$RUN"); echo "-- frankfurt photo-3: $r"; want "$r" "200 MISS"
r=$(fetch frankfurt "/images/photo-4.png?r=$RUN"); echo "-- frankfurt photo-4: $r"; want "$r" "200 MISS"
echo "   waiting 65 s for both to expire (acme's cache rules: TTL 60 s, then 30 s of stale-while-revalidate)..."; sleep 65
r=$(fetch frankfurt "/images/photo-4.png?r=$RUN"); echo "-- stale-while-revalidate, frankfurt photo-4: $r (served at once, refreshed in the background)"; want "$r" "200 STALE"
sleep 2
r=$(fetch frankfurt "/images/photo-4.png?r=$RUN"); echo "-- frankfurt photo-4: $r"; want "$r" "200 HIT"
echo "   waiting 30 s more: photo-3 is past its revalidation window..."; sleep 30
docker stop dedan-devnet-origin-1 >/dev/null
r=$(fetch frankfurt "/images/photo-3.png?r=$RUN"); docker start dedan-devnet-origin-1 >/dev/null
echo "-- stale-if-error, origin down, frankfurt photo-3: $r"; want "$r" "200 STALE"
for _ in $(seq 1 15); do node frankfurt curl -sf -o /dev/null http://origin/images/photo-4.png && break; sleep 1; done

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
