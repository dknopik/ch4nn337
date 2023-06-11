use crate::Error::*;
use ch4nn337_sys::aa_channel::{AAChannel, AAChannelCalls, CoopWithdrawCall, DisputeCall};
use ch4nn337_sys::aa_channel_factory::{AAChannelFactory, CreateAccountCall};
use ethers::abi;
use ethers::abi::{AbiDecode, AbiEncode, Tokenizable};
use ethers::contract::ContractError;
use ethers::core::k256::ecdsa;
use ethers::core::k256::ecdsa::{signature, RecoveryId, SigningKey, VerifyingKey};
use ethers::providers::Middleware;
use ethers::signers::{Signer, Wallet};
use ethers::types::userop::UserOp;
use ethers::types::{Address, Bytes, Signature, U256};
use ethers::utils::keccak256;
use rand::rngs::OsRng;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::convert::Into;
use std::num::NonZeroU128;
use std::sync::Arc;
use thiserror::Error;

const CALL_GAS_LIMIT_DISPUTE: u64 = 200000;
const CALL_GAS_LIMIT_COOP: u64 = 200000;
const VERIFICATION_GAS_LIMIT: u64 = 1500000;
const PRE_VERIFICATION_GAS: u64 = 200000;
const MAX_FEE_PER_GAS: u128 = 100_000_000;
const PRIORITY_FEE: u64 = 100_000_000; // 0.1 gwei

