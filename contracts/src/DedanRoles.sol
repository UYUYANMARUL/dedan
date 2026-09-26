// SPDX-License-Identifier: MIT
pragma solidity 0.8.25;

import {RegistryRolesLib} from "@ensdomains/contracts-v2/registry/libraries/RegistryRolesLib.sol";

/// @title Dedan's roles in ENSv2's Enhanced Access Control.
/// @dev One nybble per role; `RegistryRolesLib` uses nybbles 0-9, 30 and 31, so Dedan takes 10-17.
///      EAC counts assignees per role in that same nybble (15 at most per resource), which is why the
///      roles a whole population holds (operator, attester, auditor) are granted on a per-human resource,
///      one holder each, while node-scoped and governance roles sit where ENS puts them.
library DedanRoles {
    /// @dev Human resource: may register nodes. Granted after World ID verification.
    uint256 internal constant ROLE_NODE_OPERATOR = 1 << 40;
    /// @dev Human resource: may attest round trips to the contract. Granted after World ID verification.
    uint256 internal constant ROLE_ATTESTER = 1 << 44;
    /// @dev Human resource: may submit assigned probe results (needs the auditor stake). World ID verified.
    uint256 internal constant ROLE_AUDITOR = 1 << 48;
    /// @dev Root: an NS operator, granted by governance after the operator staked.
    uint256 internal constant ROLE_RESOLVER_OPERATOR = 1 << 52;
    uint256 internal constant ROLE_RESOLVER_OPERATOR_ADMIN = ROLE_RESOLVER_OPERATOR << 128;
    /// @dev Node resource: may sign envelopes, heartbeats and key reveals for that node.
    uint256 internal constant ROLE_NODE_SIGNER = 1 << 56;
    uint256 internal constant ROLE_NODE_SIGNER_ADMIN = ROLE_NODE_SIGNER << 128;
    /// @dev Root, governance: suspend nodes and accounts on published evidence.
    uint256 internal constant ROLE_SUSPEND = 1 << 60;
    uint256 internal constant ROLE_SUSPEND_ADMIN = ROLE_SUSPEND << 128;
    /// @dev Root, governance: slash stakes on published evidence.
    uint256 internal constant ROLE_SLASH = 1 << 64;
    uint256 internal constant ROLE_SLASH_ADMIN = ROLE_SLASH << 128;
    /// @dev Root, governance: parameters (minimum stakes, World ID verifier, resolver contract).
    uint256 internal constant ROLE_CONFIGURE = 1 << 68;
    uint256 internal constant ROLE_CONFIGURE_ADMIN = ROLE_CONFIGURE << 128;

    /// @dev What a node owner gets on its name: change the resolver, hand out signer keys, transfer.
    uint256 internal constant OPERATOR_BITMAP =
        RegistryRolesLib.ROLE_SET_RESOLVER | ROLE_NODE_SIGNER_ADMIN | RegistryRolesLib.ROLE_CAN_TRANSFER_ADMIN;

    /// @dev What governance gets on the root resource at deployment.
    uint256 internal constant GOVERNANCE_BITMAP = ROLE_SUSPEND | ROLE_SUSPEND_ADMIN | ROLE_SLASH | ROLE_SLASH_ADMIN
        | ROLE_CONFIGURE | ROLE_CONFIGURE_ADMIN | ROLE_RESOLVER_OPERATOR_ADMIN | RegistryRolesLib.ROLE_SET_PARENT
        | RegistryRolesLib.ROLE_SET_PARENT_ADMIN | RegistryRolesLib.ROLE_SET_URI | RegistryRolesLib.ROLE_SET_URI_ADMIN;

    /// @dev The EAC resource that scopes one human's roles: never a token, one holder per role.
    function humanResource(address account) internal pure returns (uint256) {
        return uint256(keccak256(abi.encodePacked("dedan:human", account)));
    }
}
