#!/bin/sh
# The settlement loop on the devnet with three bad actors (`make demo-settlement`). Needs a fresh devnet
# (`make devnet-reset`): the Settlement contract numbers periods from 1 and `make smoke` publishes its own.
#   1. the customer funds its account; the settlement profile starts: the five city humans run the auditor app
#      (staking as auditors on the way), plus the aggregator and the countersigner
#   2. sabotage: Seoul serves from a tampered origin, Tokyo is stopped, a human on an Istanbul network attests
#      Seoul's cell from 9,000 km away, an auditor in New York reports probes it never made
#   3. the first full period after that closes: inputs published, roots recomputed by the countersigner,
#      published with two signatures
#   4. the report; `recompute` reproduces the roots from the published inputs
#   5. a claim inside the dispute window fails; governance voids the root; after the window the claim still
#      fails; the aggregator republishes; after a new window the Frankfurt operator claims its pay
#   6. sabotage undone; the loop keeps settling every period
set -eu
cd "$(dirname "$0")/../.."
. deploy/scripts/lib.sh
deployed() { python3 -c "import json; print(json.load(open('contracts/deployments/devnet.json'))['$1'])"; }
S=$(deployed settlement); USDC=$(deployed usdc)
AGG=${AGG_API:-http://127.0.0.1:8060}
rpc="--rpc-url http://anvil:8545"
ACME=0x61636d6500000000000000000000000000000000000000000000000000000000   # bytes32("acme")
jump() { $COMPOSE exec -T anvil cast rpc --rpc-url http://127.0.0.1:8545 evm_increaseTime "$1" >/dev/null; $COMPOSE exec -T anvil cast rpc --rpc-url http://127.0.0.1:8545 evm_mine >/dev/null; }
agg() { $COMPOSE --profile settlement exec -T aggregator dedan-aggregator "$@"; }
usdc_balance() { cast call "$USDC" "\"balanceOf(address)(uint256)\"" "$1" $rpc | awk '{print $1}' | tr -d '\r\n'; }
json() { python3 -c "import json,sys; d=json.load(sys.stdin); $1"; }
# a payee's cumulative payout and Merkle proof from period $2's leaves, as `Settlement.claim` takes them
claim_args() { agg proof "/data/periods/$2" "$1" | json "print(d['cumulative'], '[' + ','.join(d['proof']) + ']')"; }

echo "== preflight: a fresh chain, the services image"
last=$(cast call "$S" "\"lastPeriod()(uint64)\"" $rpc | tr -d '\r\n')
[ "$last" = "0" ] || { echo "the chain already holds settlement period $last (make smoke publishes one): run make devnet-reset first"; exit 1; }
$COMPOSE up -d --build --wait resolver >/dev/null

echo "== the customer funds acme with 10,000 USDC (Settlement.createAccount, deposit)"
key=$(cast call "$S" "\"accounts(bytes32)(address,uint256,uint256,uint256,uint64)\"" "$ACME" $rpc | head -1 | tr -d '\r\n')
[ "$key" != "0x0000000000000000000000000000000000000000" ] || cast send "$S" "\"createAccount(bytes32,address)\"" "$ACME" "$(address_of 1)" --private-key "$(private_key 1)" $rpc >/dev/null
cast send "$USDC" "\"approve(address,uint256)\"" "$S" 10000000000 --private-key "$(private_key 1)" $rpc >/dev/null
cast send "$S" "\"deposit(bytes32,uint256)\"" "$ACME" 10000000000 --private-key "$(private_key 1)" $rpc >/dev/null

echo "== every node's hot key reveals this hour's and the next hour's token key on chain (the mock nodes mint with one static key)"
hour=$(( $(date +%s) / 3600 ))
for c in $(cities); do for h in $hour $((hour + 1)); do
  cast send "$(registry)" "\"revealKey(string,uint64,bytes32)\"" "$c" "$h" "0x$(token_key)" --private-key "$(private_key "$(node_field "$c" hotKey)")" $rpc >/dev/null
done; done

echo "== the settlement loop: five city humans start the auditor app (World ID proof + 100 USDC stake = ROLE_AUDITOR), the aggregator, the countersigner"
$COMPOSE --profile settlement --profile sabotage up -d --wait origin-tampered countersigner aggregator auditor-frankfurt auditor-tokyo auditor-seoul auditor-newyork auditor-istanbul
period_secs=$(curl -s "$AGG/health" | json "print(d['period_secs'])")
for _ in $(seq 1 30); do   # the auditors register on chain and take their first attestation round
  [ "$(docker logs dedan-devnet-auditor-istanbul-1 2>&1 | grep -c 'attested')" -ge 1 ] && break; sleep 2
done
docker logs dedan-devnet-auditor-istanbul-1 2>&1 | grep -E 'registered as|attested' | sed 's/^/  istanbul: /' | cut -c1-200

echo
echo "== sabotage"
echo "-- seoul now fetches from a tampered origin (every asset one byte longer than the real origin's)"
node seoul sh -c "sed -i 's#proxy_pass http://origin;#proxy_pass http://origin-tampered;#' /etc/nginx/conf.d/default.conf && rm -rf /var/cache/nginx/edge/* && nginx -s reload" 2>/dev/null
echo "-- tokyo is stopped"
docker stop dedan-devnet-tokyo-1 >/dev/null
echo "-- a human on an Istanbul network attests Seoul's cell from 9,000 km away (auditor-spoof, key 25)"
echo "-- an auditor in New York reports probes it never made, with made-up tokens (auditor-liar, key 26)"
$COMPOSE --profile settlement --profile sabotage up -d auditor-spoof auditor-liar
since=$(date +%s)
target=$(( (since / period_secs + 1) * period_secs ))
echo
echo "== waiting for the first full period after the sabotage: $(python3 -c "import time; print(time.strftime('%H:%M:%S', time.localtime($target)))") + $period_secs s + grace (auditors probe every few seconds meanwhile; logs: http://localhost:9999)"
spoof=$(address_of 25); liar=$(address_of 26)
# the outcome the loop must show; a period can miss the integrity confirmation when no two auditors sampled the
# same asset from seoul in it, so up to three periods are examined
check_period() {
  $COMPOSE --profile settlement exec -T aggregator sh -c "cat /data/periods/$1/outputs/score.json" | json "
q=d['quality']['acme']; ev=d['evidence']; pay=d['auditor_pay']
spoof='$spoof'.lower(); liar='$liar'.lower()
def ok(c, m): print(('  ok    ' if c else '  FAIL  ') + m); return c
r = ok(q['seoul']['integrity']==0.0, 'seoul: integrity 0 (tampered bytes confirmed by two auditors, another member served the right ones)')
r &= ok(q['tokyo']['uptime']==0.0 and q['tokyo']['failures']>0, 'tokyo: uptime 0 (stopped; every targeted probe failed to connect)')
r &= ok(all(q[n]['paid']>0 for n in ('frankfurt','newyork','istanbul')), 'frankfurt, newyork, istanbul: paid')
r &= ok(any(e['kind']=='spoofed_attestation' and e['auditor'].lower()==spoof for e in ev), 'spoofing attester in evidence')
r &= ok(any(e['kind']=='invalid_token' and e['auditor'].lower()==liar for e in ev), 'lying auditor in evidence')
r &= ok(not any(a.lower() in (spoof, liar) for a in pay), 'neither is paid')
r &= ok(sum(1 for a in pay if a.lower() not in (spoof, liar))>=5, 'the five honest auditors are paid')
sys.exit(0 if r else 1)"
}
tries=0; n=""
while :; do
  sleep 10
  n=$(curl -s "$AGG/periods" | json "ps=[p for p in d['periods'] if p['start']>=$target and p['published'] and p['period']>$tries]; print(ps[0]['period'] if ps else '')")
  if [ -z "$n" ]; then printf '  %s: %s probe batches filed so far\n' "$(date +%H:%M:%S)" "$(docker logs --since "${since}" dedan-devnet-aggregator-1 2>&1 | grep -c 'batch filed')"; continue; fi
  dir=/data/periods/$n
  echo
  echo "== period $n: the report (from outputs/ of the published inputs)"
  agg report "$dir"
  echo
  echo "== anyone recomputes the roots from inputs/ (the countersigner did exactly this before signing)"
  $COMPOSE --profile settlement exec -T countersigner dedan-aggregator recompute "$dir"
  echo
  echo "== checks"
  if check_period "$n"; then break; fi
  tries=$n
  [ "$(( $(curl -s "$AGG/periods" | json "print(len([p for p in d['periods'] if p['start']>=$target and p['published']]))") ))" -lt 3 ] || { echo "FAIL: not confirmed within three periods"; exit 1; }
  echo "   not all confirmed in period $n: examining the next one"
done

echo "== claims"
op=$(address_of 2)   # the Frankfurt operator: its name's addr(60) record is its payout address
set -- $(claim_args "$op" "$n"); cum=$1; proof=$2
echo "-- the Frankfurt operator claims $(python3 -c "print($cum/1e6)") USDC now, inside the 24 h dispute window"
if cast send "$S" "\"claim(address,uint256,bytes32[])\"" "$op" "$cum" "$proof" --private-key "$(private_key 2)" $rpc >/dev/null 2>&1; then echo "UNEXPECTED: the claim went through"; exit 1; fi
echo "   rejected (NoFinalRoot): no root is final yet"
echo "-- governance voids period $n (a dispute raised in the window); the clock jumps 25 h"
cast send "$S" "\"voidRoot(uint64)\"" "$n" --private-key "$(private_key 0)" $rpc >/dev/null
jump 90000
if cast send "$S" "\"claim(address,uint256,bytes32[])\"" "$op" "$cum" "$proof" --private-key "$(private_key 2)" $rpc >/dev/null 2>&1; then echo "UNEXPECTED: the claim went through"; exit 1; fi
echo "   rejected (NoFinalRoot): a voided root pays nobody"
echo "-- the dispute settled, the aggregator republishes period $n (the countersigner recomputes and signs again)"
agg publish "$dir" 2>&1 | sed -E 's/\x1b\[[0-9;]*m//g' | grep -oE 'roots published.*|error.*' | head -1 | cut -c1-120 || true
echo "-- the clock jumps 25 h: the root is final"
jump 90000
before=$(usdc_balance "$op")
latest=$(cast call "$S" "\"latestFinalPeriod()(uint64)\"" $rpc | tr -d '\r\n')   # the loop may have published more periods meanwhile
set -- $(claim_args "$op" "$latest"); cum=$1; proof=$2
cast send "$S" "\"claim(address,uint256,bytes32[])\"" "$op" "$cum" "$proof" --private-key "$(private_key 2)" $rpc >/dev/null
after=$(usdc_balance "$op")
echo "   claimed with period $latest's leaf: balance $(python3 -c "print($before/1e6)") -> $(python3 -c "print($after/1e6)") USDC (+$(python3 -c "print(($after-$before)/1e6)"))"
[ "$after" -gt "$before" ] || { echo "FAIL: the claim paid nothing"; exit 1; }

echo
echo "== undoing the sabotage: seoul back on the real origin, tokyo started, the spoofing attester and the lying auditor removed"
node seoul sh -c "sed -i 's#proxy_pass http://origin-tampered;#proxy_pass http://origin;#' /etc/nginx/conf.d/default.conf && rm -rf /var/cache/nginx/edge/* && nginx -s reload" 2>/dev/null
docker start dedan-devnet-tokyo-1 >/dev/null
$COMPOSE --profile settlement --profile sabotage rm -sf auditor-spoof auditor-liar origin-tampered >/dev/null 2>&1
echo
echo "SETTLEMENT DEMO OK"
echo "the loop keeps settling every $period_secs s: periods at $AGG/periods, logs at http://localhost:9999; make settlement-down stops it"
