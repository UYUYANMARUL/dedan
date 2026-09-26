// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Test} from "forge-std/Test.sol";
import {IEnhancedAccessControl} from "@ensdomains/contracts-v2/access-control/interfaces/IEnhancedAccessControl.sol";
import {PermissionedRegistry} from "@ensdomains/contracts-v2/registry/PermissionedRegistry.sol";
import {IPermissionedRegistry} from "@ensdomains/contracts-v2/registry/interfaces/IPermissionedRegistry.sol";
import {IRegistry} from "@ensdomains/contracts-v2/registry/interfaces/IRegistry.sol";
import {RegistryRolesLib} from "@ensdomains/contracts-v2/registry/libraries/RegistryRolesLib.sol";
import {IContractNamer} from "@ensdomains/contracts-v2/reverse-registrar/interfaces/IContractNamer.sol";
import {UniversalResolverV2} from "@ensdomains/contracts-v2/universalResolver/UniversalResolverV2.sol";
import {LabelStore} from "@ensdomains/contracts-v2/utils/LabelStore.sol";
import {IGatewayProvider} from "@ens/contracts/ccipRead/IGatewayProvider.sol";
import {IAddrResolver} from "@ens/contracts/resolvers/profiles/IAddrResolver.sol";
import {ITextResolver} from "@ens/contracts/resolvers/profiles/ITextResolver.sol";
import {NameCoder} from "@ens/contracts/utils/NameCoder.sol";
import {DedanRegistry} from "../src/DedanRegistry.sol";
import {DedanResolver} from "../src/DedanResolver.sol";
import {DedanRoles} from "../src/DedanRoles.sol";
import {LibDnsWire} from "../src/LibDnsWire.sol";
import {IWorldIDVerifier} from "../src/IWorldIDVerifier.sol";
import {StubGatewayProvider} from "../src/StubGatewayProvider.sol";
import {MockUSDC} from "../src/mocks/MockUSDC.sol";
import {MockWorldID} from "../src/mocks/MockWorldID.sol";

