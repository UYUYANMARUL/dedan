// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @title DNS wire-format records (RFC 1035) for the ENSIP-6 `setDNSRecords` profile.
/// @dev Internal library: inlined where used, nothing deployed. `dnsName` is the DNS-encoded owner name.
library LibDnsWire {
    function a(bytes memory dnsName, uint32 ttl, bytes4 ip) internal pure returns (bytes memory) {
        return abi.encodePacked(dnsName, uint16(1), uint16(1), ttl, uint16(4), ip);
    }

    function aaaa(bytes memory dnsName, uint32 ttl, bytes16 ip) internal pure returns (bytes memory) {
        return abi.encodePacked(dnsName, uint16(28), uint16(1), ttl, uint16(16), ip);
    }
}
