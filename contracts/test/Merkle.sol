// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// OpenZeppelin StandardMerkleTree hashing, for tests and scripts: double-hashed leaves, sorted pairs.
library Merkle {
    function leaf(bytes memory encoded) internal pure returns (bytes32) {
        return keccak256(bytes.concat(keccak256(encoded)));
    }

    function pair(bytes32 a, bytes32 b) internal pure returns (bytes32) {
        return a < b ? keccak256(abi.encodePacked(a, b)) : keccak256(abi.encodePacked(b, a));
    }
}
