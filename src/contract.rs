use cosmos_sdk_proto::ibc::applications::transfer::v1::{MsgTransfer, MsgTransferResponse};
use cosmos_sdk_proto::traits::Message;
#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    to_binary, Addr, BankMsg, Binary, CosmosMsg, CustomQuery, Deps, DepsMut, Env, MessageInfo,
    Reply, Response, StdError, StdResult, Storage, SubMsg, Uint128,
};
use cw2::set_contract_version;
use osmosis_std::types::cosmwasm::wasm::v1::MsgExecuteContractResponse;
use osmosis_std::types::osmosis::gamm::v1beta1::MsgSwapExactAmountInResponse;
use osmosis_std::types::{
    cosmwasm::wasm::v1::MsgExecuteContract, osmosis::cosmwasmpool::v1beta1::SwapExactAmountIn,
};
// use prost::Message;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    msg::{ExecuteMsg, InstantiateMsg, MigrateMsg, QueryMsg},
    storage::{BALANCES, TOKEN_INFO, TOTAL_SUPPLY, TOTAL_VALUE},
};
// use neutron_sdk::bindings::msg::IbcFee;
use neutron_sdk::{
    bindings::{
        msg::{IbcFee, MsgSubmitTxResponse, NeutronMsg},
        query::{NeutronQuery, QueryInterchainAccountAddressResponse},
        types::ProtobufAny,
        // types::ProtobufAny,
    },
    interchain_txs::helpers::{
        decode_acknowledgement_response, decode_message_response, get_port_id,
    },
    query::min_ibc_fee::query_min_ibc_fee,
    // query::min_ibc_fee::query_min_ibc_fee,
    sudo::msg::{RequestPacket, SudoMsg},
    NeutronError,
    // NeutronError,
    NeutronResult,
};

use crate::storage::{
    add_error_to_queue,
    read_errors_from_queue,
    read_reply_payload,
    read_sudo_payload,
    save_reply_payload,
    // save_reply_payload,
    save_sudo_payload,
    AcknowledgementResult,
    SudoPayload,
    //SudoPayload,
    ACKNOWLEDGEMENT_RESULTS,
    INTERCHAIN_ACCOUNTS,
    SUDO_PAYLOAD_REPLY_ID,
};

// Default timeout for SubmitTX is two weeks
const DEFAULT_TIMEOUT_SECONDS: u64 = 60 * 60 * 24 * 7 * 2;
const FEE_DENOM: &str = "untrn";

const CONTRACT_NAME: &str = concat!("crates.io:neutron-sdk__", env!("CARGO_PKG_NAME"));
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");
// const UMEE_CONNECTION_ID: &str = "connection-18"; // mainnet
// const OSMOSIS_CONNECTION_ID: &str = "connection-10"; // testnet
const OSMOSIS_IC_ACCOUNT_ID: &str = "babu_neutron_osmosis_v1";
const REDBANK_OSMOSIS_ADDR: &str =
    "osmo1c3ljch9dfw5kf52nfwpxd2zmj2ese7agnx0p9tenkrryasrle5sqf3ftpg";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
struct OpenAckVersion {
    version: String,
    controller_connection_id: String,
    host_connection_id: String,
    address: String,
    encoding: String,
    tx_type: String,
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut<NeutronQuery>,
    _env: Env,
    _info: MessageInfo,
    _msg: InstantiateMsg,
) -> NeutronResult<Response<NeutronMsg>> {
    deps.api.debug("WASMDEBUG: instantiate");
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    // TODO upgrade checks

    // let res = execute_register_ica(
    //     deps.storage,
    //     env,
    //     OSMOSIS_CONNECTION_ID.to_string(),
    //     OSMOSIS_IC_ACCOUNT_ID.to_string(),
    // );

    // if res.is_err() {
    //     // TODO, prly do nothing as account exists
    //     deps.api.debug("WASMDEBUG: failed to add ICA for Osmosis");
    //     return Err(NeutronError::Std(StdError::generic_err(
    //         "Failed to add ICA for Osmosis",
    //     )));
    // }

    Ok(Response::default())
}

pub fn execute_mint(
    deps: &mut DepsMut<NeutronQuery>,
    _env: Env,
    info: MessageInfo,
    recipient: String,
    amount: Uint128,
) -> Result<Response, NeutronError> {
    let mut config =
        TOKEN_INFO
            .may_load(deps.storage)?
            .ok_or(NeutronError::Std(StdError::NotFound {
                kind: "TokenInfo".to_string(),
            }))?;

    if config
        .mint
        .as_ref()
        .ok_or(NeutronError::Std(StdError::NotFound {
            kind: "config.mint".to_string(),
        }))?
        .minter
        != info.sender
    {
        return Err(NeutronError::Std(StdError::GenericErr {
            msg: "Unauthorized".to_string(),
        }));
    }

    // update supply and enforce cap
    config.total_supply += amount;
    if let Some(limit) = config.get_cap() {
        if config.total_supply > limit {
            return Err(NeutronError::Std(StdError::GenericErr {
                msg: "Cannot Exceed Cap".to_string(),
            }));
        }
    }
    TOKEN_INFO.save(deps.storage, &config)?;

    // add amount to recipient balance
    let rcpt_addr = deps.api.addr_validate(&recipient)?;
    BALANCES.update(
        deps.storage,
        rcpt_addr,
        |balance: Option<Uint128>| -> StdResult<_> { Ok(balance.unwrap_or_default() + amount) },
    )?;

    let res = Response::new()
        .add_attribute("action", "mint")
        .add_attribute("to", recipient)
        .add_attribute("amount", amount);
    Ok(res)
}

