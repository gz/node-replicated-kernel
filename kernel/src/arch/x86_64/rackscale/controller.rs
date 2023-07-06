// Copyright © 2022 University of Colorado. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::Cell;
use fallible_collections::FallibleVecGlobal;
use smoltcp::time::Instant;

use rpc::api::RPCServer;
use rpc::rpc::RPCType;
use rpc::server::Server;
use rpc::transport::TCPTransport;

use crate::cmdline::Transport;
use crate::transport::ethernet::ETHERNET_IFACE;
use crate::transport::shmem::create_shmem_transport;

use super::*;

pub(crate) const CONTROLLER_PORT_BASE: u16 = 6970;

/// Controller main method
pub(crate) fn run() {
    // Create network interface and clock
    #[derive(Debug)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    pub(crate) struct Clock(Cell<Instant>);

    impl Clock {
        fn new() -> Clock {
            let rt = rawtime::Instant::now().as_nanos();
            let rt_millis = (rt / 1_000_000) as i64;
            Clock(Cell::new(Instant::from_millis(rt_millis)))
        }

        fn elapsed(&self) -> Instant {
            self.0.get()
        }
    }
    let clock = Clock::new();

    // Initialize one server per client
    let num_clients = *crate::environment::NUM_MACHINES - 1;
    let mut servers: Vec<Box<dyn RPCServer<ControllerState>>> =
        Vec::try_with_capacity(num_clients as usize)
            .expect("Failed to allocate vector for RPC server");

    if crate::CMDLINE
        .get()
        .map_or(false, |c| c.transport == Transport::Ethernet)
    {
        for mid in 0..num_clients {
            let transport = Box::try_new(
                TCPTransport::new(
                    None,
                    CONTROLLER_PORT_BASE + mid as u16,
                    Arc::clone(&ETHERNET_IFACE),
                )
                .expect("Failed to create TCP transport"),
            )
            .expect("Out of memory during init");
            let mut server: Box<dyn RPCServer<ControllerState>> =
                Box::try_new(Server::new(transport)).expect("Out of memory during init");
            register_rpcs(&mut server);
            servers.push(server);
        }
    } else if crate::CMDLINE
        .get()
        .map_or(false, |c| c.transport == Transport::Shmem)
    {
        for mid in 1..=num_clients {
            let transport = Box::try_new(
                create_shmem_transport(mid.try_into().unwrap())
                    .expect("Failed to create shmem transport"),
            )
            .expect("Out of memory during init");

            let mut server: Box<dyn RPCServer<ControllerState>> =
                Box::try_new(Server::new(transport)).expect("Out of memory during init");
            register_rpcs(&mut server);
            servers.push(server);
        }
    } else {
        unreachable!("No supported transport layer specified in kernel argument");
    }

    let mut controller_state = ControllerState::new(num_clients as usize);

    for server in servers.iter_mut() {
        controller_state = server
            .add_client(&CLIENT_REGISTRAR, controller_state)
            .expect("Failed to connect to remote server");
    }

    #[cfg(feature = "test-controller-shmem-alloc")]
    {
        // We don't put this in integration.rs because it must happen midway-through controller initialization
        use crate::arch::debug::shutdown;
        use crate::arch::rackscale::dcm::affinity_alloc::dcm_affinity_alloc;
        use crate::memory::shmem_affinity::mid_to_shmem_affinity;
        use crate::transport::shmem::SHMEM;
        use crate::ExitReason;

        let LARGE_PAGES_PER_CLIENT = 2;

        for mid in 1..(num_clients + 1) {
            let regions = dcm_affinity_alloc(mid, LARGE_PAGES_PER_CLIENT)
                .expect("Controller failed to allocate!");
            assert!(regions.len() == LARGE_PAGES_PER_CLIENT);
            for i in 0..LARGE_PAGES_PER_CLIENT {
                assert!(
                    regions[i].base >= SHMEM.devices[mid].region.base.as_u64()
                        && regions[i].base
                            < SHMEM.devices[mid].region.base.as_u64()
                                + SHMEM.devices[mid].region.size as u64
                );
                assert!(regions[i].affinity == mid_to_shmem_affinity(mid));
            }
        }
        log::info!("controller_shmem_alloc OK");
        shutdown(ExitReason::Ok);
    }

    // Start running the RPC server
    log::info!("Starting RPC server!");
    loop {
        match ETHERNET_IFACE.lock().poll(Instant::from_millis(
            rawtime::duration_since_boot().as_millis() as i64,
        )) {
            Ok(_) => {}
            Err(e) => {
                log::warn!("poll error: {}", e);
            }
        }

        // Try to handle an RPC request
        for server in servers.iter() {
            let (mut new_state, _handled) = server
                .try_handle(controller_state)
                .expect("Controller failed to handle RPC");
            controller_state = new_state;
        }
    }
}

fn register_rpcs(server: &mut Box<dyn RPCServer<ControllerState>>) {
    // Register all of the RPC functions supported
    server
        .register(KernelRpc::Close as RPCType, &CLOSE_HANDLER)
        .unwrap();
    server
        .register(KernelRpc::Delete as RPCType, &DELETE_HANDLER)
        .unwrap();
    server
        .register(KernelRpc::GetInfo as RPCType, &GETINFO_HANDLER)
        .unwrap();
    server
        .register(KernelRpc::MkDir as RPCType, &MKDIR_HANDLER)
        .unwrap();
    server
        .register(KernelRpc::Open as RPCType, &OPEN_HANDLER)
        .unwrap();
    server
        .register(KernelRpc::FileRename as RPCType, &RENAME_HANDLER)
        .unwrap();
    server
        .register(KernelRpc::Write as RPCType, &WRITE_HANDLER)
        .unwrap();
    server
        .register(KernelRpc::WriteAt as RPCType, &WRITE_HANDLER)
        .unwrap();
    server
        .register(KernelRpc::Read as RPCType, &READ_HANDLER)
        .unwrap();
    server
        .register(KernelRpc::ReadAt as RPCType, &READ_HANDLER)
        .unwrap();
    server
        .register(KernelRpc::Log as RPCType, &LOG_HANDLER)
        .unwrap();
    server
        .register(
            KernelRpc::AllocatePhysical as RPCType,
            &ALLOCATE_PHYSICAL_HANDLER,
        )
        .unwrap();
    server
        .register(
            KernelRpc::ReleasePhysical as RPCType,
            &RELEASE_PHYSICAL_HANDLER,
        )
        .unwrap();
    server
        .register(KernelRpc::RequestCore as RPCType, &REQUEST_CORE_HANDLER)
        .unwrap();
    server
        .register(
            KernelRpc::GetHardwareThreads as RPCType,
            &GET_HARDWARE_THREADS_HANDLER,
        )
        .unwrap();
    server
        .register(
            KernelRpc::GetShmemStructure as RPCType,
            &GET_SHMEM_STRUCTURE_HANDLER,
        )
        .unwrap();
    server
        .register(
            KernelRpc::GetShmemFrames as RPCType,
            &GET_SHMEM_FRAMES_HANDLER,
        )
        .unwrap();
}