#[derive(Error, Debug)]
pub enum Error<M: Middleware> {
    #[error("{0}")]
    MiddlewareError(M::Error),
    #[error("{0}")]
    ContractError(#[from] ContractError<M>),
    #[error("insufficient balance")]
    InsufficientBalance,
    #[error("already awaiting a signature")]
    AlreadyWaiting,
    #[error("{0}")]
    Serde(#[from] serde_json::Error),
    #[error("illegal sender")]
    IllegalSender,
    #[error("illegal nonce")]
    IllegalNonce,
    #[error("illegal initcode")]
    IllegalInitcode,
    #[error("illegal constant")]
    IllegalConstant,
    #[error("illegal calldata")]
    IllegalCalldata,
    #[error("illegal value transfer")]
    IllegalValueTransfer,
    #[error("illegal signature")]
    IllegalSignature,
}

#[derive(Serialize, Deserialize, Eq, PartialEq, Copy, Clone)]
pub enum Party {
    A,
    B,
}

#[derive(Serialize, Deserialize)]
pub struct TransferMessage {
    userop: UserOp,
    value_transfer: i128,
}

#[derive(Serialize, Deserialize)]
pub struct WithdrawalMessage {
    userop: UserOp,
    withdraw_us: u128,
    withdraw_them: u128,
}

#[derive(Serialize, Deserialize)]
pub enum Message {
    Transfer(TransferMessage),
    Withdrawal(WithdrawalMessage),
}

pub struct DisputeInfo {
    pub nonce: u128,
    pub timeout: u64,
    pub withdrawal_ours: i128,
    pub withdrawal_theirs: i128,
}

#[derive(Serialize, Deserialize)]
pub struct Channel {
    chain_id: U256,
    entry_point: Address,
    factory: Address,
    address: Address,
    us: Party,
    key: Vec<u8>,
    counterparty: Address,
    salt: U256,
    messages: Vec<Message>,
    pending_message: Option<Message>,
}

impl Channel {
    pub async fn open<M: Middleware>(
        chain_id: U256,
        entry_point: Address,
        factory: Address,
        client: Arc<M>,
    ) -> Result<(Channel, Channel), ContractError<M>> {
        let key_a = SigningKey::random(&mut OsRng);
        let key_b = SigningKey::random(&mut OsRng);
        let wallet_a = Wallet::from(key_a.clone());
        let wallet_b = Wallet::from(key_b.clone());
        let salt = OsRng.gen::<[u8; 32]>().into();
        let address_a = wallet_a.address();
        let address_b = wallet_b.address();

        let address = AAChannelFactory::new(factory, client.clone())
            .get_address(address_a, address_b, salt)
            .call()
            .await?
            .0
            .into();

        Ok((
            Channel {
                chain_id,
                entry_point,
                factory,
                address,
                us: Party::A,
                key: key_a.to_bytes().as_slice().to_vec(),
                counterparty: address_b,
                salt,
                messages: vec![],
                pending_message: None,
            },
            Channel {
                chain_id,
                entry_point,
                factory,
                address,
                us: Party::B,
                key: key_b.to_bytes().as_slice().to_vec(),
                counterparty: address_a,
                salt,
                messages: vec![],
                pending_message: None,
            },
        ))
    }

    pub fn address(&self) -> Address {
        self.address
    }

    pub fn our_address(&self) -> Address {
        self.wallet().address()
    }

    pub fn their_address(&self) -> Address {
        self.counterparty
    }

    fn init_code(&self) -> Bytes {
        let (party_a, party_b) = self.parties();
        self.factory
            .to_fixed_bytes()
            .into_iter()
            .chain(AbiEncode::encode(CreateAccountCall {
                party_a,
                party_b,
                salt: self.salt,
            }))
            .collect()
    }

    fn key(&self) -> SigningKey {
        SigningKey::from_slice(&self.key).expect("pls")
    }

    fn wallet(&self) -> Wallet<SigningKey> {
        Wallet::from(self.key())
    }

    fn parties(&self) -> (Address, Address) {
        let us = self.wallet().address();
        match self.us {
            Party::A => (us, self.counterparty),
            Party::B => (self.counterparty, us),
        }
    }

    pub async fn get_balances<M: Middleware>(
        &self,
        client: Arc<M>,
    ) -> Result<(u128, u128), Error<M>> {
        let mut balance_a;
        let mut balance_b;
        if self.is_deployed(&client).await.map_err(MiddlewareError)? {
            let channel = AAChannel::new(self.address, client.clone());
            balance_a = channel.balance_a().call().await?;
            balance_b = channel.balance_b().call().await?;
        } else {
            balance_a = client
                .get_balance(self.address, None)
                .await
                .map_err(MiddlewareError)?
                .low_u128();
            balance_b = 0;
        }
        let value_transfer = self.get_value_transfer();
        balance_a = (balance_a as i128 - value_transfer) as u128;
        balance_b += (balance_b as i128 - value_transfer) as u128;
        Ok((balance_a, balance_b))
    }

    pub async fn get_sorted_balances<M: Middleware>(
        &self,
        client: Arc<M>,
    ) -> Result<(u128, u128), Error<M>> {
        let (balance_a, balance_b) = self.get_balances(client).await?;
        Ok(if self.us == Party::A {
            (balance_a, balance_b)
        } else {
            (balance_b, balance_a)
        })
    }

    fn get_value_transfer(&self) -> i128 {
        self.messages.last().map_or(0, |message| match message {
            Message::Transfer(message) => message.value_transfer,
            Message::Withdrawal(_) => 0,
        })
    }

    pub async fn is_deployed<M: Middleware>(&self, client: &Arc<M>) -> Result<bool, M::Error> {
        client
            .get_code(self.address, None)
            .await
            .map(|code| !code.0.is_empty())
    }

    pub fn last_nonce(&self) -> U256 {
        self.messages
            .last()
            .map_or(U256::zero(), |message| match message {
                Message::Transfer(message) => message.userop.nonce,
                Message::Withdrawal(message) => message.userop.nonce,
            })
    }

    /* // this is the actual impl, but it does not work. thanks ERC-4337.
    pub fn next_outgoing_nonce(&self) -> U256 {
        let last = self.last_nonce();
        let last_even = last % 2 == U256::zero();
        match self.us {
            Party::A => last + if last_even { 2 } else { 1 },
            Party::B => last + if last_even { 1 } else { 2 },
        }
    }

    pub fn next_incoming_nonce(&self) -> U256 {
        let last = self.last_nonce();
        let last_even = last % 2 == U256::zero();
        match self.us {
            Party::A => last + if last_even { 1 } else { 2 },
            Party::B => last + if last_even { 2 } else { 1 },
        }
    }
    */
    pub fn next_outgoing_nonce(&self) -> U256 {
        if !self.messages.is_empty() {
            self.last_nonce() + 1
        } else {
            U256::zero()
        }
    }

    pub fn next_incoming_nonce(&self) -> U256 {
        self.next_outgoing_nonce()
    }

    async fn sign(&self, userop: &UserOp) -> Bytes {
        self.wallet()
            .sign_message(
                &userop
                    .get_user_op_hash(self.entry_point, self.chain_id)
                    .expect("should be fine")
                    .0,
            )
            .await
            .unwrap()
            .to_vec()
            .into()
    }

    pub async fn request_transfer<M: Middleware>(
        &mut self,
        wei: NonZeroU128,
        client: Arc<M>,
    ) -> Result<String, Error<M>> {
        if self.pending_message.is_some() {
            return Err(Error::AlreadyWaiting);
        }
        if self.get_sorted_balances(client).await?.1 < wei.get() {
            return Err(Error::InsufficientBalance);
        }

        let current = self.get_value_transfer();
        let wei = i128::try_from(wei.get()).unwrap();
        let next = match self.us {
            Party::A => current - wei,
            Party::B => current + wei,
        };

        let mut userop = UserOp {
            sender: self.address,
            nonce: self.next_outgoing_nonce().into(),
            init_code: self.init_code(),
            call_data: DisputeCall {
                value_transfer: next,
            }
            .encode()
            .into(),
            call_gas_limit: CALL_GAS_LIMIT_DISPUTE.into(),
            verification_gas_limit: VERIFICATION_GAS_LIMIT.into(),
            pre_verificaiton_gas: PRE_VERIFICATION_GAS.into(),
            max_fee_per_gas: MAX_FEE_PER_GAS.into(),
            max_priority_fee_per_gas: PRIORITY_FEE.into(),
            paymaster_and_data: Bytes::new(),
            signature: Bytes::new(),
        };

        userop.signature = self.sign(&userop).await;

        self.pending_message = Some(Message::Transfer(TransferMessage {
            userop: userop.clone(),
            value_transfer: next,
        }));

        Ok(serde_json::to_string(&userop)?)
    }

    pub async fn request_full_withdraw<M: Middleware>(
        &mut self,
        client: Arc<M>,
    ) -> Result<String, Error<M>> {
        if self.pending_message.is_some() {
            return Err(Error::AlreadyWaiting);
        }
        let (withdraw_a, withdraw_b) = self.get_balances(client).await?;

        let mut userop = UserOp {
            sender: self.address,
            nonce: self.next_outgoing_nonce().into(),
            init_code: self.init_code(),
            call_data: CoopWithdrawCall {
                value_transfer: self.get_value_transfer(),
                withdraw_a,
                withdraw_b,
            }
            .encode()
            .into(),
            call_gas_limit: CALL_GAS_LIMIT_COOP.into(),
            verification_gas_limit: VERIFICATION_GAS_LIMIT.into(),
            pre_verificaiton_gas: PRE_VERIFICATION_GAS.into(),
            max_fee_per_gas: MAX_FEE_PER_GAS.into(),
            max_priority_fee_per_gas: PRIORITY_FEE.into(),
            paymaster_and_data: Bytes::new(),
            signature: Bytes::new(),
        };

        userop.signature = self.sign(&userop).await;

        match self.us {
            Party::A => {
                self.pending_message = Some(Message::Withdrawal(WithdrawalMessage {
                    userop: userop.clone(),
                    withdraw_us: withdraw_a,
                    withdraw_them: withdraw_b,
                }))
            }
            Party::B => {
                self.pending_message = Some(Message::Withdrawal(WithdrawalMessage {
                    userop: userop.clone(),
                    withdraw_us: withdraw_b,
                    withdraw_them: withdraw_a,
                }))
            }
        }

        Ok(serde_json::to_string(&userop)?)
    }

    pub async fn receive_message<M: Middleware>(
        &self,
        userop: UserOp,
        client: Arc<M>,
    ) -> Result<Message, Error<M>> {
        if self.address != userop.sender {
            return Err(IllegalSender);
        }

        if self.next_incoming_nonce() != userop.nonce {
            return Err(IllegalNonce);
        }

        if userop.init_code != self.init_code() {
            return Err(IllegalInitcode);
        }

        if userop.paymaster_and_data != Bytes::new()
            || userop.max_priority_fee_per_gas != PRIORITY_FEE.into()
            || userop.max_fee_per_gas != MAX_FEE_PER_GAS.into()
            || userop.pre_verificaiton_gas != PRE_VERIFICATION_GAS.into()
            || userop.verification_gas_limit != VERIFICATION_GAS_LIMIT.into()
        {
            return Err(IllegalConstant);
        }

        let signature =
            Signature::try_from(userop.signature.as_ref()).map_err(|_| IllegalSignature)?;
        let address = signature
            .recover(
                userop
                    .get_user_op_hash(self.entry_point, self.chain_id)
                    .unwrap()
                    .0
                    .to_vec(),
            )
            .map_err(|_| IllegalSignature)?;
        if address != self.counterparty {
            return Err(IllegalSignature);
        }

        Ok(
            match AAChannelCalls::decode(&userop.call_data).map_err(|_| IllegalCalldata)? {
                AAChannelCalls::CoopWithdraw(CoopWithdrawCall {
                    value_transfer,
                    withdraw_a,
                    withdraw_b,
                }) => {
                    if userop.call_gas_limit != CALL_GAS_LIMIT_COOP.into() {
                        return Err(IllegalConstant);
                    }
                    if value_transfer != self.get_value_transfer() {
                        return Err(IllegalValueTransfer);
                    }
                    let (balance_a, balance_b) = self.get_balances(client).await?;
                    if withdraw_a > balance_a || withdraw_b > balance_b {
                        return Err(InsufficientBalance);
                    }

                    match self.us {
                        Party::A => Message::Withdrawal(WithdrawalMessage {
                            userop,
                            withdraw_us: withdraw_a,
                            withdraw_them: withdraw_b,
                        }),
                        Party::B => Message::Withdrawal(WithdrawalMessage {
                            userop,
                            withdraw_us: withdraw_b,
                            withdraw_them: withdraw_a,
                        }),
                    }
                }
                AAChannelCalls::Dispute(DisputeCall { value_transfer }) => {
                    if userop.call_gas_limit != CALL_GAS_LIMIT_DISPUTE.into() {
                        return Err(IllegalConstant);
                    }
                    Message::Transfer(TransferMessage {
                        userop,
                        value_transfer,
                    })
                }
                _ => return Err(IllegalCalldata),
            },
        )
    }

    pub async fn sign_message<M: Middleware>(
        &mut self,
        mut message: Message,
        client: Arc<M>,
    ) -> Result<String, Error<M>> {
        let userop = match &mut message {
            Message::Transfer(msg) => &mut msg.userop,
            Message::Withdrawal(msg) => &mut msg.userop,
        };

        let signature = self.sign(&userop).await;

        let new_sig = match self.us {
            Party::A => abi::encode(&[
                signature.into_token(),
                userop.signature.clone().into_token(),
            ]),
            Party::B => abi::encode(&[
                userop.signature.clone().into_token(),
                signature.into_token(),
            ]),
        };

        userop.signature = new_sig.into();
        let userop = userop.clone();

        if matches!(message, Message::Withdrawal(_)) {
            client
                .send_user_operation(userop.clone(), self.entry_point)
                .await
                .map_err(MiddlewareError)?;
        }

        self.messages.push(message);
        Ok(serde_json::to_string(&userop)?)
    }

    pub async fn get_dispute_info<M: Middleware>(
        &self,
        client: Arc<M>,
    ) -> Result<Option<DisputeInfo>, Error<M>> {
        if self.is_deployed(&client).await.map_err(MiddlewareError)? {
            let channel = AAChannel::new(self.address, client);
            let timeout = channel.dispute_timestamp().call().await?;
            if timeout == 0 {
                Ok(None)
            } else {
                let value = channel.dispute_value().call().await?;
                let nonce = channel.dispute_start_nonce().call().await?;
                let balance_a = channel.balance_a().call().await? as i128 - value;
                let balance_b = channel.balance_b().call().await? as i128 + value;
                Ok(Some(match self.us {
                    Party::A => DisputeInfo {
                        nonce,
                        timeout,
                        withdrawal_ours: balance_a,
                        withdrawal_theirs: balance_b,
                    },
                    Party::B => DisputeInfo {
                        nonce,
                        timeout,
                        withdrawal_ours: balance_b,
                        withdrawal_theirs: balance_a,
                    },
                }))
            }
        } else {
            Ok(None)
        }
    }

    pub fn pending_message(&self) -> Option<&Message> {
        self.pending_message.as_ref()
    }

    pub fn cancel_pending_message(&mut self) -> bool {
        self.pending_message.take().is_some()
    }

    // todo import countersigned message
    // todo send dispute
    // todo close dispute
    // todo send noop
}
