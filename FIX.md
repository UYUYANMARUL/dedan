# FIX.md — problems found in the 2026-09-26 audit

Audit of `ARCHITECTURE.md` (v3.2) and `PLAN.md` against the code. When it ran, everything was green:
`make contracts-test` (27), `make core-test` (46 + 3 live), `make e2e`, and `make demo-settlement` on a fresh devnet.
The devnet never reaches the cases below. Items were found by three read-only reviews (contracts and their Rust
mirror; map, assignment, DNS, audits and settlement; node, control plane, TLS and devnet) and re-read in the code
unless marked (review). Items `PLAN.md` already lists as "Not built (M4)" or as deliberate deviations are left out.

Each entry: what is wrong, where, what breaks, the fix, the check. **Status** is updated as fixes land.

## Fix first: availability, money, trust boundaries

1. **A repeated line in a customer's asset list hangs settlement for everyone.** `core/src/schedule.rs:27` loops
   until `n.min(manifest.len())` unique picks while skipping duplicates; lists are never deduplicated. The period
   close and every auditor's sync hang. Fix: dedupe, bound by the unique count. Check: unit test `["a","a"]`.
   **Status:** open
2. **One node's bad resolver stalls every resolver, auditor and the aggregator.** `core/src/ens.rs:134/152/165`
   `abi_decode(&raw)?`, and `node_records` fails on the first node (`:204`); operators hold `ROLE_SET_RESOLVER`.
   Fix: skip and log a node whose records do not decode. Check: unit test with undecodable bytes.
   **Status:** open
3. **The countersigner signs whatever the aggregator put in `nodes.json` and `prev.json`.** It recomputes from the
   aggregator's own files and checks only registry events, period number and digest (`aggregator/src/main.rs:376-410`).
   Payees, previous leaves, the clock and the origin digests are trusted. Fix: the countersigner re-reads records and
   accounts at `registry_block`, checks the previous period against its on-chain root, the clock against the block,
   the digests against the origin, and batch times and nonces. Check: tamper each file, expect a refusal.
   **Status:** open
4. **Signer bonds are optional.** `publishRoot` checks only `isSigner` and the threshold (`Settlement.sol:240-252`);
   `bonds` is never read and the deploy deposits none. ARCHITECTURE.md:373 requires a bond. Fix: count only bonded
   signers; deposit the bonds in `Deploy.s.sol`. Check: Foundry test, unbonded signature rejected.
   **Status:** open
5. **Bills are not bounded per account and balances are never enforced.** `totalDeposited` never drops on withdraw
   (`Settlement.sol:126`) and an over-bill is silently capped (`:174`), so other customers' deposits pay it; nothing
   off chain checks the effective balance or the four-day rule (ARCHITECTURE.md:337, :600-601). Fix: count
   withdrawals in the bound, cap each bill at the account's effective balance, suspend accounts under four periods
   of list price at the resolver. Check: unit and Foundry tests.
   **Status:** open
6. **Account ids are first come, first served.** `Settlement.createAccount` is permissionless (`:115-120`), and the
   control plane trusts `accounts(id).key` for purges (ARCHITECTURE.md:410: "label reserved"). Fix: creation needs
   the registrar's (control plane's) signature over the id and key. Check: Foundry test.
   **Status:** open
7. **A root cannot be voided once the next one is published.** `voidRoot` needs `period == lastPeriod`
   (`Settlement.sol:259`); `publishRoot` accepts p+1 over a voided p. Fix: void any root inside its own window,
   which drops the roots published after it; refuse p+1 while p is voided. Check: Foundry test.
   **Status:** open
8. **Accounts never billed can never withdraw.** After the first final root `_applyBill` needs a leaf
   (`Settlement.sol:163-170`); leaves exist only for billed accounts (`core/src/score.rs:587-591`). Fix: a bill leaf
   for every account on chain. Check: unit test.
   **Status:** open
9. **Anyone can make real cells unjoinable.** `LibCell.label` keeps the low 15 nibbles and `join` never validates ids
   (`DedanCells.sol:87-100`): joining `X | 1<<60` takes X's name and every later `join(_, [X])` reverts. Fix: accept
   only valid resolution-2 H3 indexes. Check: Foundry test.
   **Status:** open
10. **Fetch tokens are not bound to time and can be reused.** Only the HMAC for the token's hour is checked
    (`core/src/map.rs:150-169`, `aggregator/src/settle.rs:297`); one real fetch backs unlimited "up" results. Fix:
    the token's minute must match the result's time; a token counts once per auditor. Check: unit test.
    **Status:** open
