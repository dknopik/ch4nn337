// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "@openzeppelin/contracts/utils/Create2.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";

import "./AAChannel.sol";

// mostly copy pasted from the samples
contract AAChannelFactory {
    AAChannel public immutable aaChannel;

    constructor(IEntryPoint entryPoint) {
        aaChannel = new AAChannel(entryPoint);
    }

    function createAccount(address partyA, address partyB, uint256 salt) public returns (AAChannel ret) {
        address addr = getAddress(partyA, partyB, salt);
        uint codeSize = addr.code.length;
        if (codeSize > 0) {
            return AAChannel(payable(addr));
        }
        ret = AAChannel(payable(new ERC1967Proxy{salt : bytes32(salt)}(
            address(aaChannel),
            abi.encodeCall(AAChannel.initialize, (partyA, partyB))
        )));
    }

    function getAddress(address partyA, address partyB, uint256 salt) public view returns (address) {
        return Create2.computeAddress(bytes32(salt), keccak256(abi.encodePacked(
            type(ERC1967Proxy).creationCode,
            abi.encode(
                address(aaChannel),
                abi.encodeCall(AAChannel.initialize, (partyA, partyB))
            )
        )));
    }
}
