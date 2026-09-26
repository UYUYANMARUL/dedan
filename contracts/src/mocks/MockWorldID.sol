// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IWorldIDVerifier} from "../IWorldIDVerifier.sol";

/// Devnet stand-in for the World ID 4.0 verifier: accepts any proof whose nullifier is non-zero.
contract MockWorldID is IWorldIDVerifier {
    error InvalidProof();

    function verify(uint256 nullifier, uint256, uint64, uint256, uint256, uint64, uint64, uint256, uint256[5] calldata)
        external
        pure
    {
        if (nullifier == 0) revert InvalidProof();
    }
}
