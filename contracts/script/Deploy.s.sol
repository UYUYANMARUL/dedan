// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Script, console2} from "forge-std/Script.sol";
import {PermissionedRegistry} from "@ensdomains/contracts-v2/registry/PermissionedRegistry.sol";
import {IPermissionedRegistry} from "@ensdomains/contracts-v2/registry/interfaces/IPermissionedRegistry.sol";
import {IRegistry} from "@ensdomains/contracts-v2/registry/interfaces/IRegistry.sol";
import {RegistryRolesLib} from "@ensdomains/contracts-v2/registry/libraries/RegistryRolesLib.sol";
import {IContractNamer} from "@ensdomains/contracts-v2/reverse-registrar/interfaces/IContractNamer.sol";
import {UniversalResolverV2} from "@ensdomains/contracts-v2/universalResolver/UniversalResolverV2.sol";
import {LabelStore} from "@ensdomains/contracts-v2/utils/LabelStore.sol";
import {IGatewayProvider} from "@ens/contracts/ccipRead/IGatewayProvider.sol";
import {DedanCells} from "../src/DedanCells.sol";
import {DedanRegistry} from "../src/DedanRegistry.sol";
import {DedanResolver} from "../src/DedanResolver.sol";
import {IWorldIDVerifier} from "../src/IWorldIDVerifier.sol";
import {Settlement} from "../src/Settlement.sol";
import {StubGatewayProvider} from "../src/StubGatewayProvider.sol";
import {MockUSDC} from "../src/mocks/MockUSDC.sol";
import {MockWorldID} from "../src/mocks/MockWorldID.sol";

/// Devnet deployment: mock USDC and World ID, the ENSv2 tree (root → eth → dedan, the last one being Dedan's own
/// registry), the node resolver, the cells contract, the Universal Resolver, Settlement, minimum stakes, signer set, price table and
/// funded actors. Anvil runs with 60 accounts (30+ for cities added at runtime): 0 governance, 1 customer, 2-6 city node operators, 7 smoke operator,
/// 8 aggregator, 9 countersigner, 10 control plane, 12-16 city hot keys, 17 smoke hot key, 20-24 city attesters,
/// 25 smoke attester, 26 smoke auditor.
contract Deploy is Script {
    string constant MNEMONIC = "test test test test test test test test test test test junk";
    uint256 constant TLD_GOV_ROLES = RegistryRolesLib.ROLE_REGISTRAR | RegistryRolesLib.ROLE_REGISTRAR_ADMIN
        | RegistryRolesLib.ROLE_SET_PARENT | RegistryRolesLib.ROLE_SET_PARENT_ADMIN;
    uint256 constant LABEL_OWNER_ROLES = RegistryRolesLib.ROLE_SET_SUBREGISTRY | RegistryRolesLib.ROLE_SET_RESOLVER;

    MockUSDC usdc;
    MockWorldID worldId;
    LabelStore labelStore;
    PermissionedRegistry root;
    PermissionedRegistry eth;
    DedanRegistry dedan;
    DedanResolver resolver;
    DedanCells cells;
    UniversalResolverV2 ur;
    Settlement settlement;

    function run() external {
        uint256 govPk = vm.deriveKey(MNEMONIC, 0);
        address gov = vm.addr(govPk);
        address aggregator = vm.addr(vm.deriveKey(MNEMONIC, 8));
        address countersigner = vm.addr(vm.deriveKey(MNEMONIC, 9));
        address controlPlane = vm.addr(vm.deriveKey(MNEMONIC, 10));

        vm.startBroadcast(govPk);
        usdc = new MockUSDC();
        worldId = new MockWorldID();
        deployEns(gov);
        deploySettlement(gov, aggregator, countersigner);
        for (uint32 i = 0; i < 60; i++) {
            usdc.mint(vm.addr(vm.deriveKey(MNEMONIC, i)), 1_000_000e6);
        }
        vm.stopBroadcast();

        writeJson(gov, aggregator, countersigner, controlPlane);
        console2.log("rootRegistry      ", address(root));
        console2.log("dedanRegistry     ", address(dedan));
        console2.log("dedanResolver     ", address(resolver));
        console2.log("dedanCells        ", address(cells));
        console2.log("universalResolver ", address(ur));
        console2.log("settlement        ", address(settlement));
        console2.log("addresses written to contracts/deployments/devnet.json");
    }

    /// ENSv2: root → eth → dedan, the same tree canonical ENS has, so the names carry over unchanged.
    function deployEns(address gov) internal {
        labelStore = new LabelStore(IContractNamer(address(0)));
        root = new PermissionedRegistry(labelStore, gov, TLD_GOV_ROLES);
        eth = new PermissionedRegistry(labelStore, gov, TLD_GOV_ROLES);
        dedan = new DedanRegistry(labelStore, gov, usdc, IWorldIDVerifier(address(worldId)), 1, "dedan.eth");
        resolver = new DedanResolver(IPermissionedRegistry(address(dedan)), "dedan.eth");
        dedan.setDedanResolver(resolver);
        cells = new DedanCells(dedan);
        dedan.setCells(address(cells));
        root.register("eth", gov, IRegistry(address(eth)), address(0), LABEL_OWNER_ROLES, type(uint64).max);
        eth.setParent(IRegistry(address(root)), "eth");
        eth.register("dedan", gov, IRegistry(address(dedan)), address(0), LABEL_OWNER_ROLES, type(uint64).max);
        dedan.setParent(IRegistry(address(eth)), "dedan");
        ur = new UniversalResolverV2(
            IPermissionedRegistry(address(root)),
            IGatewayProvider(new StubGatewayProvider()),
            IContractNamer(address(0))
        );
        dedan.setMinStake(DedanRegistry.StakeKind.Node, 1_000e6);
        dedan.setMinStake(DedanRegistry.StakeKind.Auditor, 100e6);
        dedan.setMinStake(DedanRegistry.StakeKind.Resolver, 1_000e6);
    }

    function deploySettlement(address gov, address aggregator, address countersigner) internal {
        settlement = new Settlement(usdc, gov);
        address[] memory signers = new address[](2);
        signers[0] = aggregator;
        signers[1] = countersigner;
        settlement.setSignerSet(signers, 2);
        settlement.setPriceTable(1, keccak256("dedan-price-table-v1"));
    }

    function writeJson(address gov, address aggregator, address countersigner, address controlPlane) internal {
        string memory j = "devnet";
        vm.serializeUint(j, "chainId", block.chainid);
        vm.serializeAddress(j, "usdc", address(usdc));
        vm.serializeAddress(j, "worldId", address(worldId));
        vm.serializeAddress(j, "labelStore", address(labelStore));
        vm.serializeAddress(j, "rootRegistry", address(root));
        vm.serializeAddress(j, "ethRegistry", address(eth));
        vm.serializeAddress(j, "dedanRegistry", address(dedan));
        vm.serializeAddress(j, "dedanResolver", address(resolver));
        vm.serializeAddress(j, "dedanCells", address(cells));
        vm.serializeAddress(j, "universalResolver", address(ur));
        vm.serializeAddress(j, "governance", gov);
        vm.serializeAddress(j, "aggregator", aggregator);
        vm.serializeAddress(j, "countersigner", countersigner);
        vm.serializeAddress(j, "controlPlane", controlPlane);
        string memory out = vm.serializeAddress(j, "settlement", address(settlement));
        vm.writeJson(out, "deployments/devnet.json");
    }
}
