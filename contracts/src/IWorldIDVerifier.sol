// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// World ID 4.0 on-chain verifier. Reverts on an invalid proof.
/// World Chain mainnet: 0x00000000009E00F9FE82CfeeBB4556686da094d7
/// World Chain Sepolia: 0x703a6316c975DEabF30b637c155edD53e24657DB
/// The devnet substitutes MockWorldID.
interface IWorldIDVerifier {
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
    ) external view;
}
