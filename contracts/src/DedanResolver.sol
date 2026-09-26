// SPDX-License-Identifier: MIT
pragma solidity 0.8.25;

import {Multicallable} from "@ens/contracts/resolvers/Multicallable.sol";
import {AddrResolver} from "@ens/contracts/resolvers/profiles/AddrResolver.sol";
import {DNSResolver} from "@ens/contracts/resolvers/profiles/DNSResolver.sol";
import {TextResolver} from "@ens/contracts/resolvers/profiles/TextResolver.sol";
import {NameCoder} from "@ens/contracts/utils/NameCoder.sol";
import {IPermissionedRegistry} from "@ensdomains/contracts-v2/registry/interfaces/IPermissionedRegistry.sol";

/// @dev What the node resolver asks the cells contract about alias names.
interface IDedanCellsRecords {
    function aliasOf(bytes32 node) external view returns (uint64 cell, uint256 labelId);
    function text(bytes32 node, string calldata key) external view returns (string memory);
}

/// @title The resolver of every Dedan node name.
/// @notice Standard ENS record profiles: `addr` (ENSIP-9, the payout address), `text` (ENSIP-5, the
///         `dedan.*` config keys) and DNS records (ENSIP-6, the node's A/AAAA records), plus `multicall`.
///         A record may be changed by the name's current owner in the Dedan registry, or by the registry
///         itself, which writes the initial records at registration. The name's namehash is bound to its
///         registry label at registration so owner lookups need no reverse mapping.
///         The alias names `<label>.<cell>.dedan.eth` (a node inside a cell it joined) resolve here too: the
///         cell's records for the node come from the cells contract only, every other key from the node's own name.
contract DedanResolver is Multicallable, AddrResolver, TextResolver, DNSResolver {
    IPermissionedRegistry public immutable REGISTRY;
    /// @notice namehash of the parent name, e.g. `dedan.eth`.
    bytes32 public immutable PARENT_NODE;
    IDedanCellsRecords public cells;

    /// @notice namehash → registry label id (`uint256(keccak256(label))`).
    mapping(bytes32 node => uint256 labelId) public labelOf;

    error NotRegistry();

    constructor(IPermissionedRegistry registry, string memory parentName) {
        REGISTRY = registry;
        PARENT_NODE = NameCoder.namehash(NameCoder.encode(parentName), 0);
    }

    /// @notice Called by the registry when a name is registered.
    function bind(bytes32 node, uint256 labelId) external {
        if (msg.sender != address(REGISTRY)) revert NotRegistry();
        labelOf[node] = labelId;
    }

    /// @notice Called by the registry when governance sets the cells contract.
    function setCells(address cells_) external {
        if (msg.sender != address(REGISTRY)) revert NotRegistry();
        cells = IDedanCellsRecords(cells_);
    }

    /// @dev The node name an alias name stands for, or zero when `node` is not an alias.
    function _identityOf(bytes32 node) internal view returns (bytes32) {
        if (address(cells) == address(0)) return bytes32(0);
        (, uint256 labelId) = cells.aliasOf(node);
        return labelId == 0 ? bytes32(0) : NameCoder.namehash(PARENT_NODE, bytes32(labelId));
    }

    /// @notice On an alias name, the keys the cells contract owns (`dedan.latency`, `dedan.votes`, `dedan.cell`,
    ///         `dedan.node`) come from it alone, even when empty; every other key from the node's own name.
    function text(bytes32 node, string calldata key) external view override returns (string memory) {
        string memory own = versionable_texts[recordVersions[node]][node][key];
        if (bytes(own).length != 0 || labelOf[node] != 0) return own;
        bytes32 identity = _identityOf(node);
        if (identity == bytes32(0)) return own;
        bytes32 k = keccak256(bytes(key));
        if (
            k == keccak256("dedan.latency") || k == keccak256("dedan.votes") || k == keccak256("dedan.cell")
                || k == keccak256("dedan.node")
        ) return cells.text(node, key);
        return versionable_texts[recordVersions[identity]][identity][key];
    }

    function addr(bytes32 node) public view override returns (address payable) {
        address payable own = super.addr(node);
        if (own != address(0) || labelOf[node] != 0) return own;
        bytes32 identity = _identityOf(node);
        return identity == bytes32(0) ? own : super.addr(identity);
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
