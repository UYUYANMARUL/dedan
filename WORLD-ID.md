# World ID in Dedan: one human, one vote

Dedan asks World ID one question, at one moment: **is the key about to receive a role held by a unique human who
holds no other key for that role?** This page explains that trust moment, why Proof of Human is the minimum
sufficient credential for it, how the verification runs (IDKit 4, the Developer Portal, the registry on chain),
the paths that end without a role, how to run it, and an integration debrief.

## The trust moment

Dedan's latency map decides which CDN node answers each city and which nodes a customer pays for. The map is
made of votes: a verified human measures the round trip from their location to the nodes of their H3 cell and
submits it on chain, and the contract keeps **one vote per human per node per cell** (the newest), taking medians
over those votes. Auditors, a second role, run the probes that decide node pay, and each auditor is paid per valid
probe.

Both only work if "one human" is true. With a thousand keys, one person could outvote every real attester in a
cell (steering a region's traffic to their own node) or collect a thousand auditor shares. So the product event
that needs trust is **granting a role**:

| Role | Action (IDKit) | What the role can do |
|---|---|---|
| Attester | `dedan-attester` | Vote round trips for the nodes of their cell, once a minute per cell |
| Auditor | `dedan-auditor` | Run assigned probes (with a 100 USDC stake) and be paid per valid probe |
| Node operator | `dedan-operator` | Register CDN nodes under `<label>.dedan.eth` (with a node stake) |

Each role is its own action, so its nullifiers are independent: one person may be an attester, an auditor and an
operator, each once. The nullifier is burned on chain in the registry (`nullifierUsed[action][nullifier]`), so
"once" holds whoever runs the verification.

## Why Proof of Human, and not the others

The property the map needs is **uniqueness**, nothing else. It does not need to know who someone is, their age
or their nationality, and it must not make an attester identifiable (their votes are public on chain).

| Credential | Proves | Fit for "one human, one vote" |
|---|---|---|
| **Proof of Human** (Orb, schema 1) | A unique, live human; no attributes, no data | **Yes, minimum sufficient.** The credential is exactly the uniqueness guarantee the vote needs and reveals nothing else. |
| Passport / NFC (9303) | A valid document and its attributes | No: one person can hold several documents (several passports, an ID card), so it is not a uniqueness guarantee across credentials, and it would tie votes to document holders. More assurance about identity, less about uniqueness, and more data than needed. |
| Selfie Check (11, beta) | A live face, with a Sybil *risk score* | No: medium assurance; its own documentation calls the score "a risk signal, not a uniqueness verdict". Good for low-stakes gating; not for votes that steer traffic and pay money. |

So Dedan requests only `proof_of_human` (IDKit `CredentialRequest("proof_of_human")`, `allow_legacy_proofs: false`),
and the registry refuses any proof whose issuer schema is not 1 (`NotProofOfHuman`), so a proof of a weaker
credential cannot be substituted on chain either.

## How it runs

```
dashboard (browser)                control plane (dedan-cp)            Developer Portal         registry (chain)
  1 choose role + account  ──────►  POST /v1/world-id/request
                                     RP signature (secp256k1, EIP-191,
                           ◄──────   nonce, 5 min expiry, the action)
  2 IDKit.request(rp_context)
    .constraints(proof_of_human,
     signal = account)
    QR / link ─► World App: consent
  3 pollUntilCompletion ─► result
  4 result ────────────────────────► POST /v1/world-id/verify
                                     checks: v4, action, environment,
                                     the nonce it issued, credential =
                                     proof_of_human (schema 1), signal =
                                     the account
                                     ───────────────────────────────────► POST /api/v4/verify/{rp_id}
                                     ◄─────────────────────────────────── success, nullifier
                                     nullifier not yet used on chain
                                     signs the proof's public inputs for
                           ◄──────   this chain, registry and account
  5 registerAttester(proof) ──────────────────────────────────────────────────────────────► gate checks the signature,
                                                                                              registry: schema 1,
                                                                                              burns the nullifier,
                                                                                              grants the role
```

- **Verification** happens at the Developer Portal (`developer.world.org/api/v4/verify/{rp_id}`, or
  `staging-developer.worldcoin.org` for staging), which checks the zero-knowledge proof. The control plane adds
  the checks the portal does not know about: the action matches the role, the proof is Proof of Human, the signal
  is the account that will receive the role, and the request nonce is one it signed and has not seen answered.
- **Uniqueness** is enforced on chain: the registry burns `(action, nullifier)`, so the same human cannot take a
  second key for the same role, whatever any server says.
- **On chain**, the registry calls an `IWorldIDVerifier`. On the devnet that is `WorldIdGate`: it accepts the
  control plane's signature over exactly the proof's public inputs (nullifier, action, RP id, nonce, the account's
  signal hash, expiry, schema), bound to this chain and this registry, valid for one hour. On World Chain the
  registry is pointed (`setWorldId`) at the real `WorldIDVerifier`
  (`0x00000000009E00F9FE82CfeeBB4556686da094d7`, Sepolia `0x703a6316c975DEabF30b637c155edD53e24657DB`), and the
  same IDKit proof is submitted as is: the registry's `WorldIdProof` is the v4 proof's shape (nullifier, nonce,
  `expires_at_min`, issuer schema, genesis minimum, five proof words).
- **The devnet's scripted humans** (five city auditors, a spoofer, a liar) cannot hold World IDs; they pass the
  gate through `fallbackVerifier` (the mock). Governance sets it to zero to leave only real proofs
  (`WorldIdGate.setFallback(0)`); nothing on the real path touches the mock.

## Paths that end without a role

| What happens | Where it is caught | What the person sees |
|---|---|---|
| They close the request or press Cancel | IDKit `pollUntilCompletion` is aborted, or World App returns `user_rejected` | "Cancelled: no role was granted" |
| Their World ID has no Proof of Human credential (never at an Orb) | World App returns `credential_unavailable` | "Attesting needs Proof of Human: verify at an Orb, then try again" |
| Nobody answers within three minutes, or the five-minute request expires | `pollUntilCompletion` returns `timeout`; World App returns `rp_signature_expired` | "No answer from World App" or "The request expired: start again" |
| Their World App is not on World ID 4.0 yet | `world_id_4_not_available` | "Update World App" |
| They already hold this role with another key | the registry's nullifier (checked by the control plane first) | "This human already holds the attester role: one key per human" |
| The proof is for another action, another environment, another account, or a weaker credential | the control plane's checks, then the gate and the registry | "Rejected: <reason>" |
| The Developer Portal rejects the proof | `POST /api/v4/verify` answers an error | "Rejected by World ID: <code>" |

## Running it

1. In the Developer Portal (<https://developer.world.org>) create an app, enable World ID 4.0, and note its
   `app_id`, `rp_id` and RP signing key. Use a **staging** app to test with the World ID Simulator
   (<https://simulator.worldcoin.org>), a production app for World App.
2. Put them in `deploy/devnet/world-id.env` (read by the control plane; keep the file private):
   ```
   DEDAN_WORLD_ID_APP_ID=app_...
   DEDAN_WORLD_ID_RP_ID=rp_...
   DEDAN_WORLD_ID_SIGNING_KEY=0x...
   DEDAN_WORLD_ID_ENV=staging
   ```
3. `make devnet-reset`, open the dashboard (<http://localhost:9090>), section **World ID**: pick a role and a
   devnet key, press **Verify with World ID**, and approve the request in World App (scan the QR) or in the
   simulator (paste the link). The role appears on chain, with the transaction.
4. Try the other paths: cancel; approve with a simulator identity that has no Orb credential; verify a second
   key with the same identity.

### Without a World ID app

`make worldid-check` runs on any running devnet with no Developer Portal app. It stands in only for the portal's
answer (`deploy/scripts/portal-stub.py` says "success" for any well-formed 4.0 result), so the zero-knowledge proof
is not checked; everything else is the real path. It writes a throwaway `world-id.env`, restarts the control plane,
and removes the file when it ends:

1. A Proof of Human result for devnet key 27 as attester: RP-signed request, the control plane's checks, its
   attestation, the gate, the registry; key 27 holds the role.
2. The same human (same nullifier) for key 28: refused, one key per human per role.
3. A Selfie Check result: refused, a role needs Proof of Human.
4. A result naming key 27 submitted for key 29: refused, the signal is the account.
5. A second answer to a request already answered: refused.

## Integration debrief

To be completed with the first live run; the observations below come from building the integration.

- **Time to first success:** _fill in from the first verified run (minutes from creating the app to the role on
  chain)._
- **Friction encountered:**
  - The on-chain v4 verifier exists (the IDKit types say the proof is "compatible with WorldIDVerifier.sol") but
    the integration guide documents only the Developer Portal endpoint; the contract's interface had to be taken
    from the protocol repository.
  - How the signal is hashed (a `0x` hex signal is hashed as bytes, anything else as UTF-8) is only visible in the
    SDK source (`@worldcoin/idkit-core/hashing`), yet a contract must reproduce it exactly.
  - The RP signature has no Rust implementation; the spec and its test vector on the signatures page made a
    faithful port possible (Dedan's backend is Rust), and the vector is what made it trustworthy.
  - The v4 verify response's relation to the request's signal and action is not documented: the backend has to
    check both itself.
- **Missing capability or documentation:** an on-chain verification guide for World ID 4.0 (contract interface,
  public inputs, how `rp_id` maps to the `uint64` the contract takes); a Rust crate for RP signing and signal
  hashing; a staging `WorldIDVerifier` a local chain can use.
- **The one improvement with the greatest impact:** documenting on-chain v4 verification end to end (the public
  inputs and the RP id encoding), so that apps like this one can drop the trusted verification step and let the
  chain check the proof.