11. **Key reveals: any hour, any number of times, the last one wins.** `revealKey` checks only the signer
    (`DedanRegistry.sol:266-270`); `RevealSet::add` overwrites (`map.rs:136-138`); a missing key is scored as the
    auditor's forgery. Fix: one reveal per node and hour, after the hour (a governance flag allows early reveals on
    the devnet); the mirror keeps the first; tokens whose key is missing are unverifiable, not forged.
    **Status:** open
12. **One colluding human can decide a median.** With two votes the lower middle is the lie
    (`DedanCells.sol:184-188`, `map.rs:647-652`), and k is checked only on the best-voted member (`map.rs:494-499`).
    Fix: rank only members with at least k votes. Check: unit test.
    **Status:** open
13. **The auditor counts 4xx as up.** `auditor/src/main.rs:361-365` marks any status below 500 ok, and the node puts a
    token on 421 and 404 replies. Fix: ok only for 2xx and 304; 4xx is attributed like 5xx. Check: unit test.
    **Status:** open
14. **Landing failures have no outlier check.** A failed landing needs no token; one auditor inventing a few connect
    failures halves a bill (ARCHITECTURE.md:308). Fix: an auditor whose failure rate for a target exceeds the
    others' by a wide margin is dropped for the period, unpaid, and named in the evidence. Check: unit test. (review)
    **Status:** open
15. **Eligibility uses the class the operator declares.** `core/src/config.rs:176` feeds the `dedan.class` record into
    eligibility and the admission cap; declaring 1G raises the cap tenfold. Fix: 100M for everyone until class
    probes exist. **Status:** open
16. **Suspension, slashing and bonds do not hold.** Unbond and withdraw ignore suspension and `withdraw` clears it
    (`DedanRegistry.sol:241-263`); a slashed node stays active; `isResolverOperator` ignores the bond. Fix: no unbond
    while suspended; active nodes and resolvers need the minimum stake. Check: Foundry test.
    **Status:** open
17. **The AAAA record of `<label>.node` skips the endpoint challenge** (`resolver/src/dns.rs:237-245`). Fix: challenge
    IPv6 addresses too. **Status:** open
18. **Configs are signed by the control plane, so it can reroute any customer.** `cp/src/main.rs:203` signs as `cp`;
    nodes and the resolver accept only that key. ARCHITECTURE.md:388, :169, :208 say account-signed. Fix: the
    customer signs its config with the account key, the control plane relays it, every reader verifies the key on
    chain. Check: e2e step 1 shows the account key as signer. **Status:** open
19. **Configs have no version or replay check** (`node/src/control.rs:23`, `resolver/src/state.rs:107`); an older
    signed config replayed rolls routing back. Fix: keep the highest version per account. Check: unit test.
    **Status:** open
20. **No object-size cap on the in-memory cache**; one huge origin object is buffered in memory (ARCHITECTURE.md:151).
    Fix: the account's `max_object_bytes` as Pingora's max file size. **Status:** open
21. **RPC errors read as "no record" and change pay.** `ens.rs:122-124` maps every call error to `Ok(None)`. Fix:
    only a JSON-RPC error answer means "none"; transport errors fail the read. **Status:** open
22. **Alias records fall back to text the operator sets.** `DedanResolver.text` returns the identity's own
    `dedan.latency`/`dedan.votes` when a cell has no votes (`DedanResolver.sol:60-68`). Fix: no fall-through for the
    keys DedanCells owns. Check: Foundry test. **Status:** open

## Correctness

23. **The map mirror ignores cell membership.** `Map::from_votes` never uses `_members` (`map.rs:381`); a node that
    left a cell stays ranked there for two days, unlike the contract's `snapshot`. The contract's cell record also
    still answers `dedan.latency.<label>` after `leave`. Fix: members only, both sides. Check: unit test with leave.
    **Status:** open
24. **Resolver, auditors and aggregator compute different slot sets.** The resolver is sticky on its own snapshot of
    10 s ago with quality 1.0, the auditor has no previous day, the aggregator uses real quality. Fix: all three take
    the previous period from the aggregator's published outputs. **Status:** open
25. **Kept members skip the /24 and admission checks** (`core/src/assign.rs:100-122`). Fix: the same checks as new
    members. Check: unit test. **Status:** open
26. **`withdraw` leaves the node's cell state behind.** Nobody can `leave` for a removed name and the next registrant
    of the label inherits memberships and votes. Fix: the registry removes the node from its cells on withdrawal, and
    a removed label cannot be registered again until its votes aged out. **Status:** open
27. **Code defaults are devnet values**: period 3600 s, grace 10 s, map k 1 (documented: daily, 1 h, 3); the
    attestation pay cap counts per period, not per epoch. Fix: documented defaults in code, devnet values in compose.
    **Status:** open
