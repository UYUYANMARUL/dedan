// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Script, console2} from "forge-std/Script.sol";
import {NameCoder} from "@ens/contracts/utils/NameCoder.sol";
import {DedanCells} from "../src/DedanCells.sol";
import {DedanRegistry} from "../src/DedanRegistry.sol";
import {DedanResolver} from "../src/DedanResolver.sol";
import {LibCell} from "../src/LibCell.sol";
import {LibDnsWire} from "../src/LibDnsWire.sol";
import {MockUSDC} from "../src/mocks/MockUSDC.sol";

/// Registers the devnet's city nodes and attesters on the Dedan registry from deployments/nodes.json
/// (copied from deploy/devnet/nodes.json by deploy/scripts/register-nodes.sh). Every operator and attester
/// is a World ID-verified human (mock proofs); every node is an ENSv2 name whose owner then writes its
/// records on the resolver: payout address, `dedan.*` text records and the DNS A record; then the owner joins
/// the node to its cells (its own H3 cell and the ring around it, `cells` in nodes.json).
contract RegisterNodes is Script {
    string constant MNEMONIC = "test test test test test test test test test test test junk";

    struct Node {
        string label;
        bytes4 ip;
        uint256 port;
        string class;
        string location;
        uint32 operator;
        uint32 hotKey;
        uint32 attester;
        uint64[] cells;
    }

    MockUSDC usdc;
    DedanRegistry dedan;
    DedanResolver resolver;
    DedanCells cells;
    string json;

    function run() external {
        string memory dep = vm.readFile("deployments/devnet.json");
        usdc = MockUSDC(vm.parseJsonAddress(dep, ".usdc"));
        dedan = DedanRegistry(vm.parseJsonAddress(dep, ".dedanRegistry"));
        resolver = DedanResolver(vm.parseJsonAddress(dep, ".dedanResolver"));
        cells = DedanCells(vm.parseJsonAddress(dep, ".dedanCells"));
        json = vm.readFile(vm.envOr("NODES_JSON", string("deployments/nodes.json")));
        uint256 count = vm.parseJsonUint(json, ".count");
        for (uint256 i = 0; i < count; i++) {
            registerOne(parse(i));
        }
    }

    function parse(uint256 i) internal view returns (Node memory n) {
        string memory p = string.concat(".nodes[", vm.toString(i), "]");
        n.label = vm.parseJsonString(json, string.concat(p, ".label"));
        n.ip = ipv4(vm.parseJsonString(json, string.concat(p, ".ip")));
        n.port = vm.parseJsonUint(json, string.concat(p, ".port"));
        n.class = vm.parseJsonString(json, string.concat(p, ".class"));
        n.location = vm.parseJsonString(json, string.concat(p, ".location"));
        n.operator = uint32(vm.parseJsonUint(json, string.concat(p, ".operator")));
        n.hotKey = uint32(vm.parseJsonUint(json, string.concat(p, ".hotKey")));
        n.attester = uint32(vm.parseJsonUint(json, string.concat(p, ".attester")));
        uint256[] memory raw = vm.parseJsonUintArray(json, string.concat(p, ".cells"));
        n.cells = new uint64[](raw.length);
        for (uint256 k = 0; k < raw.length; k++) {
            n.cells[k] = uint64(raw[k]);
        }
    }

    function registerOne(Node memory n) internal {
        uint256 opPk = vm.deriveKey(MNEMONIC, n.operator);
        address op = vm.addr(opPk);
        address[] memory signers = new address[](1);
        signers[0] = vm.addr(vm.deriveKey(MNEMONIC, n.hotKey));
        bytes[] memory calls = nodeRecords(n, op);

        vm.startBroadcast(opPk);
        if (!dedan.isOperator(op)) dedan.registerOperator(proof(100 + n.operator));
        usdc.approve(address(dedan), 1_000e6);
        dedan.registerNode(n.label, signers, 1_000e6);
        resolver.multicall(calls);
        cells.join(n.label, n.cells);
        vm.stopBroadcast();

        uint256 attesterPk = vm.deriveKey(MNEMONIC, n.attester);
        if (!dedan.isAttester(vm.addr(attesterPk))) {
            vm.startBroadcast(attesterPk);
            dedan.registerAttester(proof(200 + n.attester));
            vm.stopBroadcast();
        }
        console2.log("registered %s.dedan.eth for operator %s, attester index %s", n.label, op, n.attester);
        console2.log("  joined %s cells, e.g. %s.%s.dedan.eth", n.cells.length, n.label, LibCell.label(n.cells[0]));
    }

    /// The records every node publishes, as one resolver multicall by the owner.
    function nodeRecords(Node memory n, address payout) internal view returns (bytes[] memory calls) {
        bytes32 node = dedan.nodeOf(n.label);
        bytes memory dnsName = NameCoder.encode(string.concat(n.label, ".dedan.eth"));
        calls = new bytes[](7);
        calls[0] = abi.encodeWithSignature("setAddr(bytes32,address)", node, payout);
        calls[1] = abi.encodeWithSignature("setText(bytes32,string,string)", node, "dedan.class", n.class);
        calls[2] = abi.encodeWithSignature("setText(bytes32,string,string)", node, "dedan.location", n.location);
        calls[3] = abi.encodeWithSignature("setText(bytes32,string,string)", node, "dedan.port", vm.toString(n.port));
        calls[4] = abi.encodeWithSignature(
            "setText(bytes32,string,string)", node, "dedan.enckey", vm.toString(keccak256(bytes(n.label)))
        );
        calls[5] = abi.encodeWithSignature("setText(bytes32,string,string)", node, "dedan.version", "devnet-mock");
        calls[6] = abi.encodeWithSignature("setDNSRecords(bytes32,bytes)", node, LibDnsWire.a(dnsName, 60, n.ip));
    }

    function proof(uint256 nullifier) internal pure returns (DedanRegistry.WorldIdProof memory p) {
        p.nullifier = nullifier;
        p.issuerSchemaId = 1; // Proof of Human
    }

    /// "10.77.0.11" → 0x0a4d000b
    function ipv4(string memory s) internal pure returns (bytes4) {
        bytes memory b = bytes(s);
        uint32 out;
        uint32 octet;
        for (uint256 i = 0; i < b.length; i++) {
            if (b[i] == ".") {
                out = (out << 8) | octet;
                octet = 0;
            } else {
                octet = octet * 10 + (uint8(b[i]) - 48);
            }
        }
        return bytes4((out << 8) | octet);
    }
}
