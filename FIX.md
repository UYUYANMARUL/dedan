# FIX.md — problems found in the 2026-09-26 audit

Audit of `ARCHITECTURE.md` (v3.2) and `PLAN.md` against the code. When it ran, everything was green:
`make contracts-test` (27), `make core-test` (46 + 3 live), `make e2e`, and `make demo-settlement` on a fresh devnet.
The devnet never reaches the cases below. Items were found by three read-only reviews (contracts and their Rust
mirror; map, assignment, DNS, audits and settlement; node, control plane, TLS and devnet) and re-read in the code
unless marked (review). Items `PLAN.md` already lists as "Not built (M4)" or as deliberate deviations are left out.

Each entry: what is wrong, where, what breaks, the fix, the check, and its status. All fifty were settled on 2026-09-26: forty-eight fixed, two decided; the verification is at the end.

## Fix first: availability, money, trust boundaries

1. **A repeated line in a customer's asset list hangs settlement for everyone.** `core/src/schedule.rs:27` loops
   until `n.min(manifest.len())` unique picks while skipping duplicates; lists are never deduplicated. The period
   close and every auditor's sync hang. Fix: dedupe, bound by the unique count. Check: unit test `["a","a"]`.
   **Status:** fixed. `sample_assets` dedupes the list and stops at the number of distinct paths; test `repeated_manifest_lines_count_once`.
2. **One node's bad resolver stalls every resolver, auditor and the aggregator.** `core/src/ens.rs:134/152/165`
   `abi_decode(&raw)?`, and `node_records` fails on the first node (`:204`); operators hold `ROLE_SET_RESOLVER`.
   Fix: skip and log a node whose records do not decode. Check: unit test with undecodable bytes.
   **Status:** fixed. A record that does not decode reads as missing (`core/src/ens.rs`), so one node's resolver cannot stop the others.
3. **The countersigner signs whatever the aggregator put in `nodes.json` and `prev.json`.** It recomputes from the
   aggregator's own files and checks only registry events, period number and digest (`aggregator/src/main.rs:376-410`).
   Payees, previous leaves, the clock and the origin digests are trusted. Fix: the countersigner re-reads records and
   accounts at `registry_block`, checks the previous period against its on-chain root, the clock against the block,
   the digests against the origin, and batch times and nonces. Check: tamper each file, expect a refusal.
   **Status:** fixed. Before signing, the countersigner re-reads the names' records, the Settlement accounts and the tier prices at the registry block, checks `chain_time` against that block, recomputes the previous period from the inputs whose hash is on chain and compares `prev.json`, and takes the origin digests again (`aggregator/src/main.rs`). `compute` rejects batches signed outside the period and nonces filed twice; test `compute_checks_batches_and_configs`.
4. **Signer bonds are optional.** `publishRoot` checks only `isSigner` and the threshold (`Settlement.sol:240-252`);
   `bonds` is never read and the deploy deposits none. ARCHITECTURE.md:373 requires a bond. Fix: count only bonded
   signers; deposit the bonds in `Deploy.s.sol`. Check: Foundry test, unbonded signature rejected.
   **Status:** fixed. Signers below `minSignerBond` do not count toward the threshold; `Deploy.s.sol` bonds the aggregator and the countersigner with 20,000 USDC each; Foundry tests.
5. **Bills are not bounded per account and balances are never enforced.** `totalDeposited` never drops on withdraw
   (`Settlement.sol:126`) and an over-bill is silently capped (`:174`), so other customers' deposits pay it; nothing
   off chain checks the effective balance or the four-day rule (ARCHITECTURE.md:337, :600-601). Fix: count
   withdrawals in the bound, cap each bill at the account's effective balance, suspend accounts under four periods
   of list price at the resolver. Check: unit and Foundry tests.
   **Status:** fixed. `totalWithdrawn` joins the on-chain bound. The scorer caps each bill at the account's effective balance and shrinks its pools in proportion, and splits every bill exactly into its shares, so payouts can no longer exceed bills through rounding either. The resolver stops answering an account that cannot cover four periods at list price (`DEDAN_BALANCE_PERIODS`). Tests in `core/src/score.rs`, `core/src/settlement.rs` and Foundry.
