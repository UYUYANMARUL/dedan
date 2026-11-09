// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Script, console2} from "forge-std/Script.sol";
import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
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
import {WorldIdGate} from "../src/WorldIdGate.sol";
import {StubGatewayProvider} from "../src/StubGatewayProvider.sol";
import {MockUSDC} from "../src/mocks/MockUSDC.sol";
import {MockWorldID} from "../src/mocks/MockWorldID.sol";

/// Devnet deployment: mock USDC and World ID, the ENSv2 tree (root → eth → dedan, the last one being Dedan's own
/// registry), the node resolver, the cells contract, the Universal Resolver, Settlement, minimum stakes, early key
/// reveals, signer set with bonds, the account registrar, the price of the demo tier and funded actors, then the demo
/// customer's account `acme` with 10,000 USDC. Anvil runs with 60 accounts (30+ for cities added at runtime):
/// 0 governance, 1 customer, 2-6 city node operators, 7 smoke operator, 8 aggregator, 9 countersigner,
/// 10 control plane (the registrar), 12-16 city hot keys, 17 smoke hot key, 20-24 city attesters, 25 smoke attester,
/// 26 smoke auditor.
contract Deploy is Script {
    string constant MNEMONIC = "test test test test test test test test test test test junk";
    uint256 constant TLD_GOV_ROLES = RegistryRolesLib.ROLE_REGISTRAR | RegistryRolesLib.ROLE_REGISTRAR_ADMIN
        | RegistryRolesLib.ROLE_SET_PARENT | RegistryRolesLib.ROLE_SET_PARENT_ADMIN;
    uint256 constant LABEL_OWNER_ROLES = RegistryRolesLib.ROLE_SET_SUBREGISTRY | RegistryRolesLib.ROLE_SET_RESOLVER;
    uint256 constant SIGNER_BOND = 20_000e6;
    bytes32 constant ACME = bytes32("acme");

    MockUSDC usdc;
    MockWorldID worldId;
    WorldIdGate worldIdGate;
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
        uint256 aggPk = vm.deriveKey(MNEMONIC, 8);
        uint256 cosPk = vm.deriveKey(MNEMONIC, 9);
        uint256 cpPk = vm.deriveKey(MNEMONIC, 10);
        address aggregator = vm.addr(aggPk);
        address countersigner = vm.addr(cosPk);
        address controlPlane = vm.addr(cpPk);

        vm.startBroadcast(govPk);
        usdc = new MockUSDC();
        worldId = new MockWorldID();
        // real humans: IDKit proofs the control plane verified at the Developer Portal and attests (WORLD-ID.md);
        // the mock stays as the fallback for the devnet's scripted humans, who hold no World ID
        worldIdGate = new WorldIdGate(gov, controlPlane, worldId);
        deployEns(gov);
        deploySettlement(gov, aggregator, countersigner, controlPlane);
        for (uint32 i = 0; i < 60; i++) {
            usdc.mint(vm.addr(vm.deriveKey(MNEMONIC, i)), 1_000_000e6);
        }
        vm.stopBroadcast();
        bondSigner(aggPk);
        bondSigner(cosPk);
        fundAcme(vm.deriveKey(MNEMONIC, 1), cpPk);

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
        // the relying party's numeric id (`rp_<16 hex>` in the Developer Portal); only the real WorldIDVerifier checks it
        uint64 rpId = uint64(vm.envOr("WORLD_ID_RP_ID_U64", uint256(1)));
        dedan = new DedanRegistry(labelStore, gov, usdc, IWorldIDVerifier(address(worldIdGate)), rpId, "dedan.eth");
        resolver = new DedanResolver(IPermissionedRegistry(address(dedan)), "dedan.eth");
        dedan.setDedanResolver(resolver);
        cells = new DedanCells(dedan, "dedan.eth", 1); // one vote names a node on the devnet (one human per city)
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
        dedan.setEarlyReveal(true); // devnet nodes reveal the current hour's key at once
    }

    /// Signers count only with the minimum bond; the control plane signs new account ids; price table 1 lists the
    /// demo tier: 6 nodes, 100 Mbps, 99.9 %, 7,200 USDC a day.
    function deploySettlement(address gov, address aggregator, address countersigner, address controlPlane) internal {
        settlement = new Settlement(usdc, gov);
        address[] memory signers = new address[](2);
        signers[0] = aggregator;
        signers[1] = countersigner;
        settlement.setSignerSet(signers, 2);
        settlement.setMinSignerBond(SIGNER_BOND);
        settlement.setRegistrar(controlPlane);
        settlement.setPrice(1, settlement.tierKey(6, 100, 9990), 7_200e6);
    }

    function bondSigner(uint256 pk) internal {
        vm.startBroadcast(pk);
        usdc.approve(address(settlement), SIGNER_BOND);
        settlement.depositBond(SIGNER_BOND);
        vm.stopBroadcast();
    }

    /// The demo customer: account `acme` for key index 1, reserved by the registrar's signature, 10,000 USDC in.
    function fundAcme(uint256 customerPk, uint256 registrarPk) internal {
        address customer = vm.addr(customerPk);
        bytes32 d = keccak256(abi.encode(block.chainid, address(settlement), ACME, customer));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(registrarPk, MessageHashUtils.toEthSignedMessageHash(d));
        vm.startBroadcast(customerPk);
        settlement.createAccount(ACME, customer, abi.encodePacked(r, s, v));
        usdc.approve(address(settlement), 10_000e6);
        settlement.deposit(ACME, 10_000e6);
        vm.stopBroadcast();
    }

    function writeJson(address gov, address aggregator, address countersigner, address controlPlane) internal {
        string memory j = "devnet";
        vm.serializeUint(j, "chainId", block.chainid);
        vm.serializeAddress(j, "usdc", address(usdc));
        vm.serializeAddress(j, "worldId", address(worldId));
        vm.serializeAddress(j, "worldIdGate", address(worldIdGate));
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
        vm.serializeAddress(j, "registrar", settlement.registrar());
        vm.serializeBool(j, "acmeFunded", true);
        string memory out = vm.serializeAddress(j, "settlement", address(settlement));
        vm.writeJson(out, "deployments/devnet.json");
    }
}
