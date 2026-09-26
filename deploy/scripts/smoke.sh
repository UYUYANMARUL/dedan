#!/bin/sh
# One settlement day against a fresh devnet, then the mocked ACME path and the sample origin.
# Run once per `make devnet` (it publishes period 1; a second run would need period 2).
set -eu
cd "$(dirname "$0")/.."

compose() { docker compose --progress quiet "$@"; }
forge_stage() {
  compose run --rm -T -e STAGE="$1" deployer \
    "forge script script/Smoke.s.sol:Smoke --rpc-url http://anvil:8545 --broadcast -vv"
}

echo "== contracts, stage 1: fund account, register node and auditor, publish period 1 roots"
forge_stage 1

echo "== clock: +73 h so period 1 is final and the withdrawal delay has passed"
compose exec -T anvil cast rpc --rpc-url http://127.0.0.1:8545 evm_increaseTime 262800 >/dev/null
compose exec -T anvil cast rpc --rpc-url http://127.0.0.1:8545 evm_mine >/dev/null

echo "== contracts, stage 2: node claims, customer bill applied and withdrawal paid"
forge_stage 2

echo "== acme: lego follows the customer CNAME, the nameserver publishes TXT, Pebble validates DNS-01, certificate issued for www.images.com"
compose --profile smoke run --rm -T lego

echo "== origin: sample assets reachable"
compose exec -T origin wget -qO- http://127.0.0.1/assets/manifest.txt
compose exec -T origin sh -c 'wget -qO /dev/null http://127.0.0.1/assets/blob-2m.bin && echo "blob-2m.bin: ok"'

echo "SMOKE OK"
