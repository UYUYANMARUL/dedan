// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Script, console2} from "forge-std/Script.sol";
import {NameCoder} from "@ens/contracts/utils/NameCoder.sol";
import {DedanRegistry} from "../src/DedanRegistry.sol";
import {DedanResolver} from "../src/DedanResolver.sol";
import {LibDnsWire} from "../src/LibDnsWire.sol";

/// An operator rewrites its node's A record: the one on-chain write behind "the IP changed in DNS a minute later".
///   LABEL=frankfurt IP=10.77.16.99 OPERATOR_INDEX=2 forge script script/SetNodeIp.s.sol:SetNodeIp --broadcast
contract SetNodeIp is Script {
    string constant MNEMONIC = "test test test test test test test test test test test junk";

    function run() external {
        string memory dep = vm.readFile("deployments/devnet.json");
        DedanRegistry dedan = DedanRegistry(vm.parseJsonAddress(dep, ".dedanRegistry"));
        DedanResolver resolver = DedanResolver(vm.parseJsonAddress(dep, ".dedanResolver"));
        string memory label = vm.envString("LABEL");
        bytes4 ip = ipv4(vm.envString("IP"));
        uint256 opPk = vm.deriveKey(MNEMONIC, uint32(vm.envUint("OPERATOR_INDEX")));
        bytes memory dnsName = NameCoder.encode(string.concat(label, ".dedan.eth"));
        vm.startBroadcast(opPk);
        resolver.setDNSRecords(dedan.nodeOf(label), LibDnsWire.a(dnsName, 60, ip));
        vm.stopBroadcast();
        console2.log("%s.dedan.eth A record set to %s by its operator", label, vm.envString("IP"));
    }

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
