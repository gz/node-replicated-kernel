// Copyright © 2022 VMware, Inc. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use abomonation::{decode, encode, unsafe_abomonate, Abomonation};
use core2::io::Result as IOResult;
use core2::io::Write;
use kpi::system::MachineId;
use rpc::rpc::*;
use rpc::RPCClient;

use super::super::controller_state::ControllerState;
use super::super::dcm::resource_alloc::dcm_resource_alloc;
use super::super::kernelrpc::*;
use crate::arch::rackscale::CLIENT_STATE;
use crate::error::{KError, KResult};
use crate::memory::VAddr;
use crate::nr::KernelNode;
use crate::process::Pid;

#[derive(Debug, Clone, Copy)]
pub(crate) struct RequestCoreReq {
    pub pid: Pid,
    pub new_pid: bool,
    pub entry_point: u64,
}
unsafe_abomonate!(RequestCoreReq: pid, new_pid, entry_point);

pub(crate) fn rpc_request_core(pid: Pid, new_pid: bool, entry_point: u64) -> KResult<(u64, u64)> {
    log::debug!("RequestCore({:?}, {:?}, {:?})", pid, new_pid, entry_point);

    // Construct request data
    let req = RequestCoreReq {
        pid,
        new_pid,
        entry_point,
    };
    let mut req_data = [0u8; core::mem::size_of::<RequestCoreReq>()];
    unsafe { encode(&req, &mut (&mut req_data).as_mut()) }.expect("Failed to encode core request");

    // Construct result buffer and call RPC
    let mut res_data = [0u8; core::mem::size_of::<KResult<(u64, u64)>>()];
    CLIENT_STATE.rpc_client.lock().call(
        KernelRpc::RequestCore as RPCType,
        &[&req_data],
        &mut [&mut res_data],
    )?;

    // Decode and return the result
    if let Some((res, remaining)) = unsafe { decode::<KResult<(u64, u64)>>(&mut res_data) } {
        if remaining.len() > 0 {
            return Err(KError::from(RPCError::ExtraData));
        }
        log::debug!("RequestCore() {:?}", res);
        *res
    } else {
        Err(KError::from(RPCError::MalformedResponse))
    }
}

// RPC Handler function for delete() RPCs in the controller
pub(crate) fn handle_request_core(
    hdr: &mut RPCHeader,
    payload: &mut [u8],
    state: ControllerState,
) -> Result<ControllerState, RPCError> {
    log::debug!("handle_request_core() start");

    // Parse request
    let core_req = match unsafe { decode::<RequestCoreReq>(payload) } {
        Some((req, _)) => req,
        None => {
            log::error!("Invalid payload for request: {:?}", hdr);
            construct_error_ret(hdr, payload, KError::from(RPCError::MalformedRequest));
            return Ok(state);
        }
    };

    let (mids, _) = dcm_resource_alloc(core_req.pid, 1, 0);
    let mid = mids[0];

    let (gtid, gtid_affinity) = {
        let mut client_state = state.get_client_state(mid).lock();

        // TODO(performance): controller chooses a core id - right now, sequentially for cores on the machine.
        // it should really choose in a NUMA-aware fashion for the remote node.
        let mut gtid = None;
        let mut gtid_affinity = None;
        for i in 0..client_state.hw_threads.len() {
            match client_state.hw_threads[i] {
                (thread, false) => {
                    gtid = Some(thread.id);
                    gtid_affinity = Some(thread.node_id);
                    client_state.hw_threads[i] = (thread, true);
                    break;
                }
                _ => continue,
            }
        }
        // gtid should always be found, as DCM should know if there are free threads or not.
        let gtid = gtid.expect("Failed to find free thread??");
        let gtid_affinity = gtid_affinity.expect("Failed to find thread node affinity?");
        (gtid, gtid_affinity)
    };

    log::debug!(
        "Found unused thread: machine={:?}, gtid={:?} node={:?}",
        kpi::system::mid_from_gtid(gtid),
        kpi::system::mtid_from_gtid(gtid),
        gtid_affinity,
    );

    let ret = KernelNode::allocate_core_to_process(
        core_req.pid,
        VAddr(core_req.entry_point),
        Some(gtid_affinity),
        Some(gtid),
    );

    match ret {
        Ok(_) => {
            if core_req.new_pid {
                crate::fs::cnrfs::MlnrKernelNode::add_process(core_req.pid)
                    .expect("TODO(rackscale, error-handling): revert state");
            }
            construct_ret(hdr, payload, Ok((gtid as u64, 0)));
        }
        Err(err) => {
            construct_error_ret(hdr, payload, err);
        }
    }

    // Construct and return result
    Ok(state)
}