// user sends money to contract
// contract sends it to osmosis
pub fn execute_fund(
    deps: &mut DepsMut<NeutronQuery>,
    env: Env,
    info: MessageInfo,
) -> NeutronResult<Response<NeutronMsg>> {
    let funds = info.funds.get(0);
    if funds.is_none() {
        return Err(NeutronError::Std(StdError::generic_err(
            "No funds sent to contract",
        )));
    }

    let fee = min_ntrn_ibc_fee(query_min_ibc_fee(deps.as_ref())?.min_fee);
    let (ica, connection_id) = get_ica(deps.as_ref(), &env, OSMOSIS_IC_ACCOUNT_ID)?;

    let deploy_msg: MsgTransfer = MsgTransfer {
        sender: env.contract.address.to_string(),
        receiver: ica,
        token: Some(cosmos_sdk_proto::cosmos::base::v1beta1::Coin {
            denom: funds.unwrap().denom.clone(),
            amount: funds.unwrap().amount.clone().to_string(),
        }),
        source_channel: "channel-186".to_string(),
        source_port: "transfer".to_string(),
        timeout_height: None,
        timeout_timestamp: 0,
    };
    let mut buf = Vec::new();
    buf.reserve(deploy_msg.encoded_len());

    if let Err(e) = deploy_msg.encode(&mut buf) {
        return Err(NeutronError::Std(StdError::generic_err(format!(
            "Encode error: {}",
            e
        ))));
    }

    let any_msg = ProtobufAny {
        type_url: "/ibc.applications.transfer.v1.MsgTransfer".to_string(),
        value: Binary::from(buf),
    };

    let cosmos_msg = NeutronMsg::submit_tx(
        connection_id,
        OSMOSIS_IC_ACCOUNT_ID.to_string(),
        vec![any_msg],
        "".to_string(),
        DEFAULT_TIMEOUT_SECONDS,
        fee,
    );

    // We use a submessage here because we need the process message reply to save
    // the outgoing IBC packet identifier for later.
    let submsg = msg_with_sudo_callback(
        deps,
        cosmos_msg,
        SudoPayload {
            port_id: get_port_id(env.contract.address.as_str(), OSMOSIS_IC_ACCOUNT_ID),
            message: "message".to_string(),
            sender: env.contract.address.to_string(),
            executor: "execute_fund_to_osmosis".to_string(),
            amount: Some(funds.unwrap().amount.clone()),
        },
    )?;

    Ok(Response::default().add_submessages(vec![submsg]))
}

// contract deploys money from osmosis to redbank
pub fn execute_deploy(
    deps: &mut DepsMut<NeutronQuery>,
    env: Env,
    sender: String,
    value: Uint128,
) -> NeutronResult<Response<NeutronMsg>> {
    let fee = min_ntrn_ibc_fee(query_min_ibc_fee(deps.as_ref())?.min_fee);
    let (ica, connection_id) = get_ica(deps.as_ref(), &env, OSMOSIS_IC_ACCOUNT_ID)?;

    let deploy_msg = MsgExecuteContract {
        sender: ica,
        contract: REDBANK_OSMOSIS_ADDR.to_string(),
        msg: "{\"deposit\":{}}".to_string().into(),
        funds: vec![osmosis_std::types::cosmos::base::v1beta1::Coin {
            denom: "uusdc".to_string(),
            amount: value.to_string(),
        }],
    };
    let mut buf = Vec::new();
    buf.reserve(deploy_msg.encoded_len());

    if let Err(e) = deploy_msg.encode(&mut buf) {
        return Err(NeutronError::Std(StdError::generic_err(format!(
            "Encode error: {}",
            e
        ))));
    }

    let any_msg = ProtobufAny {
        type_url: "/cosmwasm.wasm.v1.MsgExecuteContract".to_string(),
        value: Binary::from(buf),
    };

    let cosmos_msg = NeutronMsg::submit_tx(
        connection_id,
        OSMOSIS_IC_ACCOUNT_ID.to_string(),
        vec![any_msg],
        "".to_string(),
        DEFAULT_TIMEOUT_SECONDS,
        fee,
    );

    // We use a submessage here because we need the process message reply to save
    // the outgoing IBC packet identifier for later.
    let submsg = msg_with_sudo_callback(
        deps,
        cosmos_msg,
        SudoPayload {
            port_id: get_port_id(env.contract.address.as_str(), OSMOSIS_IC_ACCOUNT_ID),
            message: "message".to_string(),
            sender: sender.to_string(),
            executor: "execute_fund".to_string(),
            amount: Some(value),
        },
    )?;

    Ok(Response::default().add_submessages(vec![submsg]))
}

