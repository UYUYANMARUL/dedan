// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Test} from "forge-std/Test.sol";
import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {Settlement} from "../src/Settlement.sol";
import {MockUSDC} from "../src/mocks/MockUSDC.sol";
import {Merkle} from "./Merkle.sol";

contract SettlementTest is Test {
    MockUSDC usdc;
    Settlement s;

    uint256 aggPk = 0xA11CE;
    uint256 cosPk = 0xB0B;
    uint256 strangerPk = 0xBAD;
    address customer = makeAddr("customer");
    address node = makeAddr("node");
    address treasury = makeAddr("treasury");
    bytes32 constant ACME = bytes32("acme");

    function setUp() public {
        usdc = new MockUSDC();
        s = new Settlement(usdc, address(this));
        address[] memory signers = new address[](2);
        signers[0] = vm.addr(aggPk);
        signers[1] = vm.addr(cosPk);
        s.setSignerSet(signers, 2);
        s.setPriceTable(1, keccak256("price-table-1"));
        usdc.mint(customer, 1_000_000e6);
        vm.startPrank(customer);
        usdc.approve(address(s), type(uint256).max);
        s.createAccount(ACME, customer);
        s.deposit(ACME, 1_000e6);
        vm.stopPrank();
    }

    // ---- helpers ------------------------------------------------------------------------------

    function _input(uint64 period, bytes32 payoutRoot, bytes32 billRoot, uint256 cumPayout, uint256 cumBill)
        internal
        pure
        returns (Settlement.RootInput memory)
    {
        return Settlement.RootInput(
            period, keccak256(abi.encode("inputs", period)), 1, 1, payoutRoot, billRoot, cumPayout, cumBill
        );
    }

    function _sig(uint256 pk, bytes32 d) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 ss) = vm.sign(pk, MessageHashUtils.toEthSignedMessageHash(d));
        return abi.encodePacked(r, ss, v);
    }

    function _sigs(Settlement.RootInput memory r, uint256 pk1, uint256 pk2)
        internal
        view
        returns (bytes[] memory sigs)
    {
        bytes32 d = s.digest(r);
        sigs = new bytes[](2);
        sigs[0] = _sig(pk1, d);
        sigs[1] = _sig(pk2, d);
    }

    /// Day 1: node earns 80, treasury 20, acme is billed 100.
    function _publishDay1() internal returns (bytes32[] memory nodeProof) {
        bytes32 nodeLeaf = Merkle.leaf(abi.encode(node, uint256(80e6)));
        bytes32 treasuryLeaf = Merkle.leaf(abi.encode(treasury, uint256(20e6)));
        bytes32 payoutRoot = Merkle.pair(nodeLeaf, treasuryLeaf);
        bytes32 billRoot = Merkle.leaf(abi.encode(ACME, uint256(100e6)));
        Settlement.RootInput memory r = _input(1, payoutRoot, billRoot, 100e6, 100e6);
        s.publishRoot(r, _sigs(r, aggPk, cosPk));
        nodeProof = new bytes32[](1);
        nodeProof[0] = treasuryLeaf;
    }

    // ---- publishRoot --------------------------------------------------------------------------

    function test_publishRoot_requiresThresholdOfDistinctSigners() public {
        Settlement.RootInput memory r = _input(1, bytes32(uint256(1)), bytes32(uint256(2)), 0, 0);
        bytes32 d = s.digest(r);
        bytes[] memory one = new bytes[](1);
        one[0] = _sig(aggPk, d);
        vm.expectRevert(Settlement.BadSignatures.selector);
        s.publishRoot(r, one);

        bytes[] memory duplicate = _sigs(r, aggPk, aggPk);
        vm.expectRevert(Settlement.BadSignatures.selector);
        s.publishRoot(r, duplicate);

        bytes[] memory stranger = _sigs(r, aggPk, strangerPk);
        vm.expectRevert(Settlement.BadSignatures.selector);
        s.publishRoot(r, stranger);

        s.publishRoot(r, _sigs(r, aggPk, cosPk));
        assertEq(s.lastPeriod(), 1);
    }

    function test_publishRoot_rejectsPayoutAboveBill_andBillAboveDeposits() public {
        Settlement.RootInput memory r = _input(1, 0, 0, 101e6, 100e6);
        bytes[] memory sigs = _sigs(r, aggPk, cosPk);
        vm.expectRevert(Settlement.PayoutExceedsBill.selector);
        s.publishRoot(r, sigs);

        r = _input(1, 0, 0, 0, 1_000e6 + 1);
        sigs = _sigs(r, aggPk, cosPk);
        vm.expectRevert(Settlement.BillExceedsDeposits.selector);
        s.publishRoot(r, sigs);
    }

    function test_publishRoot_periodsAreSequentialAndTotalsMonotonic() public {
        Settlement.RootInput memory r = _input(2, 0, 0, 0, 0);
        bytes[] memory sigs = _sigs(r, aggPk, cosPk);
        vm.expectRevert(Settlement.BadPeriod.selector);
        s.publishRoot(r, sigs);

        r = _input(1, 0, 0, 50e6, 50e6);
        s.publishRoot(r, _sigs(r, aggPk, cosPk));

        r = _input(2, 0, 0, 40e6, 60e6);
        sigs = _sigs(r, aggPk, cosPk);
        vm.expectRevert(Settlement.NotMonotonic.selector);
        s.publishRoot(r, sigs);

        r = _input(2, 0, 0, 60e6, 60e6);
        s.publishRoot(r, _sigs(r, aggPk, cosPk));
        assertEq(s.lastPeriod(), 2);
    }

    function test_publishRoot_unknownPriceTable_reverts() public {
        Settlement.RootInput memory r = _input(1, 0, 0, 0, 0);
        r.priceTableVersion = 9;
        bytes[] memory sigs = _sigs(r, aggPk, cosPk);
        vm.expectRevert(Settlement.UnknownPriceTable.selector);
        s.publishRoot(r, sigs);
    }

    // ---- claims -------------------------------------------------------------------------------

    function test_claim_onlyAfterWindow_thenPaysCumulativeDelta() public {
        bytes32[] memory proof = _publishDay1();
        vm.expectRevert(Settlement.NoFinalRoot.selector);
        s.claim(node, 80e6, proof);

        vm.warp(block.timestamp + 24 hours);
        s.claim(node, 80e6, proof);
        assertEq(usdc.balanceOf(node), 80e6);
        vm.expectRevert(Settlement.NothingToClaim.selector);
        s.claim(node, 80e6, proof);

        // day 2: node's cumulative goes to 150, one claim pays the 70 delta
        bytes32 nodeLeaf = Merkle.leaf(abi.encode(node, uint256(150e6)));
        bytes32 treasuryLeaf = Merkle.leaf(abi.encode(treasury, uint256(40e6)));
        Settlement.RootInput memory r =
            _input(2, Merkle.pair(nodeLeaf, treasuryLeaf), Merkle.leaf(abi.encode(ACME, uint256(200e6))), 190e6, 200e6);
        s.publishRoot(r, _sigs(r, aggPk, cosPk));
        vm.warp(block.timestamp + 24 hours);
        proof[0] = treasuryLeaf;
        s.claim(node, 150e6, proof);
        assertEq(usdc.balanceOf(node), 150e6);
    }

    function test_claim_wrongProof_reverts() public {
        bytes32[] memory proof = _publishDay1();
        vm.warp(block.timestamp + 24 hours);
        vm.expectRevert(Settlement.BadProof.selector);
        s.claim(node, 81e6, proof);
    }

    // ---- void ---------------------------------------------------------------------------------

    function test_voidRoot_insideWindow_blocksClaims_allowsRepublish() public {
        bytes32[] memory proof = _publishDay1();
        vm.prank(vm.addr(cosPk));
        s.voidRoot(1);
        vm.warp(block.timestamp + 24 hours);
        vm.expectRevert(Settlement.NoFinalRoot.selector);
        s.claim(node, 80e6, proof);

        // republish period 1 with corrected numbers
        bytes32 nodeLeaf = Merkle.leaf(abi.encode(node, uint256(70e6)));
        Settlement.RootInput memory r = _input(1, nodeLeaf, Merkle.leaf(abi.encode(ACME, uint256(70e6))), 70e6, 70e6);
        s.publishRoot(r, _sigs(r, aggPk, cosPk));
        vm.warp(block.timestamp + 24 hours);
        s.claim(node, 70e6, new bytes32[](0));
        assertEq(usdc.balanceOf(node), 70e6);
    }

    function test_voidRoot_afterWindow_orByStranger_reverts() public {
        _publishDay1();
        vm.prank(vm.addr(strangerPk));
        vm.expectRevert(Settlement.NotSigner.selector);
        s.voidRoot(1);

        vm.warp(block.timestamp + 24 hours);
        vm.expectRevert(Settlement.WindowClosed.selector);
        s.voidRoot(1);
    }

    // ---- withdrawals --------------------------------------------------------------------------

    function test_withdraw_waitsForDelay_andAppliesBillFirst() public {
        _publishDay1();
        vm.prank(customer);
        s.requestWithdraw(ACME, 500e6);

        vm.warp(block.timestamp + 24 hours); // root final, delay not over
        vm.expectRevert(Settlement.TooEarly.selector);
        s.executeWithdraw(ACME, 100e6, new bytes32[](0));

        vm.warp(block.timestamp + 48 hours);
        s.executeWithdraw(ACME, 100e6, new bytes32[](0));
        (, uint256 balance, uint256 applied, uint256 pending,) = s.accounts(ACME);
        assertEq(balance, 400e6);
        assertEq(applied, 100e6);
        assertEq(pending, 0);
        assertEq(usdc.balanceOf(customer), 1_000_000e6 - 1_000e6 + 500e6);
    }

    function test_withdraw_cannotDodgeBill_orForgeIt() public {
        // bill equals the whole balance: nothing is left to withdraw
        bytes32 billRoot = Merkle.leaf(abi.encode(ACME, uint256(1_000e6)));
        Settlement.RootInput memory r = _input(1, 0, billRoot, 0, 1_000e6);
        s.publishRoot(r, _sigs(r, aggPk, cosPk));
        vm.prank(customer);
        s.requestWithdraw(ACME, 500e6);
        vm.warp(block.timestamp + 72 hours);

        vm.expectRevert(Settlement.BadProof.selector);
        s.executeWithdraw(ACME, 10e6, new bytes32[](0)); // a smaller bill than published

        s.executeWithdraw(ACME, 1_000e6, new bytes32[](0));
        (, uint256 balance,,,) = s.accounts(ACME);
        assertEq(balance, 0);
        assertEq(usdc.balanceOf(customer), 1_000_000e6 - 1_000e6);
    }

    function test_withdraw_beforeAnyRoot_needsNoProof() public {
        vm.prank(customer);
        s.requestWithdraw(ACME, 300e6);
        vm.warp(block.timestamp + 72 hours);
        s.executeWithdraw(ACME, 0, new bytes32[](0));
        (, uint256 balance,,,) = s.accounts(ACME);
        assertEq(balance, 700e6);
    }

    // ---- fuzz ---------------------------------------------------------------------------------

    function testFuzz_publishRoot_totalsInvariant(uint128 cumPayout, uint128 cumBill) public {
        Settlement.RootInput memory r = _input(1, 0, 0, cumPayout, cumBill);
        bytes[] memory sigs = _sigs(r, aggPk, cosPk);
        if (cumPayout > cumBill) {
            vm.expectRevert(Settlement.PayoutExceedsBill.selector);
        } else if (cumBill > 1_000e6) {
            vm.expectRevert(Settlement.BillExceedsDeposits.selector);
        }
        s.publishRoot(r, sigs);
    }
}