6. **Account ids are first come, first served.** `Settlement.createAccount` is permissionless (`:115-120`), and the
   control plane trusts `accounts(id).key` for purges (ARCHITECTURE.md:410: "label reserved"). Fix: creation needs
   the registrar's (control plane's) signature over the id and key. Check: Foundry test.
   **Status:** fixed. `createAccount(id, key, registrarSig)`: the control plane signs `(chain, contract, id, key)` as the registrar; `Deploy.s.sol` creates and funds acme that way; Foundry tests.
7. **A root cannot be voided once the next one is published.** `voidRoot` needs `period == lastPeriod`
   (`Settlement.sol:259`); `publishRoot` accepts p+1 over a voided p. Fix: void any root inside its own window,
   which drops the roots published after it; refuse p+1 while p is voided. Check: Foundry test.
   **Status:** fixed. Any root can be voided inside its own window, which drops the roots after it (`RootsDropped`); no period after a voided one is accepted before it is republished; Foundry tests.
8. **Accounts never billed can never withdraw.** After the first final root `_applyBill` needs a leaf
   (`Settlement.sol:163-170`); leaves exist only for billed accounts (`core/src/score.rs:587-591`). Fix: a bill leaf
   for every account on chain. Check: unit test.
   **Status:** fixed. Every account created on the Settlement contract is an input (`accounts.json`) and gets a bill leaf, zero when never billed; test `bills_stop_at_the_balance_and_every_account_gets_a_leaf`.
9. **Anyone can make real cells unjoinable.** `LibCell.label` keeps the low 15 nibbles and `join` never validates ids
   (`DedanCells.sol:87-100`): joining `X | 1<<60` takes X's name and every later `join(_, [X])` reverts. Fix: accept
   only valid resolution-2 H3 indexes. Check: Foundry test.
   **Status:** fixed. `LibCell.isCell` accepts only valid resolution-2 H3 indexes and `join` reverts `BadCell` otherwise; Foundry test with `X | 1<<60`.
10. **Fetch tokens are not bound to time and can be reused.** Only the HMAC for the token's hour is checked
    (`core/src/map.rs:150-169`, `aggregator/src/settle.rs:297`); one real fetch backs unlimited "up" results. Fix:
    the token's minute must match the result's time; a token counts once per auditor. Check: unit test.
    **Status:** fixed. A token backs a probe result only up to 10 minutes after its minute (2 ahead for clock skew) and counts once per auditor; attestations use the same rule against the block's time, with the window a period parameter (`attestation_token_max_age_min`, widened on the devnet, whose chain clock jumps days ahead); tests in `core/src/map.rs`.
11. **Key reveals: any hour, any number of times, the last one wins.** `revealKey` checks only the signer
    (`DedanRegistry.sol:266-270`); `RevealSet::add` overwrites (`map.rs:136-138`); a missing key is scored as the
    auditor's forgery. Fix: one reveal per node and hour, after the hour (a governance flag allows early reveals on
    the devnet); the mirror keeps the first; tokens whose key is missing are unverifiable, not forged.
    **Status:** fixed. The registry takes one reveal per node and hour, after the hour ends (`earlyReveal`, set by governance on the devnet only); the mirror keeps the first reveal; a result whose key was never revealed is unverifiable, not a forgery; the node derives each hour's key from its hot key, so a restart mints with the key it already revealed; tests in core, the node and Foundry.
12. **One colluding human can decide a median.** With two votes the lower middle is the lie
    (`DedanCells.sol:184-188`, `map.rs:647-652`), and k is checked only on the best-voted member (`map.rs:494-499`).
    Fix: rank only members with at least k votes. Check: unit test.
    **Status:** fixed. A node ranks in a cell only with k votes of its own (`map.rs` `rank_votes`); test `votes_mirror_the_contract`. The lower-middle median stays (item 50).
13. **The auditor counts 4xx as up.** `auditor/src/main.rs:361-365` marks any status below 500 ok, and the node puts a
    token on 421 and 404 replies. Fix: ok only for 2xx and 304; 4xx is attributed like 5xx. Check: unit test.
    **Status:** fixed. The auditor counts only 2xx and 304 as success; other statuses are `Http4xx` or `Http5xx`, attributed alike; tests in the auditor and the scorer.
