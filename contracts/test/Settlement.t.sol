// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Test} from "forge-std/Test.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
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
    uint256 registrarPk = 0xC0DE;
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
        s.setPrice(1, s.tierKey(6, 100, 9990), 7_200e6);
        s.setRegistrar(vm.addr(registrarPk));
        usdc.mint(customer, 1_000_000e6);
        bytes memory sig = _regSig(registrarPk, ACME, customer);
        vm.startPrank(customer);
        usdc.approve(address(s), type(uint256).max);
        s.createAccount(ACME, customer, sig);
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

    function _regSig(uint256 pk, bytes32 id, address key) internal view returns (bytes memory) {
        return _sig(pk, keccak256(abi.encode(block.chainid, address(s), id, key)));
    }

    function _publish(uint64 period, uint256 cumPayout, uint256 cumBill) internal {
        Settlement.RootInput memory r = _input(period, 0, 0, cumPayout, cumBill);
        s.publishRoot(r, _sigs(r, aggPk, cosPk));
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

    function test_publishRoot_needsAPriceVersionWithPrices() public {
        Settlement.RootInput memory r = _input(1, 0, 0, 0, 0);
        r.priceTableVersion = 9;
        bytes[] memory sigs = _sigs(r, aggPk, cosPk);
        vm.expectRevert(Settlement.UnknownPriceTable.selector);
        s.publishRoot(r, sigs);

        bytes32 tier = s.tierKey(6, 100, 9990);
        assertEq(tier, keccak256(abi.encode(uint16(6), uint32(100), uint32(9990))));
        assertEq(s.prices(1, tier), 7_200e6);
        vm.prank(customer);
        vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, customer));
        s.setPrice(9, tier, 1);
        vm.expectEmit(true, true, false, true);
        emit Settlement.PriceSet(9, tier, 9_000e6);
        s.setPrice(9, tier, 9_000e6);
        assertTrue(s.priceVersionSet(9));
        s.publishRoot(r, sigs);
        assertEq(s.lastPeriod(), 1);
    }

    function test_publishRoot_ignoresUnbondedSigners() public {
        s.setMinSignerBond(20_000e6);
        usdc.mint(vm.addr(aggPk), 20_000e6);
        vm.startPrank(vm.addr(aggPk));
        usdc.approve(address(s), 20_000e6);
        s.depositBond(20_000e6);
        vm.stopPrank();

        // the countersigner has no bond: its signature does not count and one bonded signature is below the threshold
        Settlement.RootInput memory r = _input(1, 0, 0, 0, 0);
        bytes[] memory sigs = _sigs(r, aggPk, cosPk);
        vm.expectRevert(Settlement.BadSignatures.selector);
        s.publishRoot(r, sigs);

        usdc.mint(vm.addr(cosPk), 20_000e6);
        vm.startPrank(vm.addr(cosPk));
        usdc.approve(address(s), 20_000e6);
        s.depositBond(19_999e6);
        vm.expectRevert(Settlement.BadSignatures.selector);
        s.publishRoot(r, sigs);
        s.depositBond(1e6);
        vm.stopPrank();
        s.publishRoot(r, sigs);

        // a slash below the minimum takes the signer out again
        s.slashBond(vm.addr(cosPk), 1, treasury);
        r = _input(2, 0, 0, 0, 0);
        sigs = _sigs(r, aggPk, cosPk);
        vm.expectRevert(Settlement.BadSignatures.selector);
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
        vm.expectRevert(Settlement.BadPeriod.selector);
        s.voidRoot(2);
    }

    function test_voidRoot_nonLatest_dropsLaterRoots_andBlocksNextUntilRepublished() public {
        uint256 t0 = block.timestamp;
        _publish(1, 10e6, 10e6);
        vm.warp(t0 + 1 hours);
        _publish(2, 20e6, 20e6);
        vm.warp(t0 + 2 hours);
        _publish(3, 30e6, 30e6);

        address cos = vm.addr(cosPk);
        vm.expectEmit(true, false, false, true);
        emit Settlement.RootVoided(1, cos);
        vm.expectEmit(false, false, false, true);
        emit Settlement.RootsDropped(2, 3);
        vm.prank(cos);
        s.voidRoot(1);
        assertEq(s.lastPeriod(), 1);
        vm.expectRevert(Settlement.BadPeriod.selector);
        s.voidRoot(2); // dropped, no longer a root

        // period 2 cannot go on top of the voided period 1; period 1 is republished first
        Settlement.RootInput memory r = _input(2, 0, 0, 20e6, 20e6);
        bytes[] memory sigs = _sigs(r, aggPk, cosPk);
        vm.expectRevert(Settlement.BadPeriod.selector);
        s.publishRoot(r, sigs);
        _publish(1, 5e6, 5e6);
        _publish(2, 15e6, 15e6);
        assertEq(s.lastPeriod(), 2);

        // the stale period 3 never becomes final; the republished ones do
        vm.warp(t0 + 3 days);
        assertTrue(s.isFinal(2));
        assertFalse(s.isFinal(3));
        assertEq(s.latestFinalPeriod(), 2);
        (,,,, uint256 cumBill,,,,) = s.roots(2);
        assertEq(cumBill, 15e6);
    }

    function test_voidRoot_earlierRootOutsideItsWindow_reverts() public {
        _publish(1, 10e6, 10e6);
        vm.warp(block.timestamp + 25 hours);
        _publish(2, 20e6, 20e6);
        vm.expectRevert(Settlement.WindowClosed.selector);
        s.voidRoot(1);
        s.voidRoot(2); // governance, the latest root inside its window
        assertEq(s.lastPeriod(), 2);
        assertEq(s.latestFinalPeriod(), 1);
    }

    // ---- accounts -----------------------------------------------------------------------------

    function test_createAccount_needsTheRegistrarSignature() public {
        bytes32 id = bytes32("globex");
        bytes32 id2 = bytes32("initech");
        address key = makeAddr("globex");
        address newRegistrar = vm.addr(strangerPk);
        bytes memory good = _regSig(registrarPk, id, key);
        bytes memory byStranger = _regSig(strangerPk, id, key);
        vm.expectRevert(Settlement.BadRegistrarSig.selector);
        s.createAccount(id, key, byStranger);
        // the signature binds the id and the key: nobody reuses it for another key or another id
        vm.expectRevert(Settlement.BadRegistrarSig.selector);
        s.createAccount(id, customer, good);
        vm.expectRevert(Settlement.BadRegistrarSig.selector);
        s.createAccount(id2, key, good);

        vm.expectEmit(true, false, false, true);
        emit Settlement.AccountCreated(id, key);
        s.createAccount(id, key, good);
        (address k,,,,) = s.accounts(id);
        assertEq(k, key);
        vm.expectRevert(Settlement.AccountExists.selector);
        s.createAccount(id, key, good);

        // governance rotates the registrar; the old one's signatures stop working
        bytes memory byOld = _regSig(registrarPk, id2, key);
        bytes memory byNew = _regSig(strangerPk, id2, key);
        vm.expectEmit(false, false, false, true);
        emit Settlement.RegistrarSet(newRegistrar);
        s.setRegistrar(newRegistrar);
        vm.expectRevert(Settlement.BadRegistrarSig.selector);
        s.createAccount(id2, key, byOld);
        s.createAccount(id2, key, byNew);
    }

    function test_billBound_countsWithdrawals() public {
        vm.prank(customer);
        s.requestWithdraw(ACME, 600e6);
        vm.warp(block.timestamp + 72 hours);
        s.executeWithdraw(ACME, 0, new bytes32[](0));
        assertEq(s.totalWithdrawn(), 600e6);

        // 1,000 in, 600 out: a bill above 400 would be paid from other customers' money
        Settlement.RootInput memory r = _input(1, 0, 0, 0, 400e6 + 1);
        bytes[] memory sigs = _sigs(r, aggPk, cosPk);
        vm.expectRevert(Settlement.BillExceedsDeposits.selector);
        s.publishRoot(r, sigs);
        _publish(1, 0, 400e6);
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
