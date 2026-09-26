#!/bin/sh
# Registers the devnet's five city nodes and their attesters on the Dedan registry: World ID-verified operators
# (mock proofs) mint <city>.dedan.eth with IP, class and location records and a hot key; one verified attester per
# city. Reads deploy/devnet/nodes.json. Needs a running devnet with the contracts deployed.
set -eu
cd "$(dirname "$0")/../.."
cp deploy/devnet/nodes.json contracts/deployments/nodes.json
docker compose --progress quiet -f deploy/docker-compose.yml run --rm -T deployer \
  "forge script script/RegisterNodes.s.sol:RegisterNodes --rpc-url http://anvil:8545 --broadcast -vv"