14. **Landing failures have no outlier check.** A failed landing needs no token; one auditor inventing a few connect
    failures halves a bill (ARCHITECTURE.md:308). Fix: an auditor whose failure rate for a target exceeds the
    others' by a wide margin is dropped for the period, unpaid, and named in the evidence. Check: unit test. (review)
    **Status:** fixed. The outlier check covers the landing probes of each account; test `landing_outlier_is_suspended_and_unpaid`.
15. **Eligibility uses the class the operator declares.** `core/src/config.rs:176` feeds the `dedan.class` record into
    eligibility and the admission cap; declaring 1G raises the cap tenfold. Fix: 100M for everyone until class
    probes exist. **Status:** fixed. Every node is 100M until class probes exist (`config::node_infos`).
16. **Suspension, slashing and bonds do not hold.** Unbond and withdraw ignore suspension and `withdraw` clears it
    (`DedanRegistry.sol:241-263`); a slashed node stays active; `isResolverOperator` ignores the bond. Fix: no unbond
    while suspended; active nodes and resolvers need the minimum stake. Check: Foundry test.
    **Status:** fixed. Unbonding and withdrawing are refused while suspended, for nodes and accounts; an active node needs the minimum stake, a resolver operator its bond, and an unbonding auditor stops being one; the Rust identity mirror follows the same rules; Foundry tests.
17. **The AAAA record of `<label>.node` skips the endpoint challenge** (`resolver/src/dns.rs:237-245`). Fix: challenge
    IPv6 addresses too. **Status:** fixed. IPv6 records are challenged like IPv4, and AAAA is answered only when verified.
18. **Configs are signed by the control plane, so it can reroute any customer.** `cp/src/main.rs:203` signs as `cp`;
    nodes and the resolver accept only that key. ARCHITECTURE.md:388, :169, :208 say account-signed. Fix: the
    customer signs its config with the account key, the control plane relays it, every reader verifies the key on
    chain. Check: e2e step 1 shows the account key as signer. **Status:** fixed. Configs are signed by the account key. The control plane relays them (`POST /v1/configs`, the customer CLI `dedan-cp config`), and nodes, resolvers, auditors and the aggregator verify them against the key on chain. `make e2e` step 1 shows the signer.
19. **Configs have no version or replay check** (`node/src/control.rs:23`, `resolver/src/state.rs:107`); an older
    signed config replayed rolls routing back. Fix: keep the highest version per account. Check: unit test.
    **Status:** fixed. Every reader keeps the highest version per account and refuses a lower one; tests in core, the node and the control plane.
20. **No object-size cap on the in-memory cache**; one huge origin object is buffered in memory (ARCHITECTURE.md:151).
    Fix: the account's `max_object_bytes` as Pingora's max file size. **Status:** fixed. The account's `max_object_bytes` is the largest object the node caches.
21. **RPC errors read as "no record" and change pay.** `ens.rs:122-124` maps every call error to `Ok(None)`. Fix:
    only a JSON-RPC error answer means "none"; transport errors fail the read. **Status:** fixed. Only a JSON-RPC error answer, such as a revert, reads as "no record"; an unreachable RPC fails the read, and the period close retries.
22. **Alias records fall back to text the operator sets.** `DedanResolver.text` returns the identity's own
    `dedan.latency`/`dedan.votes` when a cell has no votes (`DedanResolver.sol:60-68`). Fix: no fall-through for the
    keys DedanCells owns. Check: Foundry test. **Status:** fixed. Alias names return only what DedanCells answers for its keys; Foundry test.

## Correctness

23. **The map mirror ignores cell membership.** `Map::from_votes` never uses `_members` (`map.rs:381`); a node that
    left a cell stays ranked there for two days, unlike the contract's `snapshot`. The contract's cell record also
    still answers `dedan.latency.<label>` after `leave`. Fix: members only, both sides. Check: unit test with leave.
    **Status:** fixed. The mirror ranks only a cell's current members, and the contract's cell records answer only for members; tests on both sides.
