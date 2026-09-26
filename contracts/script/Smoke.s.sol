// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Script, console2} from "forge-std/Script.sol";
import {MessageHashUtils} from "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {UniversalResolverV2} from "@ensdomains/contracts-v2/universalResolver/UniversalResolverV2.sol";
import {ITextResolver} from "@ens/contracts/resolvers/profiles/ITextResolver.sol";
import {NameCoder} from "@ens/contracts/utils/NameCoder.sol";
import {DedanRegistry} from "../src/DedanRegistry.sol";
import {DedanResolver} from "../src/DedanResolver.sol";
import {LibDnsWire} from "../src/LibDnsWire.sol";
import {Settlement} from "../src/Settlement.sol";
import {MockUSDC} from "../src/mocks/MockUSDC.sol";
import {Merkle} from "../test/Merkle.sol";

/// One settlement day on the devnet, in two stages separated by a 73 h clock jump (deploy/scripts/smoke.sh):
///   STAGE=1  the customer funds account "acme" and requests a partial withdrawal; a World ID-verified operator
///            registers the node name `smoke.dedan.eth` with its records and a hot key; an attester and an auditor
///            verify and register; the hot key reveals a token key; the attester attests; the records resolve
///            through the Universal Resolver; the operator changes the node's IP record; the aggregator and
///            countersigner bond and publish the period-1 roots (node 80, treasury 20, bill 100).
///   STAGE=2  the operator claims its 80 USDC, the customer's 100 USDC bill is applied and 500 USDC withdrawn.
contract Smoke is Script {
    string constant MNEMONIC = "test test test test test test test test test test test junk";
    bytes32 constant ACME = bytes32("acme");

    MockUSDC usdc;
    DedanRegistry dedan;
    DedanResolver resolver;
    UniversalResolverV2 ur;
    Settlement settlement;

    uint256 govPk;
    uint256 customerPk;
    uint256 opPk;
    uint256 hotPk;
    uint256 attesterPk;
    uint256 auditorPk;
    uint256 aggPk;
    uint256 cosPk;

    function run() external {
        string memory json = vm.readFile("deployments/devnet.json");
        usdc = MockUSDC(vm.parseJsonAddress(json, ".usdc"));
        dedan = DedanRegistry(vm.parseJsonAddress(json, ".dedanRegistry"));
        resolver = DedanResolver(vm.parseJsonAddress(json, ".dedanResolver"));
        ur = UniversalResolverV2(vm.parseJsonAddress(json, ".universalResolver"));
        settlement = Settlement(vm.parseJsonAddress(json, ".settlement"));
        govPk = vm.deriveKey(MNEMONIC, 0);
        customerPk = vm.deriveKey(MNEMONIC, 1);
        opPk = vm.deriveKey(MNEMONIC, 7);
        hotPk = vm.deriveKey(MNEMONIC, 17);
        attesterPk = vm.deriveKey(MNEMONIC, 25);
        auditorPk = vm.deriveKey(MNEMONIC, 26);
        aggPk = vm.deriveKey(MNEMONIC, 8);
        cosPk = vm.deriveKey(MNEMONIC, 9);

        string memory stage = vm.envOr("STAGE", string("1"));
        if (keccak256(bytes(stage)) == keccak256("1")) stage1();
        else stage2();
    }

    function stage1() internal {
        address customer = vm.addr(customerPk);
        address op = vm.addr(opPk);
        address treasury = vm.addr(govPk);

        vm.startBroadcast(customerPk);
        usdc.approve(address(settlement), 10_000e6);
        settlement.createAccount(ACME, customer);
        settlement.deposit(ACME, 10_000e6);
        settlement.requestWithdraw(ACME, 500e6);
        vm.stopBroadcast();

        // a verified human registers itself as an operator, mints its node name with a hot key, then writes
        // the node's records on the resolver as any ENS owner does
        address[] memory signers = new address[](1);
        signers[0] = vm.addr(hotPk);
        bytes32 node = dedan.nodeOf("smoke");
        bytes memory name = NameCoder.encode("smoke.dedan.eth");
        bytes[] memory calls = new bytes[](4);
        calls[0] = abi.encodeWithSignature("setAddr(bytes32,address)", node, op);
        calls[1] = abi.encodeWithSignature("setText(bytes32,string,string)", node, "dedan.class", "100M");
        calls[2] = abi.encodeWithSignature("setText(bytes32,string,string)", node, "dedan.location", "41.0082,28.9784");
        calls[3] = abi.encodeWithSignature("setDNSRecords(bytes32,bytes)", node, LibDnsWire.a(name, 60, 0x0a4d0063)); // 10.77.0.99
        vm.startBroadcast(opPk);
        usdc.approve(address(dedan), 1_000e6);
        dedan.registerOperator(_proof(70));
        uint256 tokenId = dedan.registerNode("smoke", signers, 1_000e6);
        resolver.multicall(calls);
        vm.stopBroadcast();

        // a verified attester and a staked auditor
        vm.startBroadcast(attesterPk);
        dedan.registerAttester(_proof(250));
        DedanRegistry.Attestation[] memory batch = new DedanRegistry.Attestation[](1);
        batch[0] = DedanRegistry.Attestation({
            cell: 0x851fb467fffffff,
            labelHash: keccak256("smoke"),
            rttMs: 12,
            hitTtfbMs: 20,
            token: "smoke.29000000.00",
            prefix: "10.77.0.0/24"
        });
        dedan.attest(batch);
        vm.stopBroadcast();
        vm.startBroadcast(auditorPk);
        usdc.approve(address(dedan), 100e6);
        dedan.registerAuditor(_proof(260), 100e6);
        vm.stopBroadcast();

        // the node's hot key reveals an hourly token key
        vm.startBroadcast(hotPk);
        dedan.revealKey("smoke", 483_333, bytes32(uint256(1)));
        vm.stopBroadcast();

        // the records resolve per ENSIP-10 through the Universal Resolver, and the A record is readable
        (bytes memory res, address found) = ur.resolve(name, abi.encodeCall(ITextResolver.text, (node, "dedan.class")));
        require(found == address(resolver), "universal resolver found another resolver");
        require(keccak256(bytes(abi.decode(res, (string)))) == keccak256("100M"), "dedan.class record");
        require(resolver.dnsRecord(node, keccak256(name), 1).length > 0, "A record missing");

        // the operator changes the node's IP: one DNS record write, the path every operator uses
        vm.startBroadcast(opPk);
        resolver.setDNSRecords(node, LibDnsWire.a(name, 60, 0x0a4d0064)); // 10.77.0.100
        vm.stopBroadcast();

        vm.startBroadcast(aggPk);
        usdc.approve(address(settlement), 20_000e6);
        settlement.depositBond(20_000e6);
        vm.stopBroadcast();
        vm.startBroadcast(cosPk);
        usdc.approve(address(settlement), 20_000e6);
        settlement.depositBond(20_000e6);
        vm.stopBroadcast();

        bytes32 nodeLeaf = Merkle.leaf(abi.encode(op, uint256(80e6)));
        bytes32 treasuryLeaf = Merkle.leaf(abi.encode(treasury, uint256(20e6)));
        Settlement.RootInput memory r = Settlement.RootInput({
            period: 1,
            inputsHash: keccak256("devnet inputs, period 1"),
            registryBlock: uint64(block.number),
            priceTableVersion: 1,
            payoutRoot: Merkle.pair(nodeLeaf, treasuryLeaf),
            billRoot: Merkle.leaf(abi.encode(ACME, uint256(100e6))),
            totalCumPayout: 100e6,
            totalCumBill: 100e6
        });
        bytes32 d = settlement.digest(r);
        bytes[] memory sigs = new bytes[](2);
        sigs[0] = _sign(aggPk, d);
        sigs[1] = _sign(cosPk, d);
        vm.startBroadcast(aggPk);
        settlement.publishRoot(r, sigs);
        vm.stopBroadcast();

        console2.log("stage 1: account acme funded with 10000 USDC, 500 USDC withdrawal requested");
        console2.log(
            "stage 1: smoke.dedan.eth registered (token %s), records resolve through the Universal Resolver", tokenId
        );
        console2.log(
            "stage 1: attester and auditor verified; key revealed; attestation recorded; IP record changed by the operator"
        );
        console2.log("stage 1: period 1 roots published with 2 signatures (node 80, treasury 20, acme billed 100)");
    }

    function stage2() internal {
        address customer = vm.addr(customerPk);
        address op = vm.addr(opPk);
        address treasury = vm.addr(govPk);
        require(settlement.latestFinalPeriod() == 1, "period 1 is not final yet: run the clock jump first");

        bytes32[] memory proof = new bytes32[](1);
        proof[0] = Merkle.leaf(abi.encode(treasury, uint256(20e6)));
        uint256 opBefore = usdc.balanceOf(op);
        uint256 customerBefore = usdc.balanceOf(customer);

        vm.startBroadcast(opPk);
        settlement.claim(op, 80e6, proof);
        vm.stopBroadcast();

        vm.startBroadcast(customerPk);
        settlement.executeWithdraw(ACME, 100e6, new bytes32[](0));
        vm.stopBroadcast();

        require(usdc.balanceOf(op) == opBefore + 80e6, "node claim amount");
        require(usdc.balanceOf(customer) == customerBefore + 500e6, "customer withdrawal amount");
        (, uint256 balance, uint256 applied,,) = settlement.accounts(ACME);
        require(balance == 9_400e6 && applied == 100e6, "account state after bill and withdrawal");

        console2.log("stage 2: node operator claimed 80 USDC with one Merkle proof");
        console2.log("stage 2: acme billed 100 USDC then withdrew 500 USDC; balance left %s USDC", balance / 1e6);
    }

    function _proof(uint256 nullifier) internal pure returns (DedanRegistry.WorldIdProof memory p) {
        p.nullifier = nullifier;
    }

    function _sign(uint256 pk, bytes32 d) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(pk, MessageHashUtils.toEthSignedMessageHash(d));
        return abi.encodePacked(r, s, v);
    }
}