contract DedanRegistryTest is Test {
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
    UniversalResolverV2 ur;

    address gov = makeAddr("gov");
    address op1 = makeAddr("op1");
    address op2 = makeAddr("op2");
    address hot1 = makeAddr("hot1");
    address hot2 = makeAddr("hot2");
    address attester = makeAddr("attester");
    address auditor = makeAddr("auditor");
    address stranger = makeAddr("stranger");
    uint256 nextNullifier = 1;

    function setUp() public {
        vm.warp(1_790_000_000);
        usdc = new MockUSDC();
        worldId = new MockWorldID();
        labelStore = new LabelStore(IContractNamer(address(0)));
        vm.startPrank(gov);
        root = new PermissionedRegistry(labelStore, gov, TLD_GOV_ROLES);
        eth = new PermissionedRegistry(labelStore, gov, TLD_GOV_ROLES);
        dedan = new DedanRegistry(labelStore, gov, usdc, IWorldIDVerifier(address(worldId)), 1, "dedan.eth");
        resolver = new DedanResolver(IPermissionedRegistry(address(dedan)), "dedan.eth");
        dedan.setDedanResolver(resolver);
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
        vm.stopPrank();

        address[7] memory actors = [op1, op2, hot1, hot2, attester, auditor, stranger];
        for (uint256 i; i < actors.length; ++i) {
            usdc.mint(actors[i], 10_000e6);
            vm.prank(actors[i]);
            usdc.approve(address(dedan), type(uint256).max);
        }
    }

    // ---- helpers --------------------------------------------------------------------------------

    function proof(uint256 nullifier) internal pure returns (DedanRegistry.WorldIdProof memory p) {
        p.nullifier = nullifier;
    }

    /// The records an owner writes after registration, as one resolver multicall.
    function records(address payout) internal view returns (bytes[] memory calls) {
        bytes32 n = node();
        calls = new bytes[](6);
        calls[0] = abi.encodeWithSignature("setAddr(bytes32,address)", n, payout);
        calls[1] = abi.encodeWithSignature("setText(bytes32,string,string)", n, "dedan.class", "100M");
        calls[2] = abi.encodeWithSignature("setText(bytes32,string,string)", n, "dedan.location", "50.1,8.6");
        calls[3] = abi.encodeWithSignature("setText(bytes32,string,string)", n, "dedan.port", "80");
        calls[4] = abi.encodeWithSignature("setText(bytes32,string,string)", n, "dedan.enckey", "0xc0ffee");
        calls[5] = abi.encodeWithSignature("setDNSRecords(bytes32,bytes)", n, aRecord(0x0a4d000b));
    }

    function registerFra(address op) internal {
        address[] memory signers = new address[](1);
        signers[0] = hot1;
        bytes[] memory calls = records(op);
        vm.startPrank(op);
        dedan.registerOperator(proof(nextNullifier++));
        dedan.registerNode("fra", signers, 1_000e6);
        resolver.multicall(calls);
        vm.stopPrank();
    }

    function labelId() internal pure returns (uint256) {
        return uint256(keccak256("fra"));
    }

    function name() internal pure returns (bytes memory) {
        return NameCoder.encode("fra.dedan.eth");
    }

    function node() internal view returns (bytes32) {
        return dedan.nodeOf("fra");
    }

    function aRecord(bytes4 ip) internal pure returns (bytes memory) {
        return LibDnsWire.a(name(), 60, ip);
    }

    // ---- tests ----------------------------------------------------------------------------------

    function test_registerNode_requires_a_verified_operator() public {
        vm.prank(stranger);
        vm.expectPartialRevert(IEnhancedAccessControl.EACUnauthorizedAccountRoles.selector);
        dedan.registerNode("fra", new address[](0), 1_000e6);
    }

    function test_worldId_nullifier_is_burned_per_action() public {
        vm.startPrank(op1);
        dedan.registerOperator(proof(7));
        vm.expectRevert(DedanRegistry.NullifierUsed.selector);
        dedan.registerOperator(proof(7));
        dedan.registerAttester(proof(7)); // another action, another nullifier space
        vm.expectRevert();
        dedan.registerAttester(proof(0)); // the verifier rejects the proof
        vm.stopPrank();
        assertTrue(dedan.isOperator(op1));
        assertTrue(dedan.isAttester(op1));
        assertFalse(dedan.isOperator(stranger));
    }

    function test_registerNode_mints_the_name_grants_the_signer_and_records_resolve() public {
        registerFra(op1);
        uint256 id = labelId();
        assertEq(dedan.nodeOwner("fra"), op1);
        assertEq(dedan.ownerOf(dedan.getTokenId(id)), op1);
        assertTrue(dedan.hasRoles(id, RegistryRolesLib.ROLE_SET_RESOLVER, op1));
        assertTrue(dedan.isNodeSigner("fra", hot1));
        assertFalse(dedan.isNodeSigner("fra", hot2));
        assertEq(dedan.nodeStake(id), 1_000e6);
        assertTrue(dedan.isNodeActive("fra"));
        assertEq(usdc.balanceOf(address(dedan)), 1_000e6);
        assertEq(dedan.getResolver("fra"), address(resolver));
        assertEq(labelStore.getLabel(id), "fra");

        // records straight from the resolver
        assertEq(resolver.text(node(), "dedan.class"), "100M");
        assertEq(resolver.text(node(), "dedan.port"), "80");
        assertEq(resolver.text(node(), "dedan.enckey"), "0xc0ffee");
        assertEq(address(resolver.addr(node())), op1);
        assertEq(resolver.dnsRecord(node(), keccak256(name()), 1), aRecord(0x0a4d000b));

        // and through the Universal Resolver, per ENSIP-10
        (bytes memory res, address found) =
            ur.resolve(name(), abi.encodeCall(ITextResolver.text, (node(), "dedan.location")));
        assertEq(found, address(resolver));
        assertEq(abi.decode(res, (string)), "50.1,8.6");
        (res,) = ur.resolve(name(), abi.encodeCall(IAddrResolver.addr, (node())));
        assertEq(abi.decode(res, (address)), op1);
    }

    function test_owner_hands_out_signer_keys_and_nothing_else() public {
        registerFra(op1);
        uint256 id = labelId();
        vm.startPrank(op1);
        dedan.grantRoles(id, DedanRoles.ROLE_NODE_SIGNER, hot2);
        assertTrue(dedan.isNodeSigner("fra", hot2));
        dedan.revokeRoles(id, DedanRoles.ROLE_NODE_SIGNER, hot1);
        assertFalse(dedan.isNodeSigner("fra", hot1));
        vm.expectPartialRevert(IEnhancedAccessControl.EACCannotGrantRoles.selector);
        dedan.grantRoles(id, RegistryRolesLib.ROLE_SET_RESOLVER, hot2);
        vm.expectPartialRevert(IEnhancedAccessControl.EACCannotGrantRoles.selector);
        dedan.grantRoles(id, DedanRoles.ROLE_SUSPEND, hot2);
        vm.stopPrank();
        vm.prank(stranger);
        vm.expectPartialRevert(IEnhancedAccessControl.EACCannotGrantRoles.selector);
        dedan.grantRoles(id, DedanRoles.ROLE_NODE_SIGNER, stranger);
        assertEq(dedan.ownerOf(dedan.getTokenId(id)), op1, "regenerated token stays with the owner");
    }

    function test_revealKey_is_role_gated_and_attesters_can_be_suspended() public {
        registerFra(op1);
        vm.prank(hot1);
        vm.expectEmit(true, true, false, true);
        emit DedanRegistry.KeyRevealed(bytes32(labelId()), 5, bytes32(uint256(9)));
        dedan.revealKey("fra", 5, bytes32(uint256(9)));
        vm.prank(stranger);
        vm.expectPartialRevert(IEnhancedAccessControl.EACUnauthorizedAccountRoles.selector);
        dedan.revealKey("fra", 6, bytes32(0));

        vm.prank(attester);
        dedan.registerAttester(proof(50));
        assertTrue(dedan.isAttester(attester));
        vm.prank(gov);
        dedan.suspendAccount(attester, true);
        assertFalse(dedan.isAttester(attester));
    }

    function test_revealKey_once_per_hour_after_the_hour_unless_early() public {
        registerFra(op1);
        uint64 hour = uint64(block.timestamp / 3600);
        vm.startPrank(hot1);
        vm.expectRevert(DedanRegistry.RevealTooEarly.selector);
        dedan.revealKey("fra", hour, bytes32(uint256(1)));
        dedan.revealKey("fra", hour - 1, bytes32(uint256(1)));
        assertTrue(dedan.revealed(labelId(), hour - 1));
        vm.expectRevert(DedanRegistry.AlreadyRevealed.selector);
        dedan.revealKey("fra", hour - 1, bytes32(uint256(2))); // the first reveal stands
        vm.warp((uint256(hour) + 1) * 3600); // the hour just ended
        dedan.revealKey("fra", hour, bytes32(uint256(3)));
        vm.stopPrank();

        // the devnet switch: the current hour at once, still once
        vm.prank(stranger);
        vm.expectPartialRevert(IEnhancedAccessControl.EACUnauthorizedAccountRoles.selector);
        dedan.setEarlyReveal(true);
        vm.prank(gov);
        dedan.setEarlyReveal(true);
        assertTrue(dedan.earlyReveal());
        vm.startPrank(hot1);
        dedan.revealKey("fra", hour + 1, bytes32(uint256(4)));
        vm.expectRevert(DedanRegistry.AlreadyRevealed.selector);
        dedan.revealKey("fra", hour + 1, bytes32(uint256(5)));
        vm.stopPrank();
    }

    function test_owner_rewrites_records_and_nobody_else_can() public {
        registerFra(op1);
        bytes32 n = node();
        vm.prank(op1);
        resolver.setDNSRecords(n, aRecord(0x0a4d0064));
        assertEq(resolver.dnsRecord(n, keccak256(name()), 1), aRecord(0x0a4d0064));
        vm.prank(op1);
        resolver.setText(n, "dedan.version", "0.2.0");
        assertEq(resolver.text(n, "dedan.version"), "0.2.0");
        vm.prank(op2);
        vm.expectRevert();
        resolver.setDNSRecords(n, aRecord(0x01010101));
        vm.prank(stranger);
        vm.expectRevert();
        resolver.setText(n, "dedan.version", "evil");
        assertEq(resolver.text(n, "dedan.version"), "0.2.0");
        assertEq(resolver.dnsRecord(n, keccak256(name()), 1), aRecord(0x0a4d0064));
    }

    function test_unbond_withdraw_and_reregister_with_fresh_roles() public {
        registerFra(op1);
        uint256 id = labelId();
        vm.prank(op1);
        dedan.grantRoles(id, DedanRoles.ROLE_NODE_SIGNER, hot2);
        vm.prank(op1);
        vm.expectRevert(DedanRegistry.WrongStatus.selector);
        dedan.withdraw("fra");
        vm.prank(op1);
        dedan.requestUnbond("fra");
        assertFalse(dedan.isNodeActive("fra"));
        vm.prank(op1);
        vm.expectRevert(DedanRegistry.StillUnbonding.selector);
        dedan.withdraw("fra");
        vm.warp(block.timestamp + 7 days);
        uint256 before = usdc.balanceOf(op1);
        vm.prank(op1);
        dedan.withdraw("fra");
        assertEq(usdc.balanceOf(op1), before + 1_000e6);
        assertEq(dedan.nodeOwner("fra"), address(0));
        assertEq(dedan.nodeStake(id), 0);
        assertFalse(dedan.isNodeSigner("fra", hot2), "old roles die with the EAC version");
        assertEq(address(resolver.addr(node())), address(0), "records cleared with the name");
        assertEq(resolver.text(node(), "dedan.class"), "");
        assertEq(resolver.dnsRecord(node(), keccak256(name()), 1).length, 0);

        // the label rests until the start of the second UTC day after the withdrawal
        uint256 reusable = (block.timestamp / 1 days + 2) * 1 days;
        assertEq(dedan.reusableAt(id), reusable);
        vm.startPrank(op2);
        dedan.registerOperator(proof(nextNullifier++));
        vm.expectRevert(DedanRegistry.LabelCooling.selector);
        dedan.registerNode("fra", new address[](0), 1_000e6);
        vm.warp(reusable - 1);
        vm.expectRevert(DedanRegistry.LabelCooling.selector);
        dedan.registerNode("fra", new address[](0), 1_000e6);
        vm.stopPrank();
        vm.warp(reusable);
        registerFra(op2);
        assertEq(dedan.nodeOwner("fra"), op2);
        assertTrue(dedan.isNodeSigner("fra", hot1));
        assertFalse(dedan.isNodeSigner("fra", hot2));
        assertEq(address(resolver.addr(node())), op2, "records rewritten by the new owner");
    }

    function test_governance_suspends_and_slashes() public {
        registerFra(op1);
        vm.prank(stranger);
        vm.expectPartialRevert(IEnhancedAccessControl.EACUnauthorizedAccountRoles.selector);
        dedan.suspendNode("fra", true);
        vm.prank(gov);
        dedan.suspendNode("fra", true);
        assertFalse(dedan.isNodeActive("fra"));
        assertEq(dedan.nodeOwner("fra"), op1, "suspension keeps the name");
        vm.prank(gov);
        dedan.suspendNode("fra", false);
        assertTrue(dedan.isNodeActive("fra"));

        // a slash below the minimum stake takes the node out
        vm.prank(gov);
        dedan.slashNode("fra", 1, stranger);
        assertEq(dedan.nodeStake(labelId()), 1_000e6 - 1);
        assertFalse(dedan.isNodeActive("fra"), "below the minimum stake");
        vm.prank(gov);
        dedan.slashNode("fra", 5_000e6, stranger);
        assertEq(dedan.nodeStake(labelId()), 0, "slash is capped at the stake");
        assertEq(usdc.balanceOf(stranger), 11_000e6);
    }

    function test_suspended_node_and_account_cannot_unbond_or_withdraw() public {
        registerFra(op1);
        vm.prank(gov);
        dedan.suspendNode("fra", true);
        vm.prank(op1);
        vm.expectRevert(DedanRegistry.NodeIsSuspended.selector);
        dedan.requestUnbond("fra");
        vm.prank(gov);
        dedan.suspendNode("fra", false);
        vm.prank(op1);
        dedan.requestUnbond("fra");
        vm.prank(gov);
        dedan.suspendNode("fra", true); // evidence arrives while it unbonds
        vm.warp(block.timestamp + 7 days);
        vm.prank(op1);
        vm.expectRevert(DedanRegistry.NodeIsSuspended.selector);
        dedan.withdraw("fra");
        assertTrue(dedan.nodeSuspended(labelId()));
        assertEq(dedan.nodeStake(labelId()), 1_000e6, "the stake stays for the slash");

        vm.prank(auditor);
        dedan.registerAuditor(proof(9), 100e6);
        vm.prank(gov);
        dedan.suspendAccount(auditor, true);
        vm.prank(auditor);
        vm.expectRevert(DedanRegistry.AccountIsSuspended.selector);
        dedan.requestUnbondAccount();
        vm.prank(gov);
        dedan.suspendAccount(auditor, false);
        vm.prank(auditor);
        dedan.requestUnbondAccount();
        vm.prank(gov);
        dedan.suspendAccount(auditor, true);
        vm.warp(block.timestamp + 7 days);
        vm.prank(auditor);
        vm.expectRevert(DedanRegistry.AccountIsSuspended.selector);
        dedan.withdrawAccount();
        vm.prank(gov);
        dedan.suspendAccount(auditor, false);
        vm.prank(auditor);
        dedan.withdrawAccount();
        assertEq(dedan.accountStake(auditor), 0);
    }

    function test_register_is_closed_and_stake_floor_holds() public {
        vm.prank(op1);
        vm.expectRevert(DedanRegistry.UseRegisterNode.selector);
        dedan.register("x", op1, IRegistry(address(0)), address(0), 0, 0);
        vm.startPrank(op1);
        dedan.registerOperator(proof(3));
        vm.expectRevert(DedanRegistry.StakeTooLow.selector);
        dedan.registerNode("fra", new address[](0), 999e6);
        vm.stopPrank();
    }

    function test_auditor_needs_stake_and_loses_probes_after_withdraw() public {
        vm.startPrank(auditor);
        vm.expectRevert(DedanRegistry.StakeTooLow.selector);
        dedan.registerAuditor(proof(9), 99e6);
        dedan.registerAuditor(proof(9), 100e6);
        assertTrue(dedan.isAuditor(auditor));
        dedan.requestUnbondAccount();
        assertFalse(dedan.isAuditor(auditor), "an unbonding auditor runs no probes");
        vm.warp(block.timestamp + 7 days);
        dedan.withdrawAccount();
        vm.stopPrank();
        assertFalse(dedan.isAuditor(auditor), "no stake, no assigned probes");
        assertEq(usdc.balanceOf(auditor), 10_000e6);
    }

    function test_resolver_operator_role_is_governance_granted_and_needs_the_bond() public {
        vm.prank(gov);
        dedan.grantRootRoles(DedanRoles.ROLE_RESOLVER_OPERATOR, op2);
        assertFalse(dedan.isResolverOperator(op2), "no bond yet");
        vm.prank(op2);
        dedan.stakeAccount(1_000e6);
        assertTrue(dedan.isResolverOperator(op2));
        vm.prank(gov);
        dedan.slashAccount(op2, 1, stranger);
        assertFalse(dedan.isResolverOperator(op2), "below the minimum bond");
        vm.prank(stranger);
        vm.expectPartialRevert(IEnhancedAccessControl.EACCannotGrantRoles.selector);
        dedan.grantRootRoles(DedanRoles.ROLE_RESOLVER_OPERATOR, stranger);
    }
}