24. **Resolver, auditors and aggregator compute different slot sets.** The resolver is sticky on its own snapshot of
    10 s ago with quality 1.0, the auditor has no previous day, the aggregator uses real quality. Fix: all three take
    the previous period from the aggregator's published outputs. **Status:** fixed. The aggregator serves `GET /v1/previous`; the resolver and the auditors make their sets sticky on it, so all three compute the sets the aggregator pays for.
25. **Kept members skip the /24 and admission checks** (`core/src/assign.rs:100-122`). Fix: the same checks as new
    members. Check: unit test. **Status:** fixed. Kept members pass the same /24 and admission checks as new ones; test `kept_members_pass_the_network_and_admission_checks`. ASN stays unset until GeoIP (M4).
26. **`withdraw` leaves the node's cell state behind.** Nobody can `leave` for a removed name and the next registrant
    of the label inherits memberships and votes. Fix: the registry removes the node from its cells on withdrawal, and
    a removed label cannot be registered again until its votes aged out. **Status:** fixed. `withdraw` removes the node from its cells (`Left` events) and clears its resolver records; the label cannot be registered again for two days, until its votes have aged out; Foundry tests.
27. **Code defaults are devnet values**: period 3600 s, grace 10 s, map k 1 (documented: daily, 1 h, 3); the
    attestation pay cap counts per period, not per epoch. Fix: documented defaults in code, devnet values in compose.
    **Status:** fixed. The code's defaults are the documented values (daily period, 1 h grace, k 3); compose sets the devnet values; the attestation pay cap counts per epoch.
28. **Stale-while-revalidate never serves stale.** The node does not override `should_serve_stale`, whose Pingora
    default refuses the revalidation case. Fix: serve stale while revalidating. Check: an e2e step. **Status:** fixed. `should_serve_stale` serves stale while revalidating. For HTTP/2 clients the node also marks the request HTTP/1.1 and gives it a Host header, because Pingora replays the request as HTTP/1 text for the background refresh; `make e2e` step 5 checks stale-while-revalidate and stale-if-error separately.
29. **The event stream has no replay.** The control plane ignores `Last-Event-ID` and drops lagged events; the node
    never sends it; a purge sent during a reconnect is lost (ARCHITECTURE.md:427). Fix: replay from a bounded log;
    a gap too old for the log makes the node purge everything. **Status:** fixed. The control plane keeps a log of the last 1,000 events and replays it after `Last-Event-ID`; an older gap sends `reset`, and the node then syncs fully and purges every account; a lagging stream ends instead of dropping events; tests in the control plane.
30. **A voided period is republished unchanged at the next close**, so a void only delays. Fix: no automatic
    republication of a voided period; an operator republishes by hand after the dispute. **Status:** fixed. The aggregator never republishes a voided period on its own; an operator runs `dedan-aggregator publish DIR` once the dispute is settled.
31. **The auditor reports its interface's /24, not the one the node saw** (`auditor/src/main.rs:481`); behind NAT
    every token fails. Fix: the token carries the prefix the node saw. **Status:** fixed. The token carries the network the node saw (`<label>.<minute>.<net>.<hmac>`); verification and the location check use it; tests in core and the auditor.
32. **Two names claiming one IP are both answered** (ARCHITECTURE.md:256 says neither). (review) **Status:** fixed. An address two names proved is answered for neither; test `an_address_two_names_proved_is_nobodys`.

## Gaps against the docs

33. **The schedule never assigns auditors to accounts**; every auditor probes every account (ARCHITECTURE.md:298).
    (review) **Status:** fixed. `schedule::auditors_for` draws each account's auditors from the day's seed (5 by default; 10 on the devnet so every auditor there probes); auditors probe only their accounts, and the aggregator scores only assigned auditors' results, a forged "up" staying evidence.
34. **`node_policy` (operator denylist, allowlist, minimum stake) is hardcoded empty** (`config.rs:79-81`).
    **Status:** fixed. Configs carry the node policy and the assignment applies it (`AccountInfo::from_config`).
