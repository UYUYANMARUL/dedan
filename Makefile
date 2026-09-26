COMPOSE := docker compose -f deploy/docker-compose.yml
FORGE_IMG := ghcr.io/foundry-rs/foundry:latest
FORGE := docker run --rm -v $(CURDIR)/contracts:/work -v dedan-foundry-home:/home/foundry -w /work --entrypoint sh $(FORGE_IMG) -c

NODES := frankfurt tokyo seoul newyork istanbul
USERS := user-frankfurt user-tokyo user-seoul user-newyork user-istanbul

.PHONY: help deps contracts-build contracts-test core-test resolver-test workspace-test images devnet devnet-down devnet-reset smoke demo routing-check add-node add-client demo-settlement settlement-down e2e

help: ## list targets
	@grep -E '^[a-z-]+:.*##' $(MAKEFILE_LIST) | sed 's/:.*## / - /'

deps: ## fetch Solidity dependencies into contracts/lib (needs network once): ENSv2 contracts-v2 with its submodules, forge-std
	$(FORGE) '[ -d lib/contracts-v2/contracts/src ] || (git clone --quiet --depth 1 --branch post-audit-2 https://github.com/ensdomains/contracts-v2 lib/contracts-v2 && git -C lib/contracts-v2 submodule update --init --depth 1 -- contracts/lib/openzeppelin-contracts contracts/lib/openzeppelin-contracts-v4 contracts/lib/openzeppelin-contracts-upgradeable contracts/lib/ens-contracts contracts/lib/verifiable-factory contracts/lib/buffer contracts/lib/unruggable-gateways contracts/lib/solsha1); [ -d lib/forge-std ] || forge install --no-git foundry-rs/forge-std'

contracts-build: deps ## compile contracts
	$(FORGE) 'forge build'

contracts-test: deps ## run the Foundry tests
	$(FORGE) 'forge test -vv'

core-test: ## unit and property tests of crates/core (add `-- --ignored` by hand for the live devnet tests)
	cargo test -p dedan-core

resolver-test: ## unit tests of the nameserver
	cargo test -p dedan-resolver

workspace-test: ## every crate's unit tests
	cargo test --workspace

images: ## build the services image (nameserver, auditor, aggregator; cargo-chef caches the dependency build)
	$(COMPOSE) build resolver

devnet: ## start anvil, deploy the contracts, register the city nodes on ENS, then the nameserver, Pebble, the origin, five city nodes and five users on the simulated WAN
	$(COMPOSE) up -d --build --wait anvil pebble origin
	$(COMPOSE) run --rm -T deployer
	sh deploy/scripts/register-nodes.sh
	$(COMPOSE) up -d --build --wait resolver cp $(NODES) $(USERS) dashboard dozzle
	@echo "dashboard: http://localhost:9090   logs and terminals: http://localhost:9999"

devnet-down: ## stop everything (cities added with add-node included) and delete volumes
	@ids=$$(docker ps -aq --filter label=dedan.dynamic); [ -z "$$ids" ] || docker rm -f $$ids
	rm -rf deploy/devnet/dynamic deploy/devnet/clients deploy/devnet/dynamic-hosts.txt contracts/deployments/nodes-*.json
	$(COMPOSE) --profile smoke --profile full --profile settlement --profile sabotage down -v --remove-orphans

devnet-reset: devnet-down devnet ## fresh devnet

smoke: ## one settlement day, ACME issuance through the nameserver, origin check (fresh devnet)
	sh deploy/scripts/smoke.sh

demo: ## simulated WAN: RTT matrix, nodes from ENS, key reveals and attestations on chain from five cities, per-city steering, an IP change on ENS, then the routing check
	sh deploy/scripts/demo.sh

routing-check: ## from each city's user: resolve www.images.com, fetch by name, verify different nodes served the same bytes as the origin
	sh deploy/scripts/routing-check.sh

add-node: ## add a node at a location to the running devnet: make add-node LABEL=paris LAT=48.8566 LON=2.3522
	sh deploy/scripts/add-node.sh $(LABEL) $(LAT) $(LON)

add-client: ## add a client (a human's device) at a location: make add-client LABEL=berlin LAT=52.52 LON=13.405
	sh deploy/scripts/add-client.sh $(LABEL) $(LAT) $(LON)

e2e: ## the traffic path: signed configs, certificates through the control plane, HTTPS miss then hit with a token, purge, stale on origin failure, 421, a forged endpoint not served
	sh deploy/scripts/e2e.sh

demo-settlement: ## fresh devnet: the settlement loop (city auditors, aggregator, countersigner) with a tampered node, a dead node, a spoofing attester and a lying auditor; report, recompute, void, claim
	sh deploy/scripts/demo-settlement.sh

settlement-down: ## stop the settlement loop and the sabotage containers (the rest of the devnet stays up)
	$(COMPOSE) --profile settlement --profile sabotage rm -sf aggregator countersigner auditor-frankfurt auditor-tokyo auditor-seoul auditor-newyork auditor-istanbul auditor-spoof auditor-liar origin-tampered
