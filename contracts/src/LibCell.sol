// SPDX-License-Identifier: MIT
pragma solidity 0.8.25;

/// @title The ENS label of an H3 cell: its index in lowercase hex, 15 characters (the canonical H3 string form,
///        which drops the leading zero nibble), e.g. `821f87fffffffff`. Internal library, inlined where used.
library LibCell {
    bytes16 private constant HEX = "0123456789abcdef";

    function label(uint64 cell) internal pure returns (string memory) {
        bytes memory out = new bytes(15);
        for (uint256 i; i < 15; ++i) {
            out[i] = HEX[(cell >> (4 * (14 - i))) & 0xf];
        }
        return string(out);
    }

    /// @dev True for strings that could name a cell: exactly 15 lowercase hex characters.
    function isCellLabel(string memory s) internal pure returns (bool) {
        bytes memory b = bytes(s);
        if (b.length != 15) return false;
        for (uint256 i; i < 15; ++i) {
            bytes1 c = b[i];
            if (!((c >= "0" && c <= "9") || (c >= "a" && c <= "f"))) return false;
        }
        return true;
    }
}