35. **The on-chain price table does not constrain prices**: only a non-zero hash is checked (`Settlement.sol:201`);
    prices come from `accounts.json`. **Status:** fixed. The Settlement contract holds a price per tier (`setPrice`, `prices`); the aggregator reads it at the registry block into `prices.json` and the countersigner checks it.
36. **The node ignores `cache_rules`** (per-account TTL, query-string policy, Vary); the TTL comes from an env var.
    **Status:** fixed. Each route carries the account's TTL, query-string policy and object cap, and the origin's `Vary` is honoured (`Vary: *` is not cached).
37. **Certificates are ordered without the `shortlived` profile or ARI `replaces`** (`cp/src/main.rs:290`), so Pebble
    issues 90-day certificates; ARCHITECTURE.md:436-439 says 160 h and ARI renewal. **Status:** fixed. Orders use the `shortlived` profile (`DEDAN_ACME_PROFILE`) and name the renewed certificate with ARI `replaces`, retried without it when the CA refuses (the control plane opens a new ACME account on each start).

## Docs out of date

38. ARCHITECTURE.md header says v3/v3.1; §1 and the diagram send attestations to the registry, an hourly map from the
    aggregator and keys to the aggregator; §2 puts `attest` on the registry and has no DedanCells row. **Status:** fixed in ARCHITECTURE.md.
39. Resolution 5 / 17 km / "r = 1..2" / "ring search to 2" (ARCHITECTURE.md:140, :223, :285, :509, :529, :583) vs
    resolution 2 and three rings in code and PLAN. **Status:** fixed in ARCHITECTURE.md: resolution 2 and three rings throughout.
40. `attest` on DedanRegistry in ARCHITECTURE.md:275, :305 and PLAN.md:120; PLAN's workspace layout lacks DedanCells,
    LibCell, `crates/node`, `crates/cp`. **Status:** fixed in ARCHITECTURE.md and PLAN.md.
41. PLAN.md status and decisions: "njs" nodes, 40 + 2 core tests, no node or cp crate, OpenSSL and disk storage.
    **Status:** fixed in PLAN.md.
42. PLAN.md storyline, risks and M2 text: spoof "rejected" and "from the Tokyo cell", script-revealed keys,
    keys.json, "nameserver without a previous period", per-node cap, Seoul via `proxy_pass`. **Status:** fixed in PLAN.md.
43. ARCHITECTURE.md:239/:250 say unmeasured nodes are never chosen; code and PLAN use them last. **Status:** fixed in ARCHITECTURE.md.
44. ARCHITECTURE.md:143 orders cold starts by the map's estimate; code and PLAN use the declared location. **Status:** fixed in ARCHITECTURE.md: the declared location, the map's estimate as a later refinement.
45. The recompute contract is described as stronger than built (ARCHITECTURE.md:498, PLAN.md:188). **Status:** fixed in ARCHITECTURE.md and PLAN.md.
46. The endpoint challenge runs over `http://<ip>:<port>` without SNI; ARCHITECTURE.md:256 says HTTPS with SNI.
    **Status:** fixed in ARCHITECTURE.md: plain HTTP, the signed nonce being the proof.
47. ACME TXT answers carry TTL 0; ARCHITECTURE.md:94 says 60 s. **Status:** fixed in the resolver: TTL 60 s.
48. `demo-settlement.sh:57` says the spoofer is "9,000 km away"; the report says 7,944 km. **Status:** fixed in the script: 7,900 km.

## Design questions

49. A node that served tampered bytes still counts as delivered, so the customer pays for that slot.
    **Status:** decided: a member that served bytes matching no origin digest is not delivered, so the customer is credited for it (`score.rs`, ARCHITECTURE.md §8).
50. Ring search depth (2 in ARCHITECTURE v3.2, 3 in code) and the median of an even count. **Status:** decided: three rings, as the tunables already said; ARCHITECTURE.md aligned. The lower-middle median stays: a node now needs k votes of its own to rank, so a single lie cannot be the median.

## Tests missing

