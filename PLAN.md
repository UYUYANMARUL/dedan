# Dedan — Build plan

Companion to `ARCHITECTURE.md` (v3.1). Rewritten Sep 26, 2026 for the hackathon build: milestones instead of calendar phases, each ending in one `make` target that is its definition of done, and a demo storyline that grows with them.

## Status

| Piece | State |
|---|---|
| Architecture v3.1 | `ARCHITECTURE.md`: crowd latency map, slot economics, ENSv2 names and Enhanced Access Control roles as the identity layer; reviewed against 15 attacks (Appendix B) |
| Contracts | `contracts/`: `DedanRegistry.sol` (an ENSv2 `PermissionedRegistry` for `dedan.eth` with Dedan's rules: World ID-gated roles, node names as tokens, stakes, `attest` and `revealKey` as events), `DedanResolver.sol` (`addr`, `text`, DNS records, `multicall`), `DedanRoles.sol`, `LibDnsWire.sol`, `Settlement.sol`, mocks; scripts `Deploy`, `RegisterNodes`, `SetNodeIp`, `Smoke`; ENSv2 vendored from `ensdomains/contracts-v2` `post-audit-2`; 23 Foundry tests green (2026-09-26) |
| Local devnet | `deploy/`: anvil, deployer, register-nodes, the Rust nameserver, Pebble CA, sample origin, five city nodes minting real tokens (njs) and five city users on a /16 simulated WAN; `make devnet`, `make smoke`, `make demo` pass (2026-09-26). Steering comes from attestations on the chain |
| Rust services | `crates/core` (`dedan-core`): envelope, token, merkle, geo, map, assign, score, identity, ens; 40 unit and property tests plus two live devnet tests (`cargo test -p dedan-core -- --ignored`: event replay, ENS resolution of the five nodes). `crates/resolver` (`dedan-resolver`): the authoritative nameserver (hickory-server + axum) syncing names, records, roles, key reveals and attestations from the chain; replaced the Python mock |

## Decisions already taken

| Decision | Choice |
|---|---|
| Language | Rust for every service (one Cargo workspace), Solidity for contracts |
| Node engine | Pingora 0.9 with `pingora-cache` and a Dedan-written disk storage; OpenSSL TLS backend |
| TLS | Per-node keys and certificates; control plane runs ACME under Dedan's account; Let's Encrypt `shortlived` profile, `default` as the degraded-mode switch |
| Chain | World Chain (World ID 4.0 verifier on-chain, native USDC, cent-level gas); Dedan's own ENSv2 tree on it (root → `eth` → `dedan`), bridged to canonical L1 `dedan.eth` when ENSv2 reaches mainnet; contracts immutable with governance-settable parameters |
| Economics | Slot-based pay × audited quality (ARCHITECTURE §6, §8) |
| Steering | Crowd-sourced latency map keyed by H3 cell, read by the resolvers; no regions; attestations are `attest` transactions on `DedanRegistry` from `ROLE_ATTESTER` holders and the map is a pure function of the chain's events |
| Identity | ENSv2: a node is `<label>.dedan.eth`, a token in `DedanRegistry`; its endpoint and config are the name's records (`addr(60)`, `dedan.*` text, DNS A/AAAA); every permission is an Enhanced Access Control role: World ID proofs grant `ROLE_NODE_OPERATOR`, `ROLE_ATTESTER`, `ROLE_AUDITOR` on a per-human resource, owners grant `ROLE_NODE_SIGNER` on their name, governance holds the root roles; no CCIP-Read, one chain |
| Auditors | One app, two record types: location-tagged attestations for the map, assigned probes with tokens for quality and class |
| Scope for v1 | classes 100M/1G (1G only when auditors can prove it), node counts 3/6/12, no pinned tier, no metered billing, no web UI, no HTTP/3, no DNSSEC, resolver operators permissioned, k = 2 countersigning, no automatic slashing |
| Build order | Identity (contracts, core, nameserver) first because every service reads it; then the settlement loop (auditor + aggregator); then the traffic path (node, control plane) |
| Mocks | Live only under `deploy/`, stand in only for things outside the codebase (chain, CA, World ID, USDC, customer DNS, origin) or for services not yet built. Every crate is the real implementation; the only devnet-specific behaviour in a crate is a flag that shortens a period, lowers k, or points at a local endpoint |

## Demo storyline

| Step | Shown | Target | Real by |
|---|---|---|---|
| 1 | Five World ID-verified operators own `<city>.dedan.eth` with their node's IP as an ENS record; a customer owns `images.com` and adds two CNAMEs; a verified human in each city attests round trips to every node on chain; the nameserver steers each city's client to the node the map says is closest; the Frankfurt operator changes its IP on chain and `dig` follows; a spoofed attestation is rejected; each city's user resolves `www.images.com` and fetches the asset by name: five different nodes, every body identical to the origin's, every token naming the node that owns the connected IP | `make demo` | today |
| 2 | Round trips between the cities; what a wrong assignment would cost | `make demo` | today |
| 3 | Five auditors probe for one 5-minute period; the Seoul node serves tampered bytes, the Tokyo node is down; the aggregator prints per-node uptime, integrity, throughput, quality and pay; the root goes on chain with two signatures; `recompute` reproduces it; after the dispute window the Istanbul operator claims USDC; the evidence file names Seoul; a spoofed attestation (wrong cell for its IP) is rejected | `make demo-settlement` | M2 |
| 4 | Real Pingora nodes serve `https://www.images.com` with Pebble-issued certificates obtained through the control plane; the real resolver answers from the map and the public assignment; purge, stale-if-error, 421 for unknown hosts | `make e2e` | M3 |
| 5 | The same on World Chain Sepolia with a real hostname for seven periods | testnet | M4 |

## Local devnet

```
make devnet           # anvil, deployer, register-nodes (the five city names on ENS), Rust nameserver, Pebble, origin, five city nodes, five users
make smoke            # operator, node, attester and auditor registration through World ID mocks and EAC roles; one settlement day on-chain; ACME issuance through the nameserver and the customer CNAME; origin check
make demo             # WAN round trips, customer DNS onboarding, key reveals and attestations on chain from every city, steering from the map, an IP change on ENS, a spoofed attestation rejected, then the routing check
make add-node LABEL=paris LAT=48.8566 LON=2.3522   # a node and a user at a chosen location, registered on chain and attested; also POST /nodes on the dashboard
make routing-check    # from each city's user: resolve www.images.com through the nameserver, fetch by name; passes when different nodes served byte-identical content
make demo-settlement  # (M2) one audited 5-minute period end to end
make e2e              # (M3) the traffic-path check
make contracts-test   # Foundry tests in the Foundry image
make core-test        # dedan-core unit and property tests; `cargo test -p dedan-core -- --ignored` runs the live ones
make resolver-test    # nameserver unit tests
make images           # build the nameserver image (cargo-chef caches the dependency build)
make devnet-reset     # tear down (volumes included) and start again
```

| Service | Image | Stands in for | Endpoint |
|---|---|---|---|
| anvil | ghcr.io/foundry-rs/foundry | World Chain | http://localhost:8545, chain id 31337 |
| deployer | same | Dedan governance deploying | one-shot; deploys `MockUSDC`, `MockWorldID`, the ENSv2 tree (`LabelStore`, root and `eth` `PermissionedRegistry`, `DedanRegistry` as `dedan`, `DedanResolver`, `UniversalResolverV2` with a stub gateway provider), `Settlement`; funds 60 accounts; writes `contracts/deployments/devnet.json` |
| register-nodes | same | five operators and five attesters verifying with World ID and registering | one-shot (`deploy/scripts/register-nodes.sh` runs `RegisterNodes.s.sol` over `deploy/devnet/nodes.json`): `registerOperator`, `registerNode(<city>, [hotKey], 1000 USDC)`, records through `DedanResolver.multicall` (payout, class, location, port, enckey, version, DNS A), `registerAttester` |
| resolver | `dedan-resolver:local` (`crates/resolver`, Rust) | Dedan resolvers (steering `<account>.dedan-cdn.test` from the chain's map and the assignment, `<label>.node.dedan-cdn.test` from the ENS DNS records, `_acme-challenge` TXT) and the customers' DNS providers (`deploy/resolver/zones/*.zone`) | `dig -p 15353 @127.0.0.1`, WAN 10.77.0.53 (undelayed), management API :8055 (`/health`, `/nodes`, `/map`, `/assignment`, `/set-txt`, `/clear-txt`) |
| pebble | ghcr.io/letsencrypt/pebble | Let's Encrypt (`default` and `shortlived`) | https://localhost:14000/dir |
| origin | `dedan-wan:local` | a customer origin with an asset manifest and blobs, in New York | http://localhost:8080, WAN 10.77.64.20 |
| frankfurt, tokyo, seoul, newyork, istanbul | `dedan-wan:local` | CDN nodes: nginx `proxy_cache`, real `X-Dedan-Node` tokens minted by njs from `keys.json`, `/.well-known/dedan/blob/<n>`; replaced by the Pingora node in M3 | http://localhost:8011–8015, WAN 10.77.16.1, .32.1, .48.1, .64.1, .80.1 (one /20 per city) |
| user-\<city\> | `dedan-wan:local` | a human in each city: attester for the map (anvil keys 20–24), end user for fetches; DNS goes to the nameserver | WAN 10.77.<16c>.30 |
| user-\<label\> (client added while running) | `dedan-wan:local`, plain `docker run` with label `dedan.dynamic` | `make add-client LABEL=berlin LAT=52.52 LON=13.405` or the dashboard: a human's device at a chosen location in its own /24 (10.77.<n>.0/24, n = 1..15; the address is what tells the nameserver where it is), round trips to every node from great-circle distance ±20 % with per-packet jitter, DNS to the nameserver; its human (anvil key 45+n) starts unverified: `human.sh` (World ID mock) grants `ROLE_ATTESTER`, then `attest.sh` submits metrics | WAN 10.77.<n>.30 |
| \<label\>, user-\<label\> (added while running) | `dedan-wan:local`, plain `docker run` with label `dedan.dynamic` | `make add-node LABEL=paris LAT=48.8566 LON=2.3522` or the dashboard: a node and a user at a chosen location, /20 number 6 onward, WAN delays from great-circle distance (5 ms + 20 ms per 1000 km, carried by the new containers' side), keys anvil 30+ (three per city), registered and attested on chain; listed in `deploy/devnet/dynamic-hosts.txt`, which the nameserver re-reads; removed by `make devnet-down` | WAN 10.77.<16n>.1 and .30 |
| dashboard | `dedan-dashboard:local` (`deploy/dashboard`, stdlib Python + docker CLI) | a demo console: nodes from ENS, the map, container logs, a terminal link per container, `POST /nodes {label, lat, lon}` (runs `deploy/scripts/add-node.sh`), and a client's attestation flow: `POST /api/worldid {city, index?}` verifies a human through the World ID mock and grants `ROLE_ATTESTER` (`human.sh`), `POST /api/attest {city, index?, nodes?}` measures the nodes from that city's user container and sends the batch to `attest` (`attest.sh`); an unverified key is rejected by the contract | http://localhost:9090 |
| dozzle | amir20/dozzle | per-container logs and a browser terminal into any container (`DOZZLE_ENABLE_SHELL`) | http://localhost:9999 |
| lego (profile `smoke`) | goacme/lego | a node obtaining its certificate | one-shot |
| kubo (profile `full`) | ipfs/kubo | published settlement inputs (M2) | http://localhost:5001 |

**Simulated WAN.** `deploy/wan/hosts.txt` (name, city, ip, role, lat, lon) is the one source: 10.77.0.0/16, one /20 per city (`10.77.<16c>.0/20`), each node its own /24 inside it, origin and users inside their city's /20; `wan.sh` sets one `tc netem` band per city from `latency.txt`. Measured 2026-09-26 from Istanbul: CSS hit 10 ms local, 99 ms Frankfurt, 283 ms New York, 572 ms Tokyo.

**Customer DNS onboarding.** `deploy/resolver/zones/images.com.zone` holds `www CNAME acme.dedan-cdn.test` and `_acme-challenge.www CNAME _acme-challenge.acme.dedan-cdn.test`. The nameserver serves that zone as the customer's DNS provider and chases the CNAME into its own zone. `acme` answers from the account's slot set (`core::assign` over the ENS node table), ordered for the client's cell (ECS or source IP → `hosts.txt` on the devnet) by the map built from the chain's `Attested` and `KeyRevealed` events (`core::map`, k = 1 on the devnet), and by distance to the nodes' declared locations while the map has nothing for that cell. `make demo` has each city's attester ping every node and send one `attest` batch with the tokens the nodes minted, then shows `dig +subnet` per city answering from the chain, and a spoofed batch from Istanbul claiming the Tokyo cell landing in `/map` evidence because its /24 is Istanbul's.

**Devnet growth.** Done: the /16 layout above, five users, one netem band per city, and node cities added at runtime through `add-node.sh` (up to five) and clients through `add-client.sh` (up to fifteen); `wan.sh` has fifteen delay bands. The origin serves five PNG photos under `/images/` besides the blobs; `routing-check.sh` fetches one of them from every city's user, static and added.

**Devnet inputs that replace unbuilt services.** Until M3: `deploy/devnet/nodes.json` (label, ip, port, class, location, cell, operator/hotKey/attester key index, user container), `deploy/devnet/accounts.json` (label, hostnames, node count, class, availability), `deploy/devnet/keys.json` (the static hourly key the mock nodes mint with and reveal on chain). The nameserver reads `accounts.json` until the control plane exists; everything about nodes comes from the chain.

What the mocks replace: `MockUSDC` for USDC; `MockWorldID` (any proof with a non-zero nullifier) for the World ID verifier; the zone files for the customers' DNS providers; Pebble for the CA; nginx for the nodes; `StubGatewayProvider` (no gateways) for the Universal Resolver's CCIP-Read. Actor roles by anvil account index: 0 governance, 1 customer, 2–6 city operators, 7 smoke operator, 8 aggregator, 9 countersigner, 12–16 city hot keys, 17 smoke hot key, 20–24 city attesters, 25 smoke attester, 26 smoke auditor, 30+ the cities added at runtime (operator, hot key, attester per city).

The smoke: stage 1 registers an operator (World ID mock) and the node `smoke.dedan.eth` with a hot key and records, an attester attests, an auditor stakes, the hot key reveals a token key, the Universal Resolver resolves `dedan.class`, the operator rewrites the node's A record and nobody else can; then one settlement day (roots, +73 h, stage 2 claims); lego certificate for `www.images.com` + `istanbul.node.dedan-cdn.test` through the nameserver's TXT API; origin fetch.

## Milestones

### M0 — Contracts and devnet: done
Before testnet: replace `MockWorldID` with the Sepolia verifier, set `rpId`, register the app in the World ID Developer Portal, choose the governance multisig and timelock.

### M1 — `dedan-core`, devnet map and growth

Status: done, including the devnet map step and the /16 growth; M1b below reworked identity onto ENSv2.

| Module | Contents | Test |
|---|---|---|
| `envelope` | `Envelope<T> {payload, signer, ts, nonce, sig}`; `k256` + EIP-191 over `keccak256(signer ‖ ts ‖ nonce ‖ canonical JSON)`; `KeyLookup` trait over the registry cache; skew and replay checks | round trip, tampered payload, replay |
| `identity` | cache of `DedanRegistry` events from `alloy` `get_logs` replay: names, owners, stakes, EAC roles per resource and version, suspensions, key reveals, attestations; `KeyLookup` (`node:<label>` = current `ROLE_NODE_SIGNER` holders, `attester:<addr>`, `auditor:<addr>`, `resolver:<addr>`); `can_attest`, `can_probe`, `node_active` | matches anvil after register-nodes (live test) |
| `ens` | `EnsClient`: `UniversalResolverV2.resolve(dns_encode(name), calldata)` per ENSIP-10 with `alloy-ens` `namehash` and `dns_encode`; `NodeRecord {payout, ipv4, ipv6, port, class, location, enc_key, version}` from `addr(60)`, `text`, `dnsRecord` (ENSIP-6 wire parsed) | fixtures; live test resolves the five city nodes |
| `geo` | `h3o`: `cell(lat, lon)` at resolution 5, `grid_disk`, centroid, great-circle distance | known points map to known cells |
| `map` | `Map::from_chain(events, reveals, labels, geo, now, params)`: validity rules of ARCHITECTURE §7 (token against the revealed key and the client /24, GeoIP radius, one cell per human per epoch, caps), medians over distinct humans, anchor clamp, coverage k, ring aggregation, `rank(cell)`, node location estimates, evidence per rejection | fixtures per rule: spoofed cell, bad token, moved human, over cap, ring fallback |
| `assign` | greedy k-median over covered cells with sticky members, ≤ 1 rotation, one per /24, ASN preference, admission k = 3 | property tests: determinism, constraints, sticky, rotation bound, an unmeasured node is eligible but last resort |
| `token` | `mint` / `verify` for `X-Dedan-Node: <label>.<minute>.<hmac>` binding the client's /24 (IPv6 /48) | fixed vectors shared with the node and the nginx mock |
| `merkle` | OpenZeppelin `StandardMerkleTree`-compatible | vector from `contracts/test/Merkle.sol` |
| `types` | every message of §11; `NodeId` is the ENS label, `AuditorId` and `ResolverId` are accounts | JSON fixtures round trip |
| `score` | quality, SLA with the Wilson bound, bills, pools, rollover, payouts, cumulative leaves, evidence; inconclusive-class rule; failure attribution | fixtures: bad node, dead node, lazy auditor, spoofing attester |

Devnet in M1: done through M1b (lat/lon in `hosts.txt`, five `user-<city>` containers, map steering, the `make demo` attestation step, the /16 growth).

Notes from building the crate: envelopes sign `keccak256(signer ‖ 0x00 ‖ ts ‖ nonce ‖ canonical JSON)` in the EIP-191 form through alloy's local signer, so no separate `k256` dependency; the fetch token MAC is `HMAC-SHA256(k_hour, "<minute>|<client /24>")` in hex (IPv6 /48) (test vectors from Python's `hmac`, the nginx mock must produce the same string); the Merkle tree reproduces `@openzeppelin/merkle-tree`'s layout, checked against roots computed with `cast`; the SLA credit test uses the Wilson *upper* bound (a shortfall must be significant), which corrected a wording error in ARCHITECTURE §8; `Map` and `Assignment` are the same code the resolver, aggregator and `recompute` will call.

### M1b — ENSv2 identity and the Rust nameserver: done (Sep 26, 2026)
- Contracts: `DedanRegistry is PermissionedRegistry` (`registerOperator/Attester/Auditor` with World ID proofs, `registerNode(label, signers, stake)` minting `<label>.dedan.eth` with `ROLE_SET_RESOLVER | ROLE_NODE_SIGNER_ADMIN | ROLE_CAN_TRANSFER_ADMIN`, `revealKey`, `attest`, unbond/withdraw/suspend/slash, `nodeOf`, `isNodeActive`, `isNodeSigner`); `DedanResolver` (v1 `AddrResolver` + `TextResolver` + `DNSResolver` + `Multicallable`, authorised by the name's owner through the registry); `DedanRoles` (custom nybbles 40–68, `humanResource`); records written by the owner in one `multicall` (moved out of the registry to fit 24 KB).
- Core: `identity` and `ens` modules (above); `token` binds the /24; `map.from_chain`; `score::Payees` keyed by `addr(60)`; `Heartbeat` without endpoint or location.
- Nameserver: `crates/resolver` (`state` sync loop, `dns` handler with ECS, `api`), `cargo-chef` Dockerfile (`debian:trixie-slim` runtime, same glibc as the builder), compose service `resolver` with `/health`.
- Devnet: /16 WAN, njs tokens on the nodes, `RegisterNodes.s.sol`, `SetNodeIp.s.sol`, five users, `make demo` rewritten around the chain.

Check (green 2026-09-26): `make contracts-test` (23), `make core-test` (40) and the two live tests, `make devnet-reset && make smoke && make demo`.

### M2 — Settlement loop on the mocks (`make demo-settlement`)
- Done in M1b: the mock nodes mint real tokens through nginx `njs` from `keys.json` and serve `/.well-known/dedan/blob/<n>`.
- Auditor (`crates/auditor`): opportunistic attestations to every reachable node from its cell, sent as `attest` batches to `DedanRegistry`; assigned probes from `core::assign` over `nodes.json` + `accounts.json` + registry (landing, targeted with token, per-stream throughput, class probe, resolver conformance), flagged `anchor` for the map; `wreq` with a Chrome profile; signed batches every 5 min; receipts. One container per city on the WAN.
- Aggregator (`crates/aggregator`): `axum` ingest (`/v1/probe-results`, `/v1/asset-samples`; attestations and key reveals come from the chain) with envelope verification against the identity cache; SQLite per period; `origin-digest` subcommand; at period close the inputs directory (below), `core::score`, outputs and evidence, publish (kubo or `--inputs-dir`), countersignature, `publishRoot`; `--period 5m`; `countersign` and `recompute` modes.
- Demo script: Seoul pointed at `origin-tampered`, Tokyo stopped, one attester started with a spoofed cell, one auditor with `--devnet-lie`; table, evidence, transaction, recompute, time jump, claim.

Check: Seoul integrity 0, Tokyo uptime 0, three others paid; the spoofed attestation and the lying auditor appear in evidence and are unpaid; `recompute` reproduces the root; the claim succeeds; `voidRoot` inside the window blocks a claim.

### M3 — Traffic path (`make e2e`)
- Control plane (`crates/cp`): as before (accounts, domains with CNAME and CAA checks, signed configs, delegations, purge, SSE, asset samples, ACME runner with profile switch, TXT records). Tier = node count, class, availability target.
- Resolver (`crates/resolver`): exists since M1b. M3 adds GeoIP (`maxminddb`, DB-IP Lite) to a point then `core::geo` to a cell in place of `hosts.txt`, heartbeat intake, the endpoint challenge before a name's record is answered, the hourly CNAME re-check, and configs from the control plane instead of `accounts.json`.
- Node (`crates/node`): as before (Pingora `ProxyHttp`, disk `Storage`, `core::token`, rate cap, SSE config client, cert loop, heartbeat, purge, blob and challenge endpoints). Replaces nginx in the city service blocks.
- Build story: `cargo-chef` multi-stage Dockerfile, `make images`, host-side `cargo run` for development; check Pingora on macOS at M3 start.

Check (`make e2e`): deposit → account → domain → customer CNAMEs → assignment gives S nodes → certificates through the control plane → `dig` per city answers from the map with ECS scope → `curl https://www.images.com` miss then hit with a valid token → purge → origin stopped, stale served → 421 → forged endpoint not served → `make demo-settlement` green with real tokens.

### M4 — Hardening and testnet
As before: PSL and Let's Encrypt override (start at M2), CAA guidance, EAB CA switch, DB-IP Lite; Sepolia with three resolver operators and the countersigner key in one of them, real Let's Encrypt staging, governance-run auditors in five cities plus one external operator; runbooks. Check: seven periods, no voided root, countersigner a different organisation.

## Aggregator inputs and outputs (the recompute contract)

```
inputs/
  manifest.json           period, bounds, registry_block, price_table_version, config_versions, prev_outputs_hash, score_version, files{name: sha256}
  probe-results.jsonl     accepted ProbeResult envelopes, sorted by (auditor_id, ts, hash)
  attestations.jsonl      accepted Attested events of the window, sorted by (block, log index)
  key-reveals.jsonl       KeyRevealed events
  asset-samples.jsonl     AssetSample + OriginDigest envelopes
  configs.jsonl           AccountConfig versions in force (accounts.json on the devnet until M3)
outputs/
  map.json                cell → ranked nodes with medians and human counts; node location estimates
  assignment.json         slot sets (sticky on prev_outputs)
  quality.json            per (node, account): uptime, integrity, throughput, quality, valid probes
  multipliers.json        per account: s_dim, mult, credit
  payout-leaves.json, bill-leaves.json
  evidence.json           mismatches, invalid tokens, outliers, spoofed attestations, anchor disagreements
  roots.json              payout_root, bill_root, totals, digest, signatures
```
`inputs_hash = sha256(manifest.json)`. `recompute` reproduces `outputs/` byte for byte. Anything not in `inputs/` cannot influence money or the map.

## Facts verified (do not re-verify unless something breaks)

- World ID 4.0 verifier interface and addresses (mainnet `0x00000000009E00F9FE82CfeeBB4556686da094d7`, Sepolia `0x703a6316c975DEabF30b637c155edD53e24657DB`); `action = keccak256(bytes(action)) >> 8`; `verify` is a view function. (2026-09-25)
- USDC: World Chain mainnet `0x79A02482A880bCe3F13E09da970dC34dB4cD24D1`, Sepolia `0x66145f38cBAC35Ca6F1Dfb4914dF98F1614aeA88`. Chain ids 480 / 4801.
- OpenZeppelin 5.3.0 as pinned by contracts-v2 (v4 4.9.0 for the vendored ens-contracts). Foundry image = forge/anvil 1.8.1, uid 1000, scripts through `sh -c`.
- Let's Encrypt: `shortlived` GA (160 h); 50 new certs per registered domain per week, 5 per identical SAN set, 300 orders per 3 h; ARI renewals exempt; overrides take weeks.
- lego 5.5.2: options under `run`; exec hook `present <fqdn> <txt>`; CNAME following on by default; Pebble follows CNAME chains in DNS-01 answers. (2026-09-26)
- Delegated credentials: Firefox only.
- Pingora 0.9.0: HTTP/1.1 and HTTP/2; `pingora-cache` `Storage` trait; `certificate_callback` with OpenSSL/BoringSSL; `pingora-limits` `Rate`, `Inflight`; pin the version.
- `instant-acme` 0.8.5, `rcgen` 0.14, `hickory` 0.26.3 (ECS = `EdnsOption::Subnet`), `alloy` 2.5.0, `crypto_box` 0.9.1, `maxminddb` 0.32, `merkrs` 0.3.0.
- `wreq` (formerly `rquest`; rquest releases yanked): Chrome/Safari/Firefox TLS and HTTP/2 fingerprints, BoringSSL, profiles in `wreq-util`; auditor binary only. (2026-09-26)
- ENSv2 (2026-09-26): Sepolia beta only, Namechain cancelled, ships on L1; `ensdomains/contracts-v2` branch `post-audit-2`, solc 0.8.25, submodules openzeppelin-contracts(-v4, -upgradeable), ens-contracts, verifiable-factory, buffer, unruggable-gateways, solsha1; `PermissionedRegistry.register(label, owner, subregistry, resolver, roleBitmap, expiry)` and `_register(..., checkRoles)`; EAC roles one nybble each, admin = role << 128, `ROOT_RESOURCE = 0`, `grantRoles/revokeRoles/hasRoles`, event `EACRolesChanged(resource, account, old, new)`, at most 15 holders per role per resource (the assignee count lives in the nybble); `RegistryRolesLib` uses nybbles 0–9, 30, 31; a removed name bumps its EAC version; `UniversalResolverV2(rootRegistry, gatewayProvider, contractNamer).resolve(name, data)` per ENSIP-10/23; no ENSIP-6 profile in v2, so `DedanResolver` composes the v1 `AddrResolver`, `TextResolver`, `DNSResolver`, `Multicallable`; `alloy-ens` 2.5 gives `namehash` and `dns_encode` but hardcodes the mainnet Universal Resolver (ours is bound with `sol!`); `DedanRegistry` runtime is 22.9 KB at `optimizer_runs = 100`.
- hickory-server 0.26: `Server::new(handler).register_socket/register_listener(...).block_until_done()`; `RequestHandler::handle_request<R, T: Time>`; `MessageResponseBuilder::from_message_request`; ECS = `EdnsOption::Subnet(ClientSubnet)` with `set_scope_prefix`.
- `lukemathwalker/cargo-chef:latest-rust-1` is on Debian trixie (glibc 2.38+): the runtime stage must be `debian:trixie-slim`, not bookworm.
- ECS: Google sends it only when the scope is echoed; Cloudflare and Quad9 never send it.
- Browsers fall back on RST immediately, 20–30 s on a silent drop.
- GeoIP: DB-IP Lite may ship in images; GeoLite2 needs per-operator keys.
- Docker Desktop: `tc netem` works with `CAP_NET_ADMIN`; embedded DNS forwards to a container's `dns:` server with the container's source IP; host port 5353/udp belongs to mDNSResponder.
- `h3o` 0.11 (the five cities' resolution-5 cells are in `nodes.json`); `nginx:alpine` ships njs with `require('crypto')` HMAC (`deploy/wan/dedan.js`).

## Workspace layout

```
ARCHITECTURE.md, PLAN.md
Makefile                   devnet, devnet-reset, smoke, demo, images, contracts-test, core-test, resolver-test, deps; demo-settlement (M2), e2e (M3)
contracts/                 Foundry project: src/ (DedanRegistry, DedanResolver, DedanRoles, LibDnsWire, Settlement, mocks), script/ (Deploy, RegisterNodes, SetNodeIp, Smoke), test/, lib/contracts-v2 (vendored ENSv2), deployments/
deploy/                    docker-compose.yml, resolver/zones (customer zone files), wan/ (hosts.txt, dedan.js, node template), pebble/, lego/, origin/, scripts/, devnet/ (nodes, accounts, keys)
crates/                    core, resolver; (M2) auditor, aggregator; (M3) node, cp
```

`core::map`, `core::assign` and `core::score` are the same code in the resolvers, the auditors, the aggregator, the countersigner and `recompute`. That is what makes the map, the sets and the money public.

## Tunables (initial values, all in the price table or config)

| Parameter | v1 | Where |
|---|---|---|
| Cell resolution | H3 5 (about 17 km across) | core geo |
| Coverage k | 3 minimum, 5 target distinct humans per cell (1 on the devnet) | core map |
| Ring search | up to 3 rings | core map |
| Map window | 48 h of `Attested` events | core map |
| Epoch | 24 h; one cell per human per epoch | core map |
| Attestation validity | token from the node; the client /24 within 250 km of the cell; ≤ 3 per human per node per epoch; ≤ 50 per human per epoch for pay | core map |
| Anchor clamp | crowd median within ± 50 ms of the anchor median | core map |
| Steering tie window | 20 ms, broken by spare capacity | resolver |
| Node count tiers S | 3, 6, 12 | price table |
| Admission factor k | 3 | core assignment |
| Sticky quality threshold | 0.9; rotation ≤ 1 per set per day, only for > 10% improvement | core assignment |
| Inconclusive class | eligible at the highest conclusive class, 100M when none | core score |
| Assigned probe interval | Poisson, mean 3 min per auditor per account | auditor |
| Auditors per target | min 3, target 5 | schedule |
| Auditor client profile | `wreq` Chrome | auditor |
| Per-stream throughput floor | 25 Mbps | aggregator |
| Integrity confirmation | 2 auditors + 1 other member served matching bytes | aggregator |
| SLA floor | 0.5 | aggregator |
| Pools | 10% auditor, 5% resolver, 5% protocol | price table |
| Windows | dispute 24 h, withdrawal delay 72 h, unbonding 7 d | contracts |
| Minimum stakes (devnet) | node 1,000, resolver 1,000, auditor 100 USDC | `DedanRegistry.setMinStake(kind, amount)` |
| Bonds | node 30 × largest slot pay, resolver 30 × daily share, auditor 10 × daily fee, aggregator 2 × max daily bills | governance policy |
| Certificate | `shortlived` (`default` as the degraded-mode switch), renew at 2/3 via ARI, ≤ 40 slot members per hostname | control plane |

## Risks to the demo

| Risk | Answer |
|---|---|
| Rust images build slowly | `cargo-chef`, `make images` once, host-side `cargo run` |
| Pingora on macOS | check at M3 start; fall back to the Linux container |
| Period clock vs chain clock | wall-time periods; anvil advanced only for the dispute window |
| `tc` has seven bands per host | per-city bands from the M1 growth |
| k = 1 on the devnet hides collusion cases | the `core::map` fixtures cover them; the demo shows the spoofed-cell rejection, which does not need k |
| Mock tokens vs real tokens | `keys.json` disappears at M3; `make demo-settlement` passes in both |
| Attester gas on World Chain | each attester key holds a little ETH on the devnet; a paymaster is an M4 item |
| EAC's 15 holders per role per resource | population roles live on per-human resources; a node hands out at most 15 signer keys |
| `<label>.dedan.eth` is not a canonical `.eth` name yet | the tree mirrors ENS; bridging canonical `dedan.eth` to this registry is a pointer, not a rename |

## Verification (end to end)

- Today: `make devnet` + `make smoke` + `make demo`.
- M1/M1b: `make contracts-test`, `make core-test`, `cargo test -p dedan-core -- --ignored` against the devnet, `make devnet-reset && make smoke && make demo` (green, 2026-09-26).
- M2: `make demo-settlement`.
- M3: `make e2e`, then `make demo` and `make demo-settlement` with the real node.
- Contracts: `make contracts-test`. Workspace: `cargo test --workspace`.
- No step is done until its check runs green on the devnet.