// contract send user LP tokens for funding
fn execute_tokens_to_user(
    deps: &mut DepsMut<NeutronQuery>,
    env: Env,
    sender: String,
    value: Uint128,
) -> NeutronResult<Response<NeutronMsg>> {
    let mut supply = TOTAL_SUPPLY.load(deps.storage)?;
    let mut total_value = TOTAL_VALUE.load(deps.storage)?;

    let issue: Uint128 = supply / total_value * value;

    total_value += value;
    supply += issue;

    TOTAL_SUPPLY.save(deps.storage, &supply)?;
    TOTAL_VALUE.save(deps.storage, &total_value)?;

    // call into cw20-base to mint the token, call as self as no one else is allowed
    let sub_info = MessageInfo {
        sender: env.contract.address.clone(),
        funds: vec![],
    };
    execute_mint(deps, env, sub_info, sender, issue)?;

    Ok(Response::default())
}

// contract swaps user provided liquidity to USDC
pub fn execute_ic_swap(
    mut deps: DepsMut<NeutronQuery>,
    env: Env,
    info: MessageInfo,
    timeout: Option<u64>,
) -> Result<Response<NeutronMsg>, NeutronError> {
    let coin = info.funds.get(0);
    if let Some(coin) = coin {
        let fee = min_ntrn_ibc_fee(query_min_ibc_fee(deps.as_ref())?.min_fee);
        let (ica, connection_id) = get_ica(deps.as_ref(), &env, OSMOSIS_IC_ACCOUNT_ID)?;
        let swap_msg = SwapExactAmountIn {
            sender: ica,
            token_in: Some(osmosis_std::types::cosmos::base::v1beta1::Coin {
                denom: coin.denom.clone(),
                amount: coin.amount.to_string(),
            }),
            token_out_denom: "uusdc".to_string(),
            token_out_min_amount: (coin.amount / Uint128::from(100u128) * Uint128::from(95u128))
                .to_string(),
            swap_fee: "10".to_string(),
        };
        let mut buf = Vec::new();
        buf.reserve(swap_msg.encoded_len());

        if let Err(e) = swap_msg.encode(&mut buf) {
            return Err(NeutronError::Std(StdError::generic_err(format!(
                "Encode error: {}",
                e
            ))));
        }

        let any_msg = ProtobufAny {
            type_url: "/osmosis.cosmwasmpool.v1beta1.SwapExactAmountIn".to_string(),
            value: Binary::from(buf),
        };

        let cosmos_msg = NeutronMsg::submit_tx(
            connection_id,
            OSMOSIS_IC_ACCOUNT_ID.clone().to_string(),
            vec![any_msg],
            "".to_string(),
            timeout.unwrap_or(DEFAULT_TIMEOUT_SECONDS),
            fee,
        );

        // We use a submessage here because we need the process message reply to save
        // the outgoing IBC packet identifier for later.
        let submsg = msg_with_sudo_callback(
            &mut deps,
            cosmos_msg,
            SudoPayload {
                port_id: get_port_id(env.contract.address.as_str(), OSMOSIS_IC_ACCOUNT_ID),
                message: "message".to_string(),
                sender: info.sender.to_string(),
                executor: "swap_osmosis".to_string(),
                amount: None,
            },
        )?;

        Ok(Response::default().add_submessages(vec![submsg]))
    } else {
        Err(NeutronError::Std(StdError::generic_err(
            "No funds sent to contract",
        )))
    }
}

// user starts to claim money for their LP tokens
pub fn execute_claim(
    deps: &mut DepsMut<NeutronQuery>,
    env: Env,
    info: MessageInfo,
    amount: Uint128,
) -> Result<Response<NeutronMsg>, NeutronError> {
    let balance = BALANCES.load(deps.storage, info.sender.clone())?;
    if balance < amount {
        return Err(NeutronError::Std(StdError::generic_err(
            "Cannot claim more than you have",
        )));
    }

    let total_supply = TOTAL_SUPPLY.load(deps.storage)?;
    let total_value = TOTAL_VALUE.load(deps.storage)?;
    let value = total_supply / total_value * amount;

    let fee = min_ntrn_ibc_fee(query_min_ibc_fee(deps.as_ref())?.min_fee);
    let (ica, connection_id) = get_ica(deps.as_ref(), &env, OSMOSIS_IC_ACCOUNT_ID)?;

    // we start withdrawing the claimable amount from redbank
    let claim_msg = MsgExecuteContract {
        sender: ica,
        contract: REDBANK_OSMOSIS_ADDR.to_string(),
        msg: "{\"withdraw\":{}}".to_string().into(),
        funds: vec![osmosis_std::types::cosmos::base::v1beta1::Coin {
            denom: "uusdc".to_string(),
            amount: value.to_string(),
        }],
    };

    let mut buf = Vec::new();
    buf.reserve(claim_msg.encoded_len());

    if let Err(e) = claim_msg.encode(&mut buf) {
        return Err(NeutronError::Std(StdError::generic_err(format!(
            "Encode error: {}",
            e
        ))));
    }

    let any_msg = ProtobufAny {
        type_url: "/cosmwasm.wasm.v1.MsgExecuteContract".to_string(),
        value: Binary::from(buf),
    };

    let cosmos_msg = NeutronMsg::submit_tx(
        connection_id,
        OSMOSIS_IC_ACCOUNT_ID.to_string(),
        vec![any_msg],
        "".to_string(),
        DEFAULT_TIMEOUT_SECONDS,
        fee,
    );

    // We use a submessage here because we need the process message reply to save
    // the outgoing IBC packet identifier for later.
    let submsg = msg_with_sudo_callback(
        deps,
        cosmos_msg,
        SudoPayload {
            port_id: get_port_id(env.contract.address.as_str(), OSMOSIS_IC_ACCOUNT_ID),
            message: "message".to_string(),
            sender: info.sender.to_string(),
            executor: "execute_claim".to_string(),
            amount: Some(value),
        },
    )?;

    Ok(Response::default().add_submessages(vec![submsg]))
}

