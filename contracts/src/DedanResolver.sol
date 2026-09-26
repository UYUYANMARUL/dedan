// SPDX-License-Identifier: MIT
pragma solidity 0.8.25;

import {Multicallable} from "@ens/contracts/resolvers/Multicallable.sol";
import {AddrResolver} from "@ens/contracts/resolvers/profiles/AddrResolver.sol";
import {DNSResolver} from "@ens/contracts/resolvers/profiles/DNSResolver.sol";
import {TextResolver} from "@ens/contracts/resolvers/profiles/TextResolver.sol";
import {IPermissionedRegistry} from "@ensdomains/contracts-v2/registry/interfaces/IPermissionedRegistry.sol";

/// @title The resolver of every Dedan node name.
/// @notice Standard ENS record profiles: `addr` (ENSIP-9, the payout address), `text` (ENSIP-5, the
///         `dedan.*` config keys) and DNS records (ENSIP-6, the node's A/AAAA records), plus `multicall`.
///         A record may be changed by the name's current owner in the Dedan registry, or by the registry
///         itself, which writes the initial records at registration. The name's namehash is bound to its
///         registry label at registration so owner lookups need no reverse mapping.
contract DedanResolver is Multicallable, AddrResolver, TextResolver, DNSResolver {
    IPermissionedRegistry public immutable REGISTRY;

    /// @notice namehash → registry label id (`uint256(keccak256(label))`).
    mapping(bytes32 node => uint256 labelId) public labelOf;

    error NotRegistry();

    constructor(IPermissionedRegistry registry) {
        REGISTRY = registry;
    }

    /// @notice Called by the registry when a name is registered.
    function bind(bytes32 node, uint256 labelId) external {
        if (msg.sender != address(REGISTRY)) revert NotRegistry();
        labelOf[node] = labelId;
    }

    function isAuthorised(bytes32 node) internal view override returns (bool) {
        if (msg.sender == address(REGISTRY)) return true;
        uint256 labelId = labelOf[node];
        return labelId != 0 && REGISTRY.getOwner(labelId) == msg.sender;
    }

    function supportsInterface(bytes4 interfaceId)
        public
        view
        override(Multicallable, AddrResolver, TextResolver, DNSResolver)
        returns (bool)
    {
        return super.supportsInterface(interfaceId);
    }
}
