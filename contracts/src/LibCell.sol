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

    /// @dev True for a valid H3 cell index at resolution 2: bit 63 clear, mode 1 (cell), reserved bits 56-58 clear,
    ///      resolution 2, base cell below 122, digits 1 and 2 at most 6, digits 3-15 unused (all ones). The top 12
    ///      bits are therefore 0x082. The pentagon rule (under a pentagon base cell the first non-zero digit is never
    ///      1) is not checked: such an id names no real place, but its label is still its own, so it cannot take a
    ///      real cell's name.
    function isCell(uint64 cell) internal pure returns (bool) {
        return (cell >> 52) == 0x082 && ((cell >> 45) & 0x7f) < 122 && ((cell >> 42) & 7) <= 6
            && ((cell >> 39) & 7) <= 6 && (cell & 0x7fffffffff) == 0x7fffffffff;
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