- **Status:** added. Foundry went from 27 to 41 tests, including every case listed here: the `NotCells` guard, a
  duplicate node in one batch, leave then rejoin, attest by a suspended attester, invalid cell ids, voiding a
  non-latest root. Rust unit tests went from 49 to 69: core 55, aggregator 3, node 4, control plane 3, auditor 2,
  resolver 2. `make e2e` checks stale-while-revalidate and stale-if-error separately.

## Known M4 items with security impact (already on PLAN's list, kept visible)

- `/set-txt` and `/clear-txt` at the resolver are unauthenticated until control-plane-signed TXT lands
  (`resolver/src/api.rs:188-208`): anyone who reaches its HTTP port can publish `_acme-challenge` for any account.

## Found while fixing

- **Payouts could exceed bills by rounding.** The fixed shares were floored from the sum of list prices while bills
  were floored per account, so with full quality cumulative payouts could pass cumulative bills by a few
  micro-USDC and `publishRoot` would revert for everyone. Each bill is now split exactly (item 5).
- **One reveal per hour broke restarted nodes.** With random hourly keys, a node restarted within an hour minted
  with a key it could no longer reveal, and every honest auditor's tokens for it looked forged. Keys are derived
  from the hot key (item 11).
- **Any account could claim another's hostname** once configs are account-signed. The control plane refuses a
  config naming a hostname another account serves; the CNAME ownership check stays M4.
- **Sealed origin secrets** (`OriginSecret`) were neither built nor on PLAN's list; now listed as M4.
- **The devnet's jumping chain clock** (the smoke and the dispute windows move it days ahead) made every
  attestation token look days old; the attestation window is a recorded period parameter, 10 minutes by
  default and a week on the devnet (item 10).
- **Stale-while-revalidate never refreshed over HTTP/2**: Pingora 0.9 replays the request as HTTP/1 text for the
  background refresh, but an HTTP/2 request comes out with `HTTP/2` in the request line and no Host header, and
  its own HTTP/1 parser refuses it, so the object stayed stale forever. The node now rewrites both before the
  cache runs (item 28). Worth reporting upstream to Pingora.
- **`make e2e` could not run twice on one devnet**: cached objects from the previous run turned the first MISS into
  a HIT. Each run now uses its own query string.
- **Dropped roots**: after a void rolls `lastPeriod` back, the aggregator's `/v1/previous` ignores the periods above
  it, so nobody stays sticky on a dropped period.
- **The countersigner refused honest roots**: `serde_json` parsed some floats one ulp off, so the previous period's
  state written to `prev.json` did not read back to the same numbers, and the countersigner's recomputation of it
  disagreed with the aggregator's. Both crates enable `float_roundtrip`, the aggregator computes from the inputs as
  re-read from disk, and a unit test round-trips the state.
- **The tampering origin served 404, not tampered bytes**: `deploy/origin/10-tamper.sh` was not executable, so the
  nginx image skipped it and Seoul's origin had no files. The old auditor counted the 404s as up (item 13), which
  hid it; with 4xx now a failure, Seoul lost uptime instead of integrity. The hook is executable now.

## Verification

Run on 2026-09-26 after every fix above, each devnet step on a fresh devnet (`make devnet-reset`):

| Check | Result |
|---|---|
| `make contracts-test` | 44 Foundry tests pass (27 before the audit) |
| `cargo test` per crate | core 58, aggregator 4, auditor 2, cp 5, node 4, resolver 2: all pass |
| Live devnet tests (`cargo test -p dedan-core -- --ignored`) | 3 pass: event replay, ENS resolution, the map mirror against the contract's medians |
| `make smoke`, `make demo`, `make e2e` | SMOKE OK, CDN ROUTING OK, E2E OK (e2e includes the new stale-while-revalidate step over HTTP/2) |
| `make worldid-check` | WORLD ID CHECK OK: one attested registration, then refusals for a second key of the same human, a Selfie Check credential, a proof naming another account and a reused request |
| `make demo-settlement` | SETTLEMENT DEMO OK on the first report (period 3): Seoul integrity 0 on tampered bytes seen by three auditors, Tokyo uptime 0, the spoofing attester (OutOfArea, 7,944 km) and the lying auditor (invalid tokens) in evidence and unpaid, RECOMPUTE OK, the claim refused inside the window and after the void, paid after republishing |


