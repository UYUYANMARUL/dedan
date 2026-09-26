#!/bin/sh
# Runs inside the Foundry image: fetches the Solidity dependencies once (ENSv2 contracts with the submodules they
# import, forge-std), then deploys the devnet contracts. Working directory is /repo/contracts.
set -eu
if [ ! -d lib/contracts-v2/contracts/src ]; then
  git clone --quiet --depth 1 --branch post-audit-2 https://github.com/ensdomains/contracts-v2 lib/contracts-v2
  git -C lib/contracts-v2 submodule update --init --depth 1 -- \
    contracts/lib/openzeppelin-contracts contracts/lib/openzeppelin-contracts-v4 \
    contracts/lib/openzeppelin-contracts-upgradeable contracts/lib/ens-contracts contracts/lib/verifiable-factory \
    contracts/lib/buffer contracts/lib/unruggable-gateways contracts/lib/solsha1
fi
[ -d lib/forge-std ] || forge install --no-git foundry-rs/forge-std
mkdir -p deployments
forge script script/Deploy.s.sol:Deploy --rpc-url "$RPC_URL" --broadcast -vv
