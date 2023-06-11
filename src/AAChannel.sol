// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import "account-abstraction/interfaces/UserOperation.sol";
import "account-abstraction/interfaces/IAccount.sol";
import "account-abstraction/interfaces/IEntryPoint.sol";
import "@openzeppelin/contracts/proxy/utils/Initializable.sol";

// todo: can call to entrypoint fail?

contract AAChannel is IAccount, Initializable {
    using ECDSA for bytes32;

    uint112 public nonce;

    address public partyA;
    uint96 public balanceA;

    address public partyB;
    uint96 public balanceB;

    // slot 3
    uint112 public disputeStartNonce;
    int96 public disputeValue;
    uint48 public disputeTimestamp;

    IEntryPoint private immutable _entryPoint;

    function _requireFromEntryPoint() private view {
        require(msg.sender == address(_entryPoint));
    }

    constructor(IEntryPoint entryPoint) {
        _entryPoint = entryPoint;
        _disableInitializers();
    }

    function initialize(address _partyA, address _partyB) public initializer {
        partyA = _partyA;
        partyB = _partyB;
        // assume all funds up until now belong to partyA
        balanceA += uint96(address(this).balance);
        (bool success,) = payable(address(_entryPoint)).call{value: address(this).balance}("");
        (success);
    }

    function validateUserOp(UserOperation calldata userOp, bytes32 userOpHash, uint256 /*missingAccountFunds*/)
    external override virtual returns (uint256 validationData) {
        _requireFromEntryPoint();
        bytes4 selector = bytes4(userOp.callData);
        if (selector == this.noop.selector && nonce == 0) {
            nonce = 1;
            return _validateSignature(userOp, userOpHash);
        }
        //if (userOp.nonce > nonce) {
            nonce = uint56(userOp.nonce);
            // todo: maybe limit gas limits to avoid griefing the channel
            if (selector == this.closeDispute.selector) {
                if (disputeTimestamp != 0) {
                    validationData = _validateSignature(userOp, userOpHash);
                    if (validationData == 0) {
                        validationData += uint256(disputeTimestamp) << 160;
                    }
                    return validationData;
                }
            } else if (selector == this.coopWithdraw.selector || selector == this.dispute.selector) {
                bytes32 hash = userOpHash.toEthSignedMessageHash();
                (bytes memory signatureA, bytes memory signatureB) = abi.decode(
                    userOp.signature,
                    (bytes, bytes)
                );
                if (partyA == hash.recover(signatureA) && partyB == hash.recover(signatureB)) {
                    return 0;
                }
            }
        //}
        validationData = 1;
    }

    function _validateSignature(UserOperation calldata userOp, bytes32 userOpHash) private view returns (uint256) {
        bytes32 hash = userOpHash.toEthSignedMessageHash();
        address sender;
        if (userOp.nonce % 2 == 0) {
            sender = partyB;
        } else {
            sender = partyA;
        }
        if (sender != hash.recover(userOp.signature)) {
            return 1;
        }
        return 0;
    }

    function depositToA() public payable {
        balanceA += uint96(msg.value);
        (bool success,) = payable(address(_entryPoint)).call{value: msg.value}("");
        (success);
    }

    function depositToB() public payable {
        balanceB += uint96(msg.value);
        (bool success,) = payable(address(_entryPoint)).call{value: msg.value}("");
        (success);
    }

    function depositSplit(uint256 shareOfA) public payable {
        require(shareOfA <= msg.value);
        balanceA += uint96(shareOfA);
        balanceB += uint96(msg.value - shareOfA);
        (bool success,) = payable(address(_entryPoint)).call{value: msg.value}("");
        (success);
    }

    function noop() public{}

    uint48 private constant disputeTimeout = 60 * 60 * 15;
    // positive valueTransfer signifies flow from A to B, negative valueTransfer signifies flow from B to A
    function dispute(int96 valueTransfer) public {
        _requireFromEntryPoint();
        require(disputeTimestamp == 0 || disputeTimestamp >= block.timestamp, "dispute finished");
        require((valueTransfer > 0 && uint96(valueTransfer) < balanceA) ||
            (valueTransfer < 0 && uint96(-valueTransfer) < balanceB), "illegal valueTransfer");
        if (disputeTimestamp == 0) {
            disputeStartNonce = nonce;
        }
        disputeTimestamp = uint48(block.timestamp) + disputeTimeout;
        disputeValue = valueTransfer;
    }

    function closeDispute() public {
        require(disputeTimestamp != 0, "no dispute ongoing");
        require(disputeTimestamp <= block.timestamp, "dispute not finished");

        uint112 disputeWinner; // 2 == no winner, 0 == A wins, 1 == B wins
        uint112 disputeDistance = nonce - disputeStartNonce;
        if (disputeDistance <= 1) {
            // impossible to determine who is at fault here maybe the dispute starter started because counterparty
            // refused to coopWithdraw. starting party tried to be as honest as possible
            disputeWinner = 2;
        } else {
            // penalize the starter of the dispute as he picked a version earlier than necessary
            disputeWinner = disputeStartNonce % 2; // message sender of dispute
        }

        if (disputeWinner == 2) {
            _fairDistribute(balanceA, balanceB);
        } else if (disputeWinner == 1) {
            _unbalancedDistribute(partyB, partyA, balanceB, balanceA);
        } else {
            _unbalancedDistribute(partyA, partyB, balanceA, balanceB);
        }

        disputeTimestamp = 0;
        disputeValue = 0;
        disputeStartNonce = 0;
    }

    // positive valueTransfer signifies flow from A to B, negative valueTransfer signifies flow from B to A
    function coopWithdraw(int96 valueTransfer, uint96 withdrawA, uint96 withdrawB) public {
        _requireFromEntryPoint();
        require(disputeTimestamp != 0, "dispute ongoing");
        require((valueTransfer > 0 && uint96(valueTransfer) < balanceA) || (valueTransfer < 0 && uint96(-valueTransfer) < balanceB), "illegal valueTransfer");
        balanceA -= uint96(valueTransfer);
        balanceB -= uint96(-valueTransfer);
        require(withdrawA <= balanceA && withdrawB <= balanceB, "insufficient balance");
        _fairDistribute(withdrawA, withdrawB);
    }

    uint private constant rescueThreshold = 0.01 ether;
    function _fairDistribute(uint96 withdrawA, uint96 withdrawB) private {
        uint96 balance = uint96(_entryPoint.balanceOf(address(this)));
        int96 balanceDifference = int96(withdrawA + withdrawB) - int96(balance);
        if (address(this).balance > rescueThreshold) {
            uint share = address(this).balance / 2;
            payable(partyA).transfer(share);
            payable(partyB).transfer(share);
        }
        if (balanceDifference > 0) {
            require(uint96(balanceDifference) < withdrawA + withdrawB, "unable to subtract fees from withdrawal");
            uint96 shareOfA = uint96(balanceDifference) / 2;
            uint96 shareOfB = uint96(balanceDifference) - shareOfA;
            if (shareOfA > withdrawA) {
                withdrawB -= (withdrawA - shareOfA) + shareOfB;
                withdrawA = 0;
            } else if (shareOfB > withdrawB) {
                withdrawA -= (withdrawB - shareOfB) + shareOfA;
                withdrawB = 0;
            }
            withdrawA -= shareOfA;
            withdrawB -= shareOfB;
        } else if (balanceDifference < 0) {
            uint96 halfBalanceDifference = uint96(-balanceDifference / 2);
            withdrawA += halfBalanceDifference;
            withdrawB += halfBalanceDifference;
        }
        // todo: do we need to reentrancy guard due to this...?
        _entryPoint.withdrawTo(payable(partyA), withdrawA);
        _entryPoint.withdrawTo(payable(partyB), withdrawB);
        balanceA -= withdrawA;
        balanceB -= withdrawB;
    }

    function _unbalancedDistribute(address winner, address loser, uint96 winnerBalance, uint96 loserBalance) private {
        uint96 balance = uint96(_entryPoint.balanceOf(address(this)));
        int96 balanceDifference = int96(winnerBalance + loserBalance) - int96(balance);
        if (address(this).balance > rescueThreshold) {
            // todo: or give it to the winner?
            uint share = address(this).balance / 2;
            payable(winner).transfer(share);
            payable(loser).transfer(share);
        }
        if (balanceDifference > 0) {
            require(uint96(balanceDifference) < winnerBalance + loserBalance, "unable to subtract fees from withdrawal");
            if (uint96(balanceDifference) > loserBalance) {
                winnerBalance -= uint96(balanceDifference) - loserBalance;
                loserBalance = 0;
            } else {
                loserBalance -= uint96(balanceDifference);
            }
        } else if (balanceDifference < 0) {
            // todo: or give it to the winner?
            uint96 halfBalanceDifference = uint96(-balanceDifference / 2);
            winnerBalance += halfBalanceDifference;
            loserBalance += halfBalanceDifference;
        }
        // todo: do we need to reentrancy guard due to this...?
        _entryPoint.withdrawTo(payable(winner), winnerBalance);
        _entryPoint.withdrawTo(payable(loser), loserBalance);
        balanceA = 0;
        balanceB = 0;
    }

    receive() external payable {
        depositToA();
    }

    fallback() external payable {
        depositToA();
    }
}