28. **Stale-while-revalidate never serves stale.** The node does not override `should_serve_stale`, whose Pingora
    default refuses the revalidation case. Fix: serve stale while revalidating. Check: an e2e step. **Status:** open
29. **The event stream has no replay.** The control plane ignores `Last-Event-ID` and drops lagged events; the node
    never sends it; a purge sent during a reconnect is lost (ARCHITECTURE.md:427). Fix: replay from a bounded log;
    a gap too old for the log makes the node purge everything. **Status:** open
30. **A voided period is republished unchanged at the next close**, so a void only delays. Fix: no automatic
    republication of a voided period; an operator republishes by hand after the dispute. **Status:** open
31. **The auditor reports its interface's /24, not the one the node saw** (`auditor/src/main.rs:481`); behind NAT
    every token fails. Fix: the token carries the prefix the node saw. **Status:** open
32. **Two names claiming one IP are both answered** (ARCHITECTURE.md:256 says neither). (review) **Status:** open

## Gaps against the docs

33. **The schedule never assigns auditors to accounts**; every auditor probes every account (ARCHITECTURE.md:298).
    (review) **Status:** open
34. **`node_policy` (operator denylist, allowlist, minimum stake) is hardcoded empty** (`config.rs:79-81`).
    **Status:** open
35. **The on-chain price table does not constrain prices**: only a non-zero hash is checked (`Settlement.sol:201`);
    prices come from `accounts.json`. **Status:** open
36. **The node ignores `cache_rules`** (per-account TTL, query-string policy, Vary); the TTL comes from an env var.
    **Status:** open
37. **Certificates are ordered without the `shortlived` profile or ARI `replaces`** (`cp/src/main.rs:290`), so Pebble
    issues 90-day certificates; ARCHITECTURE.md:436-439 says 160 h and ARI renewal. **Status:** open

## Docs out of date

38. ARCHITECTURE.md header says v3/v3.1; §1 and the diagram send attestations to the registry, an hourly map from the
    aggregator and keys to the aggregator; §2 puts `attest` on the registry and has no DedanCells row. **Status:** open
39. Resolution 5 / 17 km / "r = 1..2" / "ring search to 2" (ARCHITECTURE.md:140, :223, :285, :509, :529, :583) vs
    resolution 2 and three rings in code and PLAN. **Status:** open
40. `attest` on DedanRegistry in ARCHITECTURE.md:275, :305 and PLAN.md:120; PLAN's workspace layout lacks DedanCells,
    LibCell, `crates/node`, `crates/cp`. **Status:** open
41. PLAN.md status and decisions: "njs" nodes, 40 + 2 core tests, no node or cp crate, OpenSSL and disk storage.
    **Status:** open
42. PLAN.md storyline, risks and M2 text: spoof "rejected" and "from the Tokyo cell", script-revealed keys,
    keys.json, "nameserver without a previous period", per-node cap, Seoul via `proxy_pass`. **Status:** open
43. ARCHITECTURE.md:239/:250 say unmeasured nodes are never chosen; code and PLAN use them last. **Status:** open
44. ARCHITECTURE.md:143 orders cold starts by the map's estimate; code and PLAN use the declared location. **Status:** open
45. The recompute contract is described as stronger than built (ARCHITECTURE.md:498, PLAN.md:188). **Status:** open
46. The endpoint challenge runs over `http://<ip>:<port>` without SNI; ARCHITECTURE.md:256 says HTTPS with SNI.
    **Status:** open
47. ACME TXT answers carry TTL 0; ARCHITECTURE.md:94 says 60 s. **Status:** open
48. `demo-settlement.sh:57` says the spoofer is "9,000 km away"; the report says 7,944 km. **Status:** open

## Design questions

49. A node that served tampered bytes still counts as delivered, so the customer pays for that slot.
    **Status:** open
50. Ring search depth (2 in ARCHITECTURE v3.2, 3 in code) and the median of an even count. **Status:** open

## Tests missing

- Foundry: `registerCell`'s `NotCells` guard, a duplicate node in one batch, leave then rejoin, attest by a
  suspended attester, invalid cell ids, voiding a non-latest root.
- Rust: `crates/cp` and `crates/auditor` have no unit tests, `crates/resolver` has one; `make e2e` has no SWR step.

## Known M4 items with security impact (already on PLAN's list, kept visible)

- `/set-txt` and `/clear-txt` at the resolver are unauthenticated until control-plane-signed TXT lands
  (`resolver/src/api.rs:188-208`): anyone who reaches its HTTP port can publish `_acme-challenge` for any account.
