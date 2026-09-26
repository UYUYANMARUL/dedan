// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Test} from "forge-std/Test.sol";
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
import {DedanCells} from "../src/DedanCells.sol";
import {DedanRegistry} from "../src/DedanRegistry.sol";
import {DedanResolver} from "../src/DedanResolver.sol";
import {LibCell} from "../src/LibCell.sol";
import {IWorldIDVerifier} from "../src/IWorldIDVerifier.sol";
import {StubGatewayProvider} from "../src/StubGatewayProvider.sol";
import {MockUSDC} from "../src/mocks/MockUSDC.sol";
import {MockWorldID} from "../src/mocks/MockWorldID.sol";

/// Cells as names, membership, the on-chain median and its records (ARCHITECTURE.md §6, §7).
contract DedanCellsTest is Test {
    uint256 constant TLD_GOV_ROLES = RegistryRolesLib.ROLE_REGISTRAR | RegistryRolesLib.ROLE_REGISTRAR_ADMIN
        | RegistryRolesLib.ROLE_SET_PARENT | RegistryRolesLib.ROLE_SET_PARENT_ADMIN;
    uint256 constant LABEL_OWNER_ROLES = RegistryRolesLib.ROLE_SET_SUBREGISTRY | RegistryRolesLib.ROLE_SET_RESOLVER;
    // resolution-2 cells of Frankfurt and Istanbul (crates/core/examples/cell.rs)
    uint64 constant FRA_CELL = 586025403953446911;
    uint64 constant IST_CELL = 586010010790658047;

    MockUSDC usdc;
    MockWorldID worldId;
    LabelStore labelStore;
    PermissionedRegistry root;
    PermissionedRegistry eth;
    DedanRegistry dedan;
    DedanResolver resolver;
    DedanCells cells;
    UniversalResolverV2 ur;

    address gov = makeAddr("gov");
    address op1 = makeAddr("op1");
    address op2 = makeAddr("op2");
    address human1 = makeAddr("human1");
    address human2 = makeAddr("human2");
    address human3 = makeAddr("human3");
    address stranger = makeAddr("stranger");
    uint256 nextNullifier = 1;

    function setUp() public {
        usdc = new MockUSDC();
        worldId = new MockWorldID();
        labelStore = new LabelStore(IContractNamer(address(0)));
        vm.startPrank(gov);
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
        vm.stopPrank();
        vm.warp(1_790_000_000);

        address[2] memory ops = [op1, op2];
        for (uint256 i; i < ops.length; ++i) {
            usdc.mint(ops[i], 10_000e6);
            vm.prank(ops[i]);
            usdc.approve(address(dedan), type(uint256).max);
        }
        registerNode(op1, "fra");
        registerNode(op2, "ist");
        address[3] memory humans = [human1, human2, human3];
        for (uint256 i; i < humans.length; ++i) {
            vm.prank(humans[i]);
            dedan.registerAttester(proof(nextNullifier++));
        }
    }

    function proof(uint256 nullifier) internal pure returns (DedanRegistry.WorldIdProof memory p) {
        p.nullifier = nullifier;
    }

    function registerNode(address op, string memory label) internal {
        vm.startPrank(op);
        dedan.registerOperator(proof(nextNullifier++));
        dedan.registerNode(label, new address[](0), 1_000e6);
        bytes32 node = dedan.nodeOf(label);
        resolver.setText(node, "dedan.class", "100M");
        resolver.setAddr(node, op);
        vm.stopPrank();
    }

    function cellsOf(uint64 a) internal pure returns (uint64[] memory c) {
        c = new uint64[](1);
        c[0] = a;
    }

    function sample(string memory label, uint32 rtt) internal pure returns (DedanCells.Sample memory s) {
        s.labelHash = keccak256(bytes(label));
        s.rttMs = rtt;
        s.token = "tok";
        s.prefix = "10.77.16.0/24";
    }

    function one(string memory label, uint32 rtt) internal pure returns (DedanCells.Sample[] memory b) {
        b = new DedanCells.Sample[](1);
        b[0] = sample(label, rtt);
    }

    function textOf(string memory name, string memory key) internal view returns (string memory, address) {
        bytes memory dns = NameCoder.encode(name);
        bytes32 node = NameCoder.namehash(dns, 0);
        (bytes memory res, address found) = ur.resolve(dns, abi.encodeCall(ITextResolver.text, (node, key)));
        return (abi.decode(res, (string)), found);
    }

    function labelId(string memory label) internal pure returns (uint256) {
        return uint256(keccak256(bytes(label)));
    }

    function test_join_needs_the_owner_of_an_active_node() public {
        vm.prank(stranger);
        vm.expectRevert(DedanCells.NotNodeOwner.selector);
        cells.join("fra", cellsOf(FRA_CELL));
        vm.prank(op1);
        dedan.requestUnbond("fra");
        vm.prank(op1);
        vm.expectRevert(DedanCells.NodeInactive.selector);
        cells.join("fra", cellsOf(FRA_CELL));
    }

    function test_first_join_registers_the_cell_name_and_its_records_resolve() public {
        assertEq(LibCell.label(FRA_CELL), "821faffffffffff");
        vm.prank(op1);
        cells.join("fra", cellsOf(FRA_CELL));
        vm.prank(op2);
        cells.join("ist", cellsOf(FRA_CELL));
        assertEq(dedan.getOwner(labelId("821faffffffffff")), address(cells));

        (string memory nodes, address found) = textOf("821faffffffffff.dedan.eth", "dedan.nodes");
        assertEq(found, address(cells));
        assertEq(nodes, "fra,ist");
        (string memory cell,) = textOf("821faffffffffff.dedan.eth", "dedan.cell");
        assertEq(cell, "821faffffffffff");

        // the alias name walks through the self-referencing subregistry to the node resolver, which answers the
        // node's own records and the cell's records for it
        (string memory class, address found2) = textOf("fra.821faffffffffff.dedan.eth", "dedan.class");
        assertEq(found2, address(resolver));
        assertEq(class, "100M");
        (string memory latency,) = textOf("fra.821faffffffffff.dedan.eth", "dedan.latency");
        assertEq(latency, "");
        (string memory which,) = textOf("fra.821faffffffffff.dedan.eth", "dedan.cell");
        assertEq(which, "821faffffffffff");
        assertEq(resolver.addr(NameCoder.namehash(NameCoder.encode("fra.821faffffffffff.dedan.eth"), 0)), op1);

        // under a cell the node never joined the alias carries nothing
        vm.prank(op2);
        cells.join("ist", cellsOf(IST_CELL));
        (string memory none,) = textOf("fra.821ecffffffffff.dedan.eth", "dedan.class");
        assertEq(none, "");

        // a node cannot squat a cell label
        vm.prank(op1);
        vm.expectRevert(DedanRegistry.CellLabel.selector);
        dedan.registerNode("821faffffffffff", new address[](0), 1_000e6);
    }

    function test_attest_rules_and_the_median() public {
        vm.prank(op1);
        cells.join("fra", cellsOf(FRA_CELL));
        vm.prank(op2);
        cells.join("ist", cellsOf(IST_CELL));

        vm.prank(stranger);
        vm.expectRevert(DedanCells.NotAttester.selector);
        cells.attest(FRA_CELL, one("fra", 10));
        vm.prank(human1);
        vm.expectRevert(DedanCells.NotMember.selector);
        cells.attest(FRA_CELL, one("ist", 10));

        // one vote per human per node per cell: a second sample replaces the first
        vm.prank(human1);
        vm.expectEmit(true, true, true, true);
        emit DedanCells.Attested(human1, FRA_CELL, keccak256("fra"), 10, 0, "tok", "10.77.16.0/24", uint64(block.timestamp));
        cells.attest(FRA_CELL, one("fra", 10));
        vm.prank(human1);
        vm.expectRevert(DedanCells.TooSoon.selector);
        cells.attest(FRA_CELL, one("fra", 90));
        vm.prank(human1); // another cell at once is fine
        cells.attest(IST_CELL, one("ist", 50));
        vm.warp(block.timestamp + 60);
        vm.prank(human1);
        cells.attest(FRA_CELL, one("fra", 90));
        (uint32 ms, uint32 votes) = cells.median(FRA_CELL, labelId("fra"));
        assertEq(votes, 1);
        assertEq(ms, 92); // bucket 11 (88-96 ms), midpoint
        uint16[32] memory h = cells.histogram(FRA_CELL, labelId("fra"), uint32(block.timestamp / 1 days));
        assertEq(h[1], 0);
        assertEq(h[11], 1);

        // the median is over humans: 90, 20, 40 -> 40 (lower-middle of the sorted votes)
        vm.prank(human2);
        cells.attest(FRA_CELL, one("fra", 20));
        vm.prank(human3);
        cells.attest(FRA_CELL, one("fra", 40));
        (ms, votes) = cells.median(FRA_CELL, labelId("fra"));
        assertEq(votes, 3);
        assertEq(ms, 44);
        (string memory rec,) = textOf("fra.821faffffffffff.dedan.eth", "dedan.latency");
        assertEq(rec, "44");
        (string memory v,) = textOf("821faffffffffff.dedan.eth", "dedan.votes.fra");
        assertEq(v, "3");
        (uint256[] memory ids, uint32[] memory medians, uint32[] memory counts) = cells.snapshot(FRA_CELL);
        assertEq(ids.length, 1);
        assertEq(medians[0], 44);
        assertEq(counts[0], 3);

        // yesterday's vote is replaced by today's, never counted twice; two days on, it ages out
        vm.warp(block.timestamp + 1 days);
        vm.prank(human1);
        cells.attest(FRA_CELL, one("fra", 200));
        (ms, votes) = cells.median(FRA_CELL, labelId("fra"));
        assertEq(votes, 3);
        assertEq(ms, 44);
        vm.warp(block.timestamp + 1 days);
        (ms, votes) = cells.median(FRA_CELL, labelId("fra"));
        assertEq(votes, 1);
        assertEq(ms, 204);
        // the same human's vote in the other cell is independent
        (ms, votes) = cells.median(IST_CELL, labelId("ist"));
        assertEq(votes, 0); // two days old, out of the window
    }

    function test_leave_removes_membership_and_the_alias() public {
        uint64[] memory both = new uint64[](2);
        both[0] = FRA_CELL;
        both[1] = IST_CELL;
        vm.prank(op1);
        cells.join("fra", both);
        vm.prank(op2);
        cells.join("ist", cellsOf(FRA_CELL));
        vm.prank(op1);
        cells.leave("fra", cellsOf(FRA_CELL));
        uint256[] memory left = cells.membersOf(FRA_CELL);
        assertEq(left.length, 1);
        assertEq(left[0], labelId("ist"));
        (string memory nodes,) = textOf("821faffffffffff.dedan.eth", "dedan.nodes");
        assertEq(nodes, "ist");
        (string memory class,) = textOf("fra.821faffffffffff.dedan.eth", "dedan.class");
        assertEq(class, "");
        (string memory still,) = textOf("fra.821ecffffffffff.dedan.eth", "dedan.class");
        assertEq(still, "100M");
        vm.prank(human1);
        vm.expectRevert(DedanCells.NotMember.selector);
        cells.attest(FRA_CELL, one("fra", 10));
    }
}