#[entry_point]
pub fn execute(
    deps: DepsMut<NeutronQuery>,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> NeutronResult<Response<NeutronMsg>> {
    deps.api
        .debug(format!("WASMDEBUG: execute: received msg: {:?}", msg).as_str());
    match msg {
        ExecuteMsg::Register {
            connection_id,
            interchain_account_id,
        } => execute_register_ica(deps.storage, env, connection_id, interchain_account_id),
        ExecuteMsg::Fund {} => execute_fund(deps, env, info),
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps<NeutronQuery>, env: Env, msg: QueryMsg) -> NeutronResult<Binary> {
    match msg {
        QueryMsg::InterchainAccountAddress {
            interchain_account_id,
            connection_id,
        } => query_interchain_address(deps, env, interchain_account_id, connection_id),
        QueryMsg::InterchainAccountAddressFromContract {
            interchain_account_id,
        } => query_interchain_address_contract(deps, env, interchain_account_id),
        QueryMsg::AcknowledgementResult {
            interchain_account_id,
            sequence_id,
        } => query_acknowledgement_result(deps, env, interchain_account_id, sequence_id),
        QueryMsg::ErrorsQueue {} => query_errors_queue(deps),
        QueryMsg::Supply {} => query_supply(deps),
        QueryMsg::Balance { addr } => query_balance(deps, addr),
    }
}

// returns ICA address from Neutron ICA SDK module
pub fn query_interchain_address(
    deps: Deps<NeutronQuery>,
    env: Env,
    interchain_account_id: String,
    connection_id: String,
) -> NeutronResult<Binary> {
    let query = NeutronQuery::InterchainAccountAddress {
        owner_address: env.contract.address.to_string(),
        interchain_account_id,
        connection_id,
    };

    let res: QueryInterchainAccountAddressResponse = deps.querier.query(&query.into())?;
    Ok(to_binary(&res)?)
}

// returns ICA address from the contract storage. The address was saved in sudo_open_ack method
pub fn query_interchain_address_contract(
    deps: Deps<NeutronQuery>,
    env: Env,
    interchain_account_id: String,
) -> NeutronResult<Binary> {
    Ok(to_binary(&get_ica(deps, &env, &interchain_account_id)?)?)
}

// returns the result
pub fn query_acknowledgement_result(
    deps: Deps<NeutronQuery>,
    env: Env,
    interchain_account_id: String,
    sequence_id: u64,
) -> NeutronResult<Binary> {
    let port_id = get_port_id(env.contract.address.as_str(), &interchain_account_id);
    let res = ACKNOWLEDGEMENT_RESULTS.may_load(deps.storage, (port_id, sequence_id))?;
    Ok(to_binary(&res)?)
}

pub fn query_errors_queue(deps: Deps<NeutronQuery>) -> NeutronResult<Binary> {
    let res = read_errors_from_queue(deps.storage)?;
    Ok(to_binary(&res)?)
}

pub fn query_supply(deps: Deps<NeutronQuery>) -> NeutronResult<Binary> {
    let supply = TOTAL_SUPPLY.load(deps.storage)?;
    let value = TOTAL_VALUE.load(deps.storage)?;
    let res = to_binary(&[&supply, &value])?;
    Ok(res)
}

pub fn query_balance(deps: Deps<NeutronQuery>, addr: String) -> NeutronResult<Binary> {
    let res = BALANCES.load(deps.storage, Addr::unchecked(addr))?;
    Ok(to_binary(&res)?)
}

// saves payload to process later to the storage and returns a SubmitTX Cosmos SubMsg with necessary reply id
fn msg_with_sudo_callback<C: Into<CosmosMsg<T>>, T>(
    deps: &mut DepsMut<NeutronQuery>,
    msg: C,
    payload: SudoPayload,
) -> StdResult<SubMsg<T>> {
    save_reply_payload(deps.storage, payload)?;
    Ok(SubMsg::reply_on_success(msg, SUDO_PAYLOAD_REPLY_ID))
}

// register ICAs
fn execute_register_ica(
    storage: &mut dyn Storage,
    env: Env,
    connection_id: String,
    interchain_account_id: String,
) -> NeutronResult<Response<NeutronMsg>> {
    let register =
        NeutronMsg::register_interchain_account(connection_id, interchain_account_id.clone());
    let key = get_port_id(env.contract.address.as_str(), &interchain_account_id);

    // we are saving empty data here because we handle response of registering ICA in sudo_open_ack method
    INTERCHAIN_ACCOUNTS.save(storage, key, &None)?;
    Ok(Response::new().add_message(register))
}

// contract recalls USDC from osmosis to Neutron
fn execute_return_funds(
    deps: &mut DepsMut<NeutronQuery>,
    env: Env,
    sender: String,
    amount: Uint128,
) -> NeutronResult<Response<NeutronMsg>> {
    let fee = min_ntrn_ibc_fee(query_min_ibc_fee(deps.as_ref())?.min_fee);
    let (ica, connection_id) = get_ica(deps.as_ref(), &env, OSMOSIS_IC_ACCOUNT_ID)?;

    let deploy_msg: MsgTransfer = MsgTransfer {
        sender: ica,
        receiver: env.contract.address.to_string(),
        token: Some(cosmos_sdk_proto::cosmos::base::v1beta1::Coin {
            denom: "uusdc".to_string(),
            amount: amount.to_string(),
        }),
        source_channel: "channel-3515".to_string(),
        source_port: "transfer".to_string(),
        timeout_height: None,
        timeout_timestamp: 0,
    };
    let mut buf = Vec::new();
    buf.reserve(deploy_msg.encoded_len());

    if let Err(e) = deploy_msg.encode(&mut buf) {
        return Err(NeutronError::Std(StdError::generic_err(format!(
            "Encode error: {}",
            e
        ))));
    }

    let any_msg = ProtobufAny {
        type_url: "/ibc.applications.transfer.v1.MsgTransfer".to_string(),
        value: Binary::from(buf),
    };

    let cosmos_msg = NeutronMsg::submit_tx(
        connection_id,
        OSMOSIS_IC_ACCOUNT_ID.to_string(),
        vec![any_msg],
        "".to_string(),
        DEFAULT_TIMEOUT_SECONDS,
        fee,
    );

    // We use a submessage here because we need the process message reply to save
    // the outgoing IBC packet identifier for later.
    let submsg = msg_with_sudo_callback(
        deps,
        cosmos_msg,
        SudoPayload {
            port_id: get_port_id(env.contract.address.as_str(), OSMOSIS_IC_ACCOUNT_ID),
            message: "message".to_string(),
            sender: sender.to_string(),
            executor: "execute_return_funds".to_string(),
            amount: Some(amount),
        },
    )?;

    Ok(Response::default().add_submessages(vec![submsg]))
}

// contract sends USDC to user as a result of their LP share claim
fn execute_return_funds_user(
    storage: &mut dyn Storage,
    _env: Env,
    sender: String,
    amount: Uint128,
) -> StdResult<Response> {
    BALANCES.update(
        storage,
        Addr::unchecked(sender.clone()), // TODO
        |balance: Option<Uint128>| -> StdResult<_> { Ok(balance.unwrap_or_default() - amount) },
    )?;
    let resp = Response::new().add_message(CosmosMsg::Bank(BankMsg::Send {
        to_address: sender.clone(),
        amount: vec![cosmwasm_std::Coin {
            denom: "uusdc".to_string(), // TODO get ibc denom
            amount,
        }],
    }));
    Ok(resp)
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn sudo(deps: DepsMut<NeutronQuery>, env: Env, msg: SudoMsg) -> StdResult<Response> {
    deps.api
        .debug(format!("WASMDEBUG: sudo: received sudo msg: {:?}", msg).as_str());

    match msg {
        // For handling successful (non-error) acknowledgements.
        SudoMsg::Response { request, data } => sudo_response(deps, env, request, data),

        // For handling error acknowledgements.
        SudoMsg::Error { request, details } => sudo_error(deps, request, details),

        // For handling error timeouts.
        SudoMsg::Timeout { request } => sudo_timeout(deps, env, request),

        // For handling successful registering of ICA
        SudoMsg::OpenAck {
            port_id,
            channel_id,
            counterparty_channel_id,
            counterparty_version,
        } => sudo_open_ack(
            deps,
            env,
            port_id,
            channel_id,
            counterparty_channel_id,
            counterparty_version,
        ),
        _ => Ok(Response::default()),
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn migrate(deps: DepsMut, _env: Env, _msg: MigrateMsg) -> StdResult<Response> {
    deps.api.debug("WASMDEBUG: migrate");
    Ok(Response::default())
}

// handler
fn sudo_open_ack(
    deps: DepsMut<NeutronQuery>,
    _env: Env,
    port_id: String,
    _channel_id: String,
    _counterparty_channel_id: String,
    counterparty_version: String,
) -> StdResult<Response> {
    // The version variable contains a JSON value with multiple fields,
    // including the generated account address.
    let parsed_version: Result<OpenAckVersion, _> =
        serde_json_wasm::from_str(counterparty_version.as_str());

    // Update the storage record associated with the interchain account.
    if let Ok(parsed_version) = parsed_version {
        INTERCHAIN_ACCOUNTS.save(
            deps.storage,
            port_id,
            &Some((
                parsed_version.address,
                parsed_version.controller_connection_id,
            )),
        )?;
        return Ok(Response::default());
    }
    Err(StdError::generic_err("Can't parse counterparty_version"))
}

fn sudo_response(
    mut deps: DepsMut<NeutronQuery>,
    env: Env,
    request: RequestPacket,
    data: Binary,
) -> StdResult<Response> {
    let api = deps.api;
    api.debug(
        format!(
            "WASMDEBUG: sudo_response: sudo received: {:?} {:?}",
            request, data
        )
        .as_str(),
    );

    // WARNING: RETURNING THIS ERROR CLOSES THE CHANNEL.
    // AN ALTERNATIVE IS TO MAINTAIN AN ERRORS QUEUE AND PUT THE FAILED REQUEST THERE
    // FOR LATER INSPECTION.
    // In this particular case, we return an error because not having the sequence id
    // in the request value implies that a fatal error occurred on Neutron side.
    let seq_id = request
        .sequence
        .ok_or_else(|| StdError::generic_err("sequence not found"))?;

    // WARNING: RETURNING THIS ERROR CLOSES THE CHANNEL.
    // AN ALTERNATIVE IS TO MAINTAIN AN ERRORS QUEUE AND PUT THE FAILED REQUEST THERE
    // FOR LATER INSPECTION.
    // In this particular case, we return an error because not having the sequence id
    // in the request value implies that a fatal error occurred on Neutron side.
    let channel_id = request
        .source_channel
        .ok_or_else(|| StdError::generic_err("channel_id not found"))?;

    // NOTE: NO ERROR IS RETURNED HERE. THE CHANNEL LIVES ON.
    // In this particular example, this is a matter of developer's choice. Not being able to read
    // the payload here means that there was a problem with the contract while submitting an
    // interchain transaction. You can decide that this is not worth killing the channel,
    // write an error log and / or save the acknowledgement to an errors queue for later manual
    // processing. The decision is based purely on your application logic.
    let payload = read_sudo_payload(deps.storage, channel_id, seq_id).ok();
    if let Some(payload) = payload {
        api.debug(format!("WASMDEBUG: sudo_response: sudo payload: {:?}", payload).as_str());

        // WARNING: RETURNING THIS ERROR CLOSES THE CHANNEL.
        // AN ALTERNATIVE IS TO MAINTAIN AN ERRORS QUEUE AND PUT THE FAILED REQUEST THERE
        // FOR LATER INSPECTION.
        // In this particular case, we return an error because not being able to parse this data
        // that a fatal error occurred on Neutron side, or that the remote chain sent us unexpected data.
        // Both cases require immediate attention.
        let parsed_data = decode_acknowledgement_response(data)?;

        let mut item_types = vec![];

        for item in parsed_data {
            let item_type = item.msg_type.as_str();
            item_types.push(item_type.to_string());
            match item_type {
                "/osmosis.cosmwasmpool.v1beta1.SwapExactAmountIn" => {
                    // WARNING: RETURNING THIS ERROR CLOSES THE CHANNEL.
                    // AN ALTERNATIVE IS TO MAINTAIN AN ERRORS QUEUE AND PUT THE FAILED REQUEST THERE
                    // FOR LATER INSPECTION.
                    // In this particular case, a mismatch between the string message type and the
                    // serialised data layout looks like a fatal error that has to be investigated.
                    let out: MsgSwapExactAmountInResponse = decode_message_response(&item.data)?;

                    api.debug(format!("Swapped for uusdc: {:?}", out.token_out_amount).as_str());

                    let value_res = out.token_out_amount.parse::<Uint128>();
                    if let Ok(value) = value_res {
                        let res =
                            execute_deploy(&mut deps, env.clone(), payload.sender.clone(), value);
                        if res.is_err() {
                            let error_msg = "WASMDEBUG: Error: Unable to execute_deploy";
                            api.debug(error_msg);

                            // TODO add error to queue
                            return Err(StdError::generic_err(error_msg));
                        }
                    } else {
                        let error_msg = "WASMDEBUG: Error: Unable to parse token_out_amount";
                        api.debug(error_msg);

                        // TODO add error to queue
                        return Err(StdError::generic_err(error_msg));
                    }
                }
                // RE deploy to mars
                "/cosmwasm.wasm.v1.MsgExecuteContract" => match payload.executor.as_str() {
                    "execute_fund" => {
                        let out: MsgExecuteContractResponse = decode_message_response(&item.data)?;
                        api.debug(format!("Deployed to mars: {:?}", out).as_str());
                        if let Some(amount) = payload.amount {
                            let res = execute_tokens_to_user(
                                &mut deps,
                                env.clone(),
                                payload.sender.clone(),
                                amount,
                            );
                            if res.is_err() {
                                let error_msg =
                                    "WASMDEBUG: Error: Unable to execute_tokens_to_user";
                                api.debug(error_msg);

                                // TODO add error to queue
                                return Err(StdError::generic_err(error_msg));
                            }
                        } else {
                            let error_msg = "WASMDEBUG: Error: Unable to execute_tokens_to_user";
                            api.debug(error_msg);

                            // TODO add error to queue
                            return Err(StdError::generic_err(error_msg));
                        }
                    }
                    "execute_tokens_to_user" => {
                        let out: MsgExecuteContractResponse = decode_message_response(&item.data)?;
                        api.debug(format!("Tokens to user: {:?}", out).as_str());
                        return Ok(Response::default());
                    }
                    "execute_claim" => {
                        let out: MsgExecuteContractResponse = decode_message_response(&item.data)?;
                        api.debug(format!("Claiming tokens: {:?}", out).as_str());

                        if let Some(amount) = payload.amount {
                            let res = execute_return_funds(
                                &mut deps,
                                env.clone(),
                                payload.sender.clone(),
                                amount,
                            );

                            if res.is_err() {
                                let error_msg = "WASMDEBUG: Error: Unable to execute_return_funds";
                                api.debug(error_msg);

                                // TODO add error to queue
                                return Err(StdError::generic_err(error_msg));
                            }

                            return Ok(Response::default());
                        }
                        return Err(StdError::generic_err("amount not found"));
                    }
                    "execute_return_funds" => {
                        let out: MsgExecuteContractResponse = decode_message_response(&item.data)?;
                        api.debug(format!("Claiming tokens: {:?}", out).as_str());

                        if let Some(amount) = payload.amount {
                            let res = execute_return_funds_user(
                                deps.storage,
                                env.clone(),
                                payload.sender.clone(),
                                amount,
                            );

                            if res.is_err() {
                                let error_msg =
                                    "WASMDEBUG: Error: Unable to execute_return_funds_user";
                                api.debug(error_msg);

                                // TODO add error to queue
                                return Err(StdError::generic_err(error_msg));
                            }

                            return Ok(Response::default());
                        }
                        return Err(StdError::generic_err("amount not found"));
                    }
                    _ => {
                        let error_msg = "WASMDEBUG: Error: Unknown executor";
                        api.debug(error_msg);
                        return Err(StdError::generic_err(error_msg));
                    }
                },
                "/ibc.applications.transfer.v1.MsgTransfer" => match payload.executor.as_str() {
                    "execute_fund_to_osmosis" => {
                        let out: MsgTransferResponse = decode_message_response(&item.data)?;
                        api.debug(format!("Transferred to osmosis: {:?}", out).as_str());
                        execute_ic_swap(deps, env, info, None);
                        return Ok(Response::default());
                    }
                },
                _ => {
                    let error_msg = "WASMDEBUG: Error: Unknown message type";
                    api.debug(error_msg);
                    return Err(StdError::generic_err(error_msg));
                }
            }
        }

        // update but also check that we don't update same seq_id twice
        ACKNOWLEDGEMENT_RESULTS.update(
            deps.storage,
            (payload.port_id, seq_id),
            |maybe_ack| -> StdResult<AcknowledgementResult> {
                match maybe_ack {
                    Some(_ack) => Err(StdError::generic_err("trying to update same seq_id")),
                    None => Ok(AcknowledgementResult::Success(item_types)),
                }
            },
        )?;

        Ok(Response::default())
    } else {
        let error_msg = "WASMDEBUG: Error: Unable to read sudo payload";
        api.debug(error_msg);
        add_error_to_queue(deps.storage, error_msg.to_string());
        Ok(Response::default())
    }
}

fn sudo_timeout(
    deps: DepsMut<NeutronQuery>,
    _env: Env,
    request: RequestPacket,
) -> StdResult<Response> {
    deps.api
        .debug(format!("WASMDEBUG: sudo timeout request: {:?}", request).as_str());

    // WARNING: RETURNING THIS ERROR CLOSES THE CHANNEL.
    // AN ALTERNATIVE IS TO MAINTAIN AN ERRORS QUEUE AND PUT THE FAILED REQUEST THERE
    // FOR LATER INSPECTION.
    // In this particular case, we return an error because not having the sequence id
    // in the request value implies that a fatal error occurred on Neutron side.
    let seq_id = request
        .sequence
        .ok_or_else(|| StdError::generic_err("sequence not found"))?;

    // WARNING: RETURNING THIS ERROR CLOSES THE CHANNEL.
    // AN ALTERNATIVE IS TO MAINTAIN AN ERRORS QUEUE AND PUT THE FAILED REQUEST THERE
    // FOR LATER INSPECTION.
    // In this particular case, we return an error because not having the sequence id
    // in the request value implies that a fatal error occurred on Neutron side.
    let channel_id = request
        .source_channel
        .ok_or_else(|| StdError::generic_err("channel_id not found"))?;

    // update but also check that we don't update same seq_id twice
    // NOTE: NO ERROR IS RETURNED HERE. THE CHANNEL LIVES ON.
    // In this particular example, this is a matter of developer's choice. Not being able to read
    // the payload here means that there was a problem with the contract while submitting an
    // interchain transaction. You can decide that this is not worth killing the channel,
    // write an error log and / or save the acknowledgement to an errors queue for later manual
    // processing. The decision is based purely on your application logic.
    // Please be careful because it may lead to an unexpected state changes because state might
    // has been changed before this call and will not be reverted because of supressed error.
    let payload = read_sudo_payload(deps.storage, channel_id, seq_id).ok();
    if let Some(payload) = payload {
        // update but also check that we don't update same seq_id twice
        ACKNOWLEDGEMENT_RESULTS.update(
            deps.storage,
            (payload.port_id, seq_id),
            |maybe_ack| -> StdResult<AcknowledgementResult> {
                match maybe_ack {
                    Some(_ack) => Err(StdError::generic_err("trying to update same seq_id")),
                    None => Ok(AcknowledgementResult::Timeout(payload.message)),
                }
            },
        )?;
    } else {
        let error_msg = "WASMDEBUG: Error: Unable to read sudo payload";
        deps.api.debug(error_msg);
        add_error_to_queue(deps.storage, error_msg.to_string());
    }

    Ok(Response::default())
}

fn sudo_error(
    deps: DepsMut<NeutronQuery>,
    request: RequestPacket,
    details: String,
) -> StdResult<Response> {
    deps.api
        .debug(format!("WASMDEBUG: sudo error: {}", details).as_str());
    deps.api
        .debug(format!("WASMDEBUG: request packet: {:?}", request).as_str());

    // WARNING: RETURNING THIS ERROR CLOSES THE CHANNEL.
    // AN ALTERNATIVE IS TO MAINTAIN AN ERRORS QUEUE AND PUT THE FAILED REQUEST THERE
    // FOR LATER INSPECTION.
    // In this particular case, we return an error because not having the sequence id
    // in the request value implies that a fatal error occurred on Neutron side.
    let seq_id = request
        .sequence
        .ok_or_else(|| StdError::generic_err("sequence not found"))?;

    // WARNING: RETURNING THIS ERROR CLOSES THE CHANNEL.
    // AN ALTERNATIVE IS TO MAINTAIN AN ERRORS QUEUE AND PUT THE FAILED REQUEST THERE
    // FOR LATER INSPECTION.
    // In this particular case, we return an error because not having the sequence id
    // in the request value implies that a fatal error occurred on Neutron side.
    let channel_id = request
        .source_channel
        .ok_or_else(|| StdError::generic_err("channel_id not found"))?;
    let payload = read_sudo_payload(deps.storage, channel_id, seq_id).ok();

    if let Some(payload) = payload {
        // update but also check that we don't update same seq_id twice
        ACKNOWLEDGEMENT_RESULTS.update(
            deps.storage,
            (payload.port_id, seq_id),
            |maybe_ack| -> StdResult<AcknowledgementResult> {
                match maybe_ack {
                    Some(_ack) => Err(StdError::generic_err("trying to update same seq_id")),
                    None => Ok(AcknowledgementResult::Error((payload.message, details))),
                }
            },
        )?;
    } else {
        let error_msg = "WASMDEBUG: Error: Unable to read sudo payload";
        deps.api.debug(error_msg);
        add_error_to_queue(deps.storage, error_msg.to_string());
    }

    Ok(Response::default())
}

// prepare_sudo_payload is called from reply handler
// The method is used to extract sequence id and channel from SubmitTxResponse to process sudo payload defined in msg_with_sudo_callback later in Sudo handler.
// Such flow msg_with_sudo_callback() -> reply() -> prepare_sudo_payload() -> sudo() allows you "attach" some payload to your SubmitTx message
// and process this payload when an acknowledgement for the SubmitTx message is received in Sudo handler
fn prepare_sudo_payload(mut deps: DepsMut, _env: Env, msg: Reply) -> StdResult<Response> {
    let payload = read_reply_payload(deps.storage)?;
    let resp: MsgSubmitTxResponse = serde_json_wasm::from_slice(
        msg.result
            .into_result()
            .map_err(StdError::generic_err)?
            .data
            .ok_or_else(|| StdError::generic_err("no result"))?
            .as_slice(),
    )
    .map_err(|e| StdError::generic_err(format!("failed to parse response: {:?}", e)))?;
    deps.api
        .debug(format!("WASMDEBUG: reply msg: {:?}", resp).as_str());
    let seq_id = resp.sequence_id;
    let channel_id = resp.channel;
    save_sudo_payload(deps.branch().storage, channel_id, seq_id, payload)?;
    Ok(Response::new())
}

fn get_ica(
    deps: Deps<impl CustomQuery>,
    env: &Env,
    interchain_account_id: &str,
) -> Result<(String, String), StdError> {
    let key = get_port_id(env.contract.address.as_str(), interchain_account_id);

    INTERCHAIN_ACCOUNTS
        .load(deps.storage, key)?
        .ok_or_else(|| StdError::generic_err("Interchain account is not created yet"))
}

#[entry_point]
pub fn reply(deps: DepsMut, env: Env, msg: Reply) -> StdResult<Response> {
    deps.api
        .debug(format!("WASMDEBUG: reply msg: {:?}", msg).as_str());
    match msg.id {
        SUDO_PAYLOAD_REPLY_ID => prepare_sudo_payload(deps, env, msg),
        _ => Err(StdError::generic_err(format!(
            "unsupported reply message id {}",
            msg.id
        ))),
    }
}

fn min_ntrn_ibc_fee(fee: IbcFee) -> IbcFee {
    IbcFee {
        recv_fee: fee.recv_fee,
        ack_fee: fee
            .ack_fee
            .into_iter()
            .filter(|a| a.denom == FEE_DENOM)
            .collect(),
        timeout_fee: fee
            .timeout_fee
            .into_iter()
            .filter(|a| a.denom == FEE_DENOM)
            .collect(),
    }
}
