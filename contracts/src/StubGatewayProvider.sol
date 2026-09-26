// SPDX-License-Identifier: MIT
pragma solidity 0.8.25;

import {IGatewayProvider} from "@ens/contracts/ccipRead/IGatewayProvider.sol";

/// @notice The Universal Resolver wants a batch-gateway provider for CCIP-Read; Dedan keeps every record on
///         chain, so there are no gateways.
contract StubGatewayProvider is IGatewayProvider {
    function gateways() external pure returns (string[] memory) {
        return new string[](0);
    }
}
