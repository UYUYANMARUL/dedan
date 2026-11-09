#!/bin/sh
# A human verified with World ID (IDKit, Proof of Human) takes a role on the registry with the proof the control
# plane verified at the Developer Portal and attested (WORLD-ID.md). Run by the dashboard after /v1/world-id/verify:
#   sh deploy/scripts/worldid-register.sh <attester|auditor|operator> <key index> <nullifier> <nonce> \
#      <expires_at_min> <issuer_schema_id> <genesis_min> <p0> <p1> <p2> <p3> <p4>
set -eu
cd "$(dirname "$0")/../.."
. deploy/scripts/lib.sh
role=$1; index=$2; shift 2
proof="($1,$2,$3,$4,$5,[$6,$7,$8,$9,${10}])"
rpc="--rpc-url http://anvil:8545"
key=$(private_key "$index")
addr=$(address_of "$index")
tuple="(uint256,uint256,uint64,uint64,uint256,uint256[5])"
echo "account $addr (devnet key $index) takes the $role role"
case "$role" in
  attester)
    cast send "$(registry)" "\"registerAttester($tuple)\"" "\"$proof\"" --private-key "$key" $rpc | grep -E 'transactionHash|status' ;;
  auditor)
    stake=$(cast call "$(registry)" "\"minStake(uint8)(uint256)\"" 1 $rpc | cut -d' ' -f1 | tr -d '\r\n')
    cast send "$(deployed usdc)" "\"approve(address,uint256)\"" "$(registry)" "$stake" --private-key "$key" $rpc >/dev/null
    cast send "$(registry)" "\"registerAuditor($tuple,uint256)\"" "\"$proof\"" "$stake" --private-key "$key" $rpc | grep -E 'transactionHash|status' ;;
  operator)
    cast send "$(registry)" "\"registerOperator($tuple)\"" "\"$proof\"" --private-key "$key" $rpc | grep -E 'transactionHash|status' ;;
  *) echo "unknown role $role"; exit 2 ;;
esac
check=$(case "$role" in attester) echo isAttester ;; auditor) echo isAuditor ;; operator) echo isOperator ;; esac)
echo "$check($addr) = $(cast call "$(registry)" "\"$check(address)(bool)\"" "$addr" $rpc | tr -d '\r\n')"
