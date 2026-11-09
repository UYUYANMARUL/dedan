// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {IWorldIDVerifier} from "./IWorldIDVerifier.sol";

/// @title World ID 4.0 proofs verified by the Developer Portal, attested on chain.
/// @notice The registry calls `verify` with the proof's public inputs. On World Chain it can point at the real
///         `WorldIDVerifier` instead; this gate is for chains without it (the devnet): Dedan's control plane
///         (the registrar) verifies the IDKit result at `developer.world.org/api/v4/verify`, checks the action,
///         the environment, the credential (Proof of Human) and the signal (the account), and signs exactly the
///         public inputs the registry will pass, bound to this chain and that registry, with a deadline. The
///         `zeroKnowledgeProof` words carry that signature: `[r, s, v, deadline, 0]`.
///         `fallbackVerifier` (devnet only) keeps the scripted humans, who hold no World ID, working.
contract WorldIdGate is IWorldIDVerifier, Ownable {
    address public registrar;
    IWorldIDVerifier public fallbackVerifier;

    event RegistrarSet(address registrar);
    event FallbackSet(address verifier);

    error NotAttested();
    error AttestationExpired();

    constructor(address owner_, address registrar_, IWorldIDVerifier fallback_) Ownable(owner_) {
        registrar = registrar_;
        fallbackVerifier = fallback_;
    }

    function setRegistrar(address registrar_) external onlyOwner {
        registrar = registrar_;
        emit RegistrarSet(registrar_);
    }

    /// @notice Zero turns the fallback off: only attested proofs pass.
    function setFallback(IWorldIDVerifier verifier) external onlyOwner {
        fallbackVerifier = verifier;
        emit FallbackSet(address(verifier));
    }

    /// @notice What the registrar signs (EIP-191), for a proof the registry at `registry` will pass.
    function attestationDigest(
        address registry,
        uint256 nullifier,
        uint256 action,
        uint64 rpId,
        uint256 nonce,
        uint256 signalHash,
        uint64 expiresAtMin,
        uint64 issuerSchemaId,
        uint256 credentialGenesisIssuedAtMin,
        uint256 deadline
    ) public view returns (bytes32) {
        return keccak256(
            abi.encode(
                block.chainid,
                registry,
                nullifier,
                action,
                rpId,
                nonce,
                signalHash,
                expiresAtMin,
                issuerSchemaId,
                credentialGenesisIssuedAtMin,
                deadline
            )
        );
    }

    function verify(
        uint256 nullifier,
        uint256 action,
        uint64 rpId,
        uint256 nonce,
        uint256 signalHash,
        uint64 expiresAtMin,
        uint64 issuerSchemaId,
        uint256 credentialGenesisIssuedAtMin,
        uint256[5] calldata zeroKnowledgeProof
    ) external view {
        if (zeroKnowledgeProof[4] == 0 && zeroKnowledgeProof[2] >= 27 && zeroKnowledgeProof[2] <= 28) {
            bytes32 d = attestationDigest(
                msg.sender,
                nullifier,
                action,
                rpId,
                nonce,
                signalHash,
                expiresAtMin,
                issuerSchemaId,
                credentialGenesisIssuedAtMin,
                zeroKnowledgeProof[3]
            );
            (address signer, ECDSA.RecoverError err,) = ECDSA.tryRecover(
                MessageHashUtils.toEthSignedMessageHash(d),
                uint8(zeroKnowledgeProof[2]),
                bytes32(zeroKnowledgeProof[0]),
                bytes32(zeroKnowledgeProof[1])
            );
            if (err == ECDSA.RecoverError.NoError && signer == registrar && signer != address(0)) {
                if (block.timestamp > zeroKnowledgeProof[3]) revert AttestationExpired();
                return;
            }
        }
        if (address(fallbackVerifier) == address(0)) revert NotAttested();
        fallbackVerifier.verify(
            nullifier,
            action,
            rpId,
            nonce,
            signalHash,
            expiresAtMin,
            issuerSchemaId,
            credentialGenesisIssuedAtMin,
            zeroKnowledgeProof
        );
    }
}
