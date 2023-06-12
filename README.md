# ch4nn337
An attempt at payment channels with the powers of ERC-4337

ETHPrague 2023

***NOTE: DO NOT USE FOR PRODUCTION PURPOSES*** 

*NB: For simplicity, I will use ERC-4337 specific terms in this explanation. However, the approach can be also applied to other AA techniques*

# Background

Payment channels work by having parties exchange signed and versioned messages offchain. When one or both parties want to exit the channel and receive their current channel balance onchain, they submit the latest message to the chain.
Ethereum-based payment channels (check https://github.com/MariusVanDerWijden/go-pay for a stupid simple implementation) use smart contracts to verify these messages and transfer the funds to the participating parties.

An important feature of payment channels is fraud resistance: as malicious parties can submit outdated messages, their counterparties need to be able to submit more recent states before the funds are paid out.
This process is called a dispute.

# How can AA help?

To submit channel states into classical smart contract payment channels, you obviously need a funded account to execute the transaction on the network.
We can avoid this if we implement the payment channel as an account! 
With the payment channel being the account, the channel itself can pay for the dispute. Parties no longer need separate funds.
When the dispute is settled, the gas used for the dispute is subtracted from the payout. 
This also allows us to unevenly distribute the fees. For example, if party A submits state 3, and party B later disputes this by submitting state 7, A dishonestly submitted a outdated state. It would be unfair to have B pay for the transaction which was made necessary by A's dishonesty, so B receives his full balance after the dispute while B's balance is reduced by the total gas spent for processing all messages.

# The approach

ERC-4337 is (obviously) not designed with adversarial parties submitting userops to the same account. Heres an example attack that we have to mitigate:

A party with little balance might grief the channel by submitting a userop with an exorbitant priority fee per gas. This causes the channel to overpay for the transaction. When the channel is closed, party A loses all its funds, and the rest of the gas cost is taken from B's balance, in severe cases until the balance is almost fully drained.

We can mitigate this by not exchanging messages opaque to ERC-4337, but exchanging full userops instead. This way, both parties agree to the gas parameters that will be used during any disputes.

# 1) What

During the last night of the hackathon, I realized that this won't work because of two issues:

### Non-sequential nonces
ERC-4337 specifies: 
>the “nonce” and “signature” fields usage is not defined by the protocol, but by each account implementation

This was nice to read. It gave me the power to embed the signatures of both parties into the signature field and allowed me to identify who issued the userop by having one party use only even nonces, and the other party only odd nonces.

So I worked with that, only to realize that the current version of the EntryPoint contract (0.6.0) enforces sequential nonces per account. This breaks the approach completely, as we don't know in advance in which userops will be actually used. Obviously malicious parties will refuse "re-signing" userops to update the nonce.

This was really frustrating as the implemented behaviour directly contradicts the published spec.

### Gas cost
It is important that parties can dispute onchain at any time. Therefore, the exchanged userops should allow any base fee per gas in order to allow disputes regardless of current network conditions. Therefore, I set the max fee per gas in the exchanged messages to the maximum uint256. This broke *something*, so these userops were rejected. At first, I attributed this to the bundler simulating the gas costs using the max fee instead of the current base fee + max priority fee, but looking back, this might not have been the case. 

# Next

Unfortunately, the nonce issue completely breaks the approach. Setting up an entry point and bundler that complies to the sentence I cited from the spec should mitigate this, but this is obviously not a good solution long-term. Maybe I will look into reimplementing this on some other AA stack.  
