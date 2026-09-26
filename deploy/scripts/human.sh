#!/bin/sh
# A human at a location verifies with World ID (mock) and becomes an attester on the registry, if not one already:
#   sh deploy/scripts/human.sh <city> [key index]      default: the city's registered attester key
# Prints the account and whether the contract grants it ROLE_ATTESTER. Used by the dashboard's "verify" button.
set -eu
cd "$(dirname "$0")/../.."
. deploy/scripts/lib.sh
city=${1:?city}; index=${2:-$(node_field "$city" attester)}
addr=$(address_of "$index")
echo "human in $city: key index $index, account $addr"
if [ "$(is_attester "$addr")" = "true" ]; then
  echo "already verified: holds ROLE_ATTESTER on DedanRegistry"
else
  echo "World ID (mock): proof accepted, nullifier $((300 + index)) burned for action dedan-attester"
  register_attester "$index" "$((300 + index))"
  echo "registerAttester sent: $addr now holds ROLE_ATTESTER"
fi
echo "verified: $addr"
