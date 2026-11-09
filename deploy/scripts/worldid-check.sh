#!/bin/sh
# World ID end to end on a running devnet (`make worldid-check`), with a stand-in for the Developer Portal's answer
# (deploy/scripts/portal-stub.py): the zero-knowledge proof itself can only come from World App or the simulator
# (WORLD-ID.md). Everything else runs for real: the control plane's RP signature and its checks, its attestation,
# the gate and the registry on chain.
#   1. a Proof of Human for a spare key (27, or the next without the role) as attester: verified, attested,
#      registered; the key holds the role
#   2. the same human (same nullifier) with another key: refused, one key per human per role
#   3. a Selfie Check proof: refused, a role needs Proof of Human
#   4. a proof naming the first key submitted for the other: refused, the signal is the account
#   5. a second answer to a request already answered: refused
set -eu
cd "$(dirname "$0")/../.."
. deploy/scripts/lib.sh
DASH=${DASH_API:-http://127.0.0.1:9090}
env_file=deploy/devnet/world-id.env
[ ! -f "$env_file" ] || { echo "$env_file exists (a real app): move it aside to run this check"; exit 1; }
cleanup() {
  rm -f "$env_file"
  $COMPOSE up -d --no-deps --force-recreate --wait cp >/dev/null 2>&1 || true
}
trap cleanup EXIT
json() { python3 -c "import json,sys; d=json.load(sys.stdin); $1"; }
post() { curl -s -X POST "$DASH$1" -H 'content-type: application/json' -d "$2"; }
field_hash() { h=$(cast keccak "$1" | tr -d '\r\n'); echo "0x00${h#0x}" | cut -c1-66; }   # keccak256 >> 8

echo "== a throwaway World ID app (staging) and the portal stand-in"
printf '%s\n' "DEDAN_WORLD_ID_APP_ID=app_staging_dedan_check" "DEDAN_WORLD_ID_RP_ID=rp_00000000000000aa" \
  "DEDAN_WORLD_ID_SIGNING_KEY=0xabababababababababababababababababababababababababababababababab" \
  "DEDAN_WORLD_ID_ENV=staging" "DEDAN_WORLD_ID_VERIFY_URL=http://dashboard:9095" > "$env_file"
docker exec -d dedan-devnet-dashboard-1 python3 "$PWD/deploy/scripts/portal-stub.py" 9095
$COMPOSE up -d --no-deps --force-recreate --wait cp >/dev/null
for _ in $(seq 1 30); do
  cfg=$(curl -s "$DASH/api/worldid/config")
  echo "$cfg" | grep -q '"configured": *true' && break
  sleep 1
done
echo "$cfg" | json "print('control plane:', 'configured' if d.get('configured') else 'NOT configured', d.get('app_id'), d.get('environment'))"
echo "$cfg" | grep -q '"configured": *true' || { echo "FAIL: the control plane did not pick up $env_file"; exit 1; }

# an IDKit 4 result as World App returns it for a request of the control plane, the account as the signal
result() {  # role account identifier schema nullifier nonce
  action=dedan-$1; sig=$(field_hash "$2")
  printf '{"protocol_version":"4.0","nonce":"%s","action":"%s","environment":"staging","responses":[{"identifier":"%s","issuer_schema_id":%s,"signal_hash":"%s","nullifier":"%s","expires_at_min":%s,"proof":["0x1","0x2","0x3","0x4","0x5"]}]}' \
    "$6" "$action" "$3" "$4" "$sig" "$5" "$(( $(date +%s) / 60 + 1440 ))"
}
request() { post /api/worldid/request "{\"role\":\"$1\"}" | json "print(d['rp_context']['nonce'])"; }
verify() {  # index result-json
  body=$(printf '{"role":"attester","index":%s,"result":%s}' "$1" "$2")
  post /api/worldid/verify "$body"
}
expect() {  # label http-json want-substring
  echo "-- $1: $(echo "$2" | json "print(d.get('error', d))")"
  echo "$2" | grep -q "$3" || { echo "FAIL: expected '$3'"; exit 1; }
}
# a key without the attester role (the check can run again on the same devnet), and another key
for i in 27 28 29 11 18 19; do [ "$(is_attester "$(address_of $i)")" = "true" ] || { ia=$i; break; }; done
[ -n "${ia:-}" ] || { echo "FAIL: every spare key already holds the attester role: make devnet-reset"; exit 1; }
ib=$([ "$ia" = 28 ] && echo 29 || echo 28)
aa=$(address_of "$ia"); ab=$(address_of "$ib")
nullifier=0x$(openssl rand -hex 31)

echo "== 1. Proof of Human for $aa (key $ia) as attester"
n=$(request attester)
out=$(verify "$ia" "$(result attester "$aa" proof_of_human 1 "$nullifier" "$n")")
watch=$(echo "$out" | json "print(d.get('watch',''))")
[ -n "$watch" ] || { echo "FAIL: $out"; exit 1; }
echo "$out" | json "print('verified:', d['mode'], 'nullifier', d['nullifier'][:18] + '...')"
for _ in $(seq 1 60); do st=$(curl -s "$DASH$watch" | json "print(d['status'])"); [ "$st" != running ] && break; sleep 2; done
curl -s "$DASH$watch" | json "print(d['log'].strip().splitlines()[-1])"
[ "$(is_attester "$aa")" = "true" ] || { echo "FAIL: $aa does not hold ROLE_ATTESTER"; exit 1; }

echo "== 2. the same human with key $ib"
used=$(request attester)
out=$(verify "$ib" "$(result attester "$ab" proof_of_human 1 "$nullifier" "$used")")
expect "refused" "$out" "one per human"
echo "== 3. a Selfie Check proof"
n=$(request attester)
out=$(verify "$ib" "$(result attester "$ab" selfie 11 "0x$(openssl rand -hex 31)" "$n")")
expect "refused" "$out" "Proof of Human"
echo "== 4. a proof naming $aa submitted for key $ib"
out=$(verify "$ib" "$(result attester "$aa" proof_of_human 1 "0x$(openssl rand -hex 31)" "$n")")
expect "refused" "$out" "another account"
echo "== 5. another answer to the request step 2 used"
out=$(verify "$ib" "$(result attester "$ab" proof_of_human 1 "0x$(openssl rand -hex 31)" "$used")")
expect "refused" "$out" "used or expired"
echo
echo "WORLD ID CHECK OK"
