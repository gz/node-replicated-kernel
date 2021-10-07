// Copyright © 2021 VMware, Inc. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0 OR MIT

#![allow(warnings)]
use core::convert::TryInto;
use core::lazy;
use core::num::NonZeroUsize;

use bit_field::BitField;
use gdbstub::state_machine::{Event, GdbStubStateMachine};
use gdbstub::target::ext::base::multithread::ThreadStopReason;
use gdbstub::target::ext::base::singlethread::{
    SingleThreadOps, SingleThreadSingleStep, SingleThreadSingleStepOps, StopReason,
};
use gdbstub::target::ext::base::{BaseOps, SingleRegisterAccess, SingleRegisterAccessOps};
use gdbstub::target::ext::breakpoints::{
    Breakpoints, BreakpointsOps, HwBreakpoint, HwBreakpointOps, HwWatchpoint, HwWatchpointOps,
    SwBreakpoint, SwBreakpointOps, WatchKind,
};
use gdbstub::target::ext::section_offsets::{Offsets, SectionOffsets, SectionOffsetsOps};
use gdbstub::target::{Target, TargetError, TargetResult};
use gdbstub::{Connection, ConnectionExt, DisconnectReason, GdbStub, GdbStubError};
use gdbstub_arch::x86::reg::id::{X86SegmentRegId, X86_64CoreRegId};
use gdbstub_arch::x86::X86_64_SSE;
use kpi::arch::{StReg, ST_REGS};
use lazy_static::lazy_static;
use log::{debug, error, info, trace, warn};
use spin::Mutex;
use x86::bits64::rflags::RFlags;
use x86::debugregs;
use x86::io;

use super::debug::GDB_REMOTE_PORT;
use crate::arch::memory::KERNEL_BASE;
use crate::error::KError;
use crate::memory::vspace::AddressSpace;
use crate::memory::{VAddr, BASE_PAGE_SIZE};

mod serial;

use serial::{GdbSerial, GdbSerialErr};

/// Indicates the reason for interruption (e.g. a breakpoint was hit).
#[derive(Debug, PartialEq, Eq)]
pub enum KCoreStopReason {
    /// DebugInterrupt was received.
    ///
    /// This normally means we hit a hardware breakpoint, watchpoint, rflags
    /// step-mode was enabled or we are at the start of the kernel program)
    DebugInterrupt,
    /// A breakpoint was hit.
    ///
    /// This usually means gdb fiddled with the instructions and inserted an
    /// `int 3` somewhere it deemed necessary.
    BreakpointInterrupt,
    /// We have received data on the (serial) line between us and gdb.
    ConnectionInterrupt,
}

lazy_static! {
    /// The GDB connection state machine.
    ///
    /// This is a state machine that handles the communication with the GDB.
    pub static ref GDB_STUB: Mutex<Option<GdbStubStateMachine<'static, KernelDebugger, GdbSerial>>> = {
        let connection = wait_for_gdb_connection(GDB_REMOTE_PORT).expect("Can't connect to GDB");
        Mutex::new(Some(gdbstub::GdbStub::new(connection).run_state_machine().expect("Can't start GDB session")))
    };
}

/// Wait until a GDB connection is established (e.g., until we can read
/// something from the serial line).
fn wait_for_gdb_connection(port: u16) -> Result<GdbSerial, KError> {
    let gdb = GdbSerial::new(port);

    info!("Waiting for a GDB connection (I/O port {:#x})...", port);
    // If you modify the next line, you also need to adjust the corresponding
    // line in the `s02_gdb` integration test:
    info!("Use `target remote localhost:1234` in gdb to connect.");

    // Block until a GDB client connects:
    while !gdb.can_read() {
        core::hint::spin_loop();
    }

    // If you modify the next line, you also need to adjust the corresponding
    // line in the `s02_gdb` integration test:
    info!("Debugger connected.");
    Ok(gdb)
}

/// Resume the gdb client connection by passing an optional event for the
/// interruption.
///
/// # Arguments
/// - `resume_with`: Should probably always be Some(reason) except the first
///   time after connecting.
pub fn event_loop(reason: KCoreStopReason) -> Result<(), KError> {
    if GDB_STUB.is_locked() {
        panic!("re-entrant into event_loop!");
    }

    let mut gdb_stm = GDB_STUB.lock().take().unwrap();
    let target = super::kcb::get_kcb()
        .arch
        .kdebug
        .as_mut()
        .expect("Need a target");

    let mut stop_reason = target.determine_stop_reason(reason);
    debug!("event_loop stop_reason {:?}", stop_reason);
    loop {
        gdb_stm = match gdb_stm {
            GdbStubStateMachine::Pump(mut gdb_stm_inner) => {
                //trace!("GdbStubStateMachine::Pump");

                // This means we expect stuff on the serial line (from GDB)
                // Let's read and react to it:
                let conn = gdb_stm_inner.borrow_conn();
                conn.disable_irq();
                let byte = conn.read()?;

                match gdb_stm_inner.pump(target, byte) {
                    Ok((gdb_stm_new, None)) => gdb_stm_new,
                    Ok((_, Some(disconnect_reason))) => {
                        match disconnect_reason {
                            DisconnectReason::Disconnect => info!("GDB Disconnected"),
                            DisconnectReason::TargetExited(_) => info!("Target exited"),
                            DisconnectReason::TargetTerminated(_) => info!("Target halted"),
                            DisconnectReason::Kill => info!("GDB sent a kill command"),
                        }
                        break;
                    }
                    Err(GdbStubError::TargetError(e)) => {
                        error!("Debugger raised a fatal error {:?}", e);
                        break;
                    }
                    Err(e) => {
                        error!("gdbstub internal error {:?}", e);
                        break;
                    }
                }
            }
            GdbStubStateMachine::DeferredStopReason(mut gdb_stm_inner) => {
                //trace!("GdbStubStateMachine::DeferredStopReason");

                // If we're here we were running but have stopped now (either
                // because we hit Ctrl+c in gdb and hence got a serial interrupt
                // or we hit a breakpoint).
                let conn = gdb_stm_inner.borrow_conn();
                conn.disable_irq();
                let data_to_read = conn.peek().unwrap().is_some();

                if data_to_read {
                    let byte = gdb_stm_inner.borrow_conn().read().unwrap();
                    match gdb_stm_inner.pump(target, byte) {
                        Ok((pumped_stm, e)) => match e {
                            Event::None => GdbStubStateMachine::DeferredStopReason(pumped_stm),
                            Event::CtrlCInterrupt => {
                                // if the target wants to handle the interrupt, report the
                                // stop reason
                                if let Some(stop_reason) = stop_reason.take() {
                                    assert_eq!(stop_reason, ThreadStopReason::Signal(5));
                                    match pumped_stm
                                        .deferred_stop_reason(target, stop_reason)
                                        .unwrap()
                                    {
                                        (_, Some(disconnect_reason)) => {
                                            info!("disconnect reason {:?}", disconnect_reason);
                                            break;
                                        }
                                        (gdb, None) => gdb,
                                    }
                                } else {
                                    pumped_stm.into()
                                }
                            }
                            Event::Disconnect(reason) => {
                                info!("GDB client disconnected: {:?}", reason);
                                break;
                            }
                        },
                        Err(GdbStubError::TargetError(e)) => {
                            error!("Target raised a fatal error: {:?}", e);
                            break;
                        }
                        Err(e) => {
                            error!("gdbstub error in DeferredStopReason.pump: {:?}", e);
                            break;
                        }
                    }
                } else if let Some(reason) = stop_reason.take() {
                    match gdb_stm_inner.deferred_stop_reason(target, reason) {
                        Ok((gdb_stm_new, None)) => gdb_stm_new,
                        Ok((_, Some(disconnect_reason))) => {
                            match disconnect_reason {
                                DisconnectReason::Disconnect => info!("GDB Disconnected"),
                                DisconnectReason::TargetExited(_) => info!("Target exited"),
                                DisconnectReason::TargetTerminated(_) => info!("Target halted"),
                                DisconnectReason::Kill => info!("GDB sent a kill command"),
                            }
                            break;
                        }
                        Err(GdbStubError::TargetError(e)) => {
                            error!("Target raised a fatal error {:?}", e);
                            break;
                        }
                        Err(e) => {
                            error!("gdbstub internal error {:?}", e);
                            break;
                        }
                    }
                } else if target.resume_with.is_some() {
                    // We don't have a `stop_reason` and we don't have something
                    // to read on the line. This probably means we're done and
                    // we should run again.
                    conn.enable_irq();
                    let r = GDB_STUB
                        .lock()
                        .replace(GdbStubStateMachine::DeferredStopReason(gdb_stm_inner));
                    assert!(
                        r.is_none(),
                        "Put something in GDB_STUB which we shouldn't have..."
                    );
                    break;
                } else {
                    unreachable!("Can't happen?");
                }
            }
        }
    }

    match target.resume_with.take() {
        Some(ExecMode::Continue) => {
            //trace!("Resume execution.");
            let kcb = super::kcb::get_kcb();
            // If we were stepping, we need to remove the TF bit again for resuming
            if let Some(saved) = &mut kcb.arch.save_area {
                let mut rflags = RFlags::from_bits_truncate(saved.rflags);
                rflags.remove(x86::bits64::rflags::RFlags::FLAGS_TF);
                saved.rflags = rflags.bits();
            }
        }
        Some(ExecMode::SingleStep) => {
            trace!("Step execution, set TF flag.");
            let kcb = super::kcb::get_kcb();
            if let Some(saved) = &mut kcb.arch.save_area {
                saved.rflags |= RFlags::FLAGS_TF.bits();
            }
        }
        _ => {
            unimplemented!("Resume strategy not handled...");
        }
    }

    Ok(())
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum ExecMode {
    /// Continue program unconditionally
    Continue,
    SingleStep,
}

/// What kind of breakpoint GDB is trying to set.
///
/// Heads-up we're using hardware breakpoint either way.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum BreakRequest {
    /// GDB requested a hardware breakpoint
    Hardware,
    /// GDB requested a software breakpoint
    Software,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum BreakType {
    /// For instructions
    Breakpoint,
    /// For data access/writes
    Watchpoint(WatchKind),
}

/// Keeps information about any breakpoints we've set.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct BreakState(VAddr, BreakType, BreakRequest);

/// A kernel level debug implementation that can interface with GDB over remote
/// serial protocol.
pub struct KernelDebugger {
    /// Maintains meta-data about our hardware breakpoint registers.
    hw_break_points: [Option<BreakState>; 4],
    /// Resume program with this signal (if needed).
    signal: Option<u8>,
    /// How we resume the program (set by gdbstub in resume or step).
    resume_with: Option<ExecMode>,
}

impl KernelDebugger {
    pub fn new() -> Self {
        Self {
            hw_break_points: [None; 4],
            resume_with: None,
            signal: None,
        }
    }

    fn add_breakpoint(
        &mut self,
        req: BreakRequest,
        addr: u64,
        _kind: usize,
    ) -> TargetResult<bool, Self> {
        let kcb = super::kcb::get_kcb();
        trace!(
            "add_breakpoint {:#x} (in ELF: {:#x}",
            addr,
            addr - kcb.arch.kernel_args().kernel_elf_offset.as_u64(),
        );
        let sa = kcb
            .arch
            .save_area
            .as_mut()
            .expect("Need to have a save area");

        for (idx, (reg, entry)) in debugregs::BREAKPOINT_REGS
            .iter()
            .zip(self.hw_break_points.iter_mut())
            .enumerate()
        {
            if entry.is_none() {
                *entry = Some(BreakState(VAddr::from(addr), BreakType::Breakpoint, req));

                // Safety: We're in CPL0.
                //
                // TODO: For more safety we should sanitize the address to make
                // sure we can't set it inside certain regions (on code that is
                // used by the gdb module etc.)
                unsafe {
                    // break size has to be Bytes1 on x86 for instructions, so I
                    // think we can ignore the `_kind` arg
                    reg.configure(
                        addr.try_into().unwrap(),
                        debugregs::BreakCondition::Instructions,
                        debugregs::BreakSize::Bytes1,
                    );
                }

                // Don't enable the BP, but mark register as "to enable" for the
                // context restore logic:
                sa.enabled_bps.set_bit(idx, true);
                info!("sa.enabled_bps {:#b}", sa.enabled_bps);
                return Ok(true);
            }
        }

        warn!("No more debug registers available for add_hw_breakpoint");
        Ok(false)
    }

    fn remove_breakpoint(
        &mut self,
        req: BreakRequest,
        addr: u64,
        _kind: usize,
    ) -> TargetResult<bool, Self> {
        let kcb = super::kcb::get_kcb();
        trace!(
            "remove_breakpoint {:#x} (in ELF: {:#x}",
            addr,
            addr - kcb.arch.kernel_args().kernel_elf_offset.as_u64(),
        );
        let sa = kcb
            .arch
            .save_area
            .as_mut()
            .expect("Need to have a save area");

        for (idx, (reg, entry)) in debugregs::BREAKPOINT_REGS
            .iter()
            .zip(self.hw_break_points.iter_mut())
            .enumerate()
        {
            if let Some(BreakState(entry_addr, BreakType::Breakpoint, entry_req)) = entry {
                if entry_addr.as_u64() == addr && *entry_req == req {
                    unsafe {
                        reg.disable_global();
                    }
                    *entry = None;
                    sa.enabled_bps.set_bit(idx, false);
                    return Ok(true);
                }
            }
        }

        // No break point matching the address was found
        warn!("Unable to remove hw breakpoint for addr {:#x}", addr);
        Ok(false)
    }

    /// Figures out why a core got a debug interrupt by looking through the
    /// hardware debug register and reading which one was hit.
    ///
    // Also does some additional stuff like re-enabling the breakpoints.
    fn determine_stop_reason(&mut self, reason: KCoreStopReason) -> Option<ThreadStopReason<u64>> {
        match reason {
            KCoreStopReason::ConnectionInterrupt => Some(ThreadStopReason::Signal(5)),
            KCoreStopReason::BreakpointInterrupt => {
                unimplemented!("Breakpoint interrupt not implemented");
                Some(ThreadStopReason::HwBreak(NonZeroUsize::new(1).unwrap()))
            }
            KCoreStopReason::DebugInterrupt => {
                // Safety: We are in the kernel so we can access dr6.
                let mut dr6 = unsafe { debugregs::dr6() };

                let bp = if dr6.contains(debugregs::Dr6::B0) {
                    dr6.remove(debugregs::Dr6::B0);
                    self.hw_break_points[0]
                } else if dr6.contains(debugregs::Dr6::B1) {
                    dr6.remove(debugregs::Dr6::B1);
                    self.hw_break_points[1]
                } else if dr6.contains(debugregs::Dr6::B2) {
                    dr6.remove(debugregs::Dr6::B2);
                    self.hw_break_points[2]
                } else if dr6.contains(debugregs::Dr6::B3) {
                    dr6.remove(debugregs::Dr6::B3);
                    self.hw_break_points[3]
                } else {
                    // If None, we are either single-stepping (debugregs::Dr6::BS,
                    // handled below) or are at the start of the kernel (no
                    // breakpoints were set yet)
                    None
                };

                // Map things to a gdbstub stop reason:
                let stop: Option<ThreadStopReason<u64>> =
                    if let Some(BreakState(va, BreakType::Breakpoint, BreakRequest::Hardware)) = bp
                    {
                        Some(ThreadStopReason::HwBreak(NonZeroUsize::new(1).unwrap()))
                    } else if let Some(BreakState(
                        va,
                        BreakType::Breakpoint,
                        BreakRequest::Software,
                    )) = bp
                    {
                        Some(ThreadStopReason::SwBreak(NonZeroUsize::new(1).unwrap()))
                    } else if let Some(BreakState(
                        va,
                        BreakType::Watchpoint(kind),
                        BreakRequest::Hardware,
                    )) = bp
                    {
                        Some(ThreadStopReason::Watch {
                            tid: NonZeroUsize::new(1).unwrap(),
                            kind,
                            addr: va.as_u64(),
                        })
                    } else if let Some(BreakState(
                        va,
                        BreakType::Watchpoint(kind),
                        BreakRequest::Software,
                    )) = bp
                    {
                        unimplemented!("Shouldn't have ever set a software watchpoint!")
                    } else if dr6.contains(debugregs::Dr6::BS) {
                        // When the BS flag is set, any of the other debug status bits also may be set.
                        dr6.remove(debugregs::Dr6::BS);
                        Some(ThreadStopReason::DoneStep)
                    } else {
                        None
                    };

                // Sanity check that we indeed handled all bits in dr6 (except RTM
                // which isn't a condition):
                if !dr6.difference(debugregs::Dr6::RTM).is_empty() {
                    // The code assumes that only one BP (or BS) condition gets set
                    // for a single IRQ. If multiple conditions can trigger per IRQ,
                    // we need to rethink this code.
                    //
                    // Maybe we have to handle every potential flag in dr6 in some
                    // loop one after the other, or ignore some? For now, we just
                    // log and then clear/ignore the unhandled bits:
                    error!("Unhandled/ignored these conditions in dr6: {:?}", dr6);
                    dr6 = debugregs::Dr6::empty(); // This will clear RTM (we don't use this atm.)
                }

                unsafe { debugregs::dr6_write(dr6) };

                stop
            }
        }
    }
}

impl Target for KernelDebugger {
    type Error = KError;
    type Arch = X86_64_SSE;

    fn base_ops(&mut self) -> BaseOps<Self::Arch, Self::Error> {
        BaseOps::SingleThread(self)
    }

    fn section_offsets(&mut self) -> Option<SectionOffsetsOps<Self>> {
        Some(self)
    }

    fn breakpoints(&mut self) -> Option<BreakpointsOps<Self>> {
        Some(self)
    }

    fn use_x_upcase_packet(&self) -> bool {
        true
    }
}

impl Breakpoints for KernelDebugger {
    fn sw_breakpoint(&mut self) -> Option<SwBreakpointOps<Self>> {
        Some(self)
    }

    fn hw_breakpoint(&mut self) -> Option<HwBreakpointOps<Self>> {
        Some(self)
    }

    fn hw_watchpoint(&mut self) -> Option<HwWatchpointOps<Self>> {
        Some(self)
    }
}

impl SingleThreadSingleStep for KernelDebugger {
    fn step(&mut self, signal: Option<u8>) -> Result<(), Self::Error> {
        assert!(signal.is_none(), "Not supported at the moment.");

        self.signal = signal;
        self.resume_with = Some(ExecMode::SingleStep);
        info!(
            "set signal = {:?} resume_with =  {:?}",
            signal, self.resume_with
        );

        Ok(())
    }
}

impl SingleThreadOps for KernelDebugger {
    fn support_single_step(&mut self) -> Option<SingleThreadSingleStepOps<Self>> {
        Some(self)
        //None
    }

    fn resume(&mut self, signal: Option<u8>) -> Result<(), Self::Error> {
        assert!(signal.is_none(), "Not supported at the moment.");

        self.signal = signal;
        self.resume_with = Some(ExecMode::Continue);
        trace!(
            "resume: signal = {:?} resume_with =  {:?}",
            signal,
            self.resume_with
        );

        // If the target is running under the more advanced GdbStubStateMachine
        // API, it is possible to "defer" reporting a stop reason to some point
        // outside of the resume implementation by returning None.
        Ok(())
    }

    fn read_registers(
        &mut self,
        regs: &mut gdbstub_arch::x86::reg::X86_64CoreRegs,
    ) -> TargetResult<(), Self> {
        let kcb = super::kcb::get_kcb();
        if let Some(saved) = &kcb.arch.save_area {
            // RAX, RBX, RCX, RDX, RSI, RDI, RBP, RSP, r8-r15
            regs.regs[00] = saved.rax;
            regs.regs[01] = saved.rbx;
            regs.regs[02] = saved.rcx;
            regs.regs[03] = saved.rdx;
            regs.regs[04] = saved.rsi;
            regs.regs[05] = saved.rdi;
            regs.regs[06] = saved.rbp;
            regs.regs[07] = saved.rsp;
            regs.regs[08] = saved.r8;
            regs.regs[09] = saved.r9;
            regs.regs[10] = saved.r10;
            regs.regs[11] = saved.r11;
            regs.regs[12] = saved.r12;
            regs.regs[13] = saved.r13;
            regs.regs[14] = saved.r14;
            regs.regs[15] = saved.r15;

            regs.rip = saved.rip;
            regs.eflags = saved.rflags.try_into().unwrap();

            // Segment registers: CS, SS, DS, ES, FS, GS
            regs.segments = gdbstub_arch::x86::reg::X86SegmentRegs {
                cs: saved.cs.try_into().unwrap(),
                ss: saved.ss.try_into().unwrap(),
                ds: 0x0, // Don't bother with ds; should be irrelevant on 64bit
                es: 0x0, // Don't bother with es; should be irrelevant on 64bit
                fs: saved.fs.try_into().unwrap(),
                gs: saved.gs.try_into().unwrap(),
            };

            // FPU registers: ST0 through ST7
            for (i, reg) in ST_REGS.iter().enumerate() {
                regs.st[i] = saved.fxsave.st(*reg);
            }

            // FPU internal registers
            regs.fpu.fctrl = saved.fxsave.fcw.try_into().unwrap();
            regs.fpu.fstat = saved.fxsave.fsw.try_into().unwrap();
            regs.fpu.ftag = saved.fxsave.ftw.try_into().unwrap();
            regs.fpu.fiseg = saved.fxsave.fcs.try_into().unwrap();
            regs.fpu.fioff = saved.fxsave.fip.try_into().unwrap();
            regs.fpu.foseg = saved.fxsave.fds.try_into().unwrap();
            regs.fpu.fooff = saved.fxsave.fdp.try_into().unwrap();
            regs.fpu.fop = saved.fxsave.fop.try_into().unwrap();

            // SIMD Registers: XMM0 through XMM15
            regs.xmm = saved.fxsave.xmm;

            // SSE Status/Control Register
            regs.mxcsr = saved.fxsave.mxcsr;
        }

        trace!("read_registers {:02X?}", regs);
        Ok(())
    }

    fn write_registers(
        &mut self,
        regs: &gdbstub_arch::x86::reg::X86_64CoreRegs,
    ) -> TargetResult<(), Self> {
        trace!("write_registers {:?}", regs);
        let kcb = super::kcb::get_kcb();
        if let Some(saved) = &mut kcb.arch.save_area {
            // RAX, RBX, RCX, RDX, RSI, RDI, RBP, RSP, r8-r15
            saved.rax = regs.regs[00];
            saved.rbx = regs.regs[01];
            saved.rcx = regs.regs[02];
            saved.rdx = regs.regs[03];
            saved.rsi = regs.regs[04];
            saved.rdi = regs.regs[05];
            saved.rbp = regs.regs[06];
            saved.rsp = regs.regs[07];
            saved.r8 = regs.regs[08];
            saved.r9 = regs.regs[09];
            saved.r10 = regs.regs[10];
            saved.r11 = regs.regs[11];
            saved.r12 = regs.regs[12];
            saved.r13 = regs.regs[13];
            saved.r14 = regs.regs[14];
            saved.r15 = regs.regs[15];

            saved.rip = regs.rip;
            saved.rflags = regs.eflags.try_into().unwrap();

            // Segment registers: CS, SS, DS, ES, FS, GS
            saved.cs = regs.segments.cs.try_into().unwrap();
            saved.ss = regs.segments.ss.try_into().unwrap();
            // ignore ss, ds because we don't use it.
            // saved.ds = regs.segments.ds.try_into().unwrap();
            // saved.es = regs.segments.es.try_into().unwrap();
            saved.fs = regs.segments.fs.try_into().unwrap();
            saved.gs = regs.segments.gs.try_into().unwrap();

            // FPU registers: ST0 through ST7
            for (i, reg) in ST_REGS.iter().enumerate() {
                //regs.st[i] = saved.fxsave.st(*reg);
            }

            // FPU internal registers
            saved.fxsave.fcw = regs.fpu.fctrl.try_into().unwrap();
            saved.fxsave.fsw = regs.fpu.fstat.try_into().unwrap();
            saved.fxsave.ftw = regs.fpu.ftag.try_into().unwrap();
            saved.fxsave.fcs = regs.fpu.fiseg.try_into().unwrap();
            saved.fxsave.fip = regs.fpu.fioff.try_into().unwrap();
            saved.fxsave.fds = regs.fpu.foseg.try_into().unwrap();
            saved.fxsave.fdp = regs.fpu.fooff.try_into().unwrap();
            saved.fxsave.fop = regs.fpu.fop.try_into().unwrap();

            // SIMD Registers: XMM0 through XMM15
            saved.fxsave.xmm = regs.xmm;

            // SSE Status/Control Register
            saved.fxsave.mxcsr = regs.mxcsr;
        }
        Ok(())
    }

    fn read_addrs(&mut self, start_addr: u64, data: &mut [u8]) -> TargetResult<(), Self> {
        trace!("read_addr {:#x}", start_addr);
        // (Un)Safety: Well, this can easily violate the rust aliasing model
        // because when we arrive in the debugger; there might some mutable
        // reference to the PTs somewhere in a context that was modifying the
        // PTs. We'll just have to accept this for debugging.
        let pt = unsafe { super::vspace::page_table::ReadOnlyPageTable::current() };

        let start_addr: usize = start_addr.try_into().unwrap();
        if !kpi::arch::VADDR_RANGE.contains(&start_addr) {
            warn!("Address out of range {}", start_addr);
            return Err(TargetError::NonFatal);
        }

        for i in 0..data.len() {
            let va = VAddr::from(start_addr + i);

            // Check access rights for start of every new page we encounter (and
            // for the first byte that might start within a page)
            if i == 0 || (start_addr + i) % BASE_PAGE_SIZE == 0 {
                match pt.resolve(va) {
                    Ok((pa, rights)) => {
                        if !rights.is_readable() {
                            // Mapped but not read-able (this can't really happen would mean
                            // something like swapped out but we don't do that)
                            return Err(TargetError::NonFatal);
                        }
                    }
                    Err(_) => {
                        warn!("Target page was not mapped.");
                        // Target page was not mapped
                        return Err(TargetError::NonFatal);
                    }
                }
            }

            // Read the byte
            let ptr: *const u8 = va.as_ptr();
            // Safety: This should be ok, see all the effort above...
            data[i] = unsafe { *ptr };
        }

        Ok(())
    }

    fn write_addrs(&mut self, start_addr: u64, data: &[u8]) -> TargetResult<(), Self> {
        trace!("write_addrs {:#x}", start_addr);

        // (Un)Safety: Well, this can easily violate the rust aliasing model
        // because when we arrive in the debugger; there might some mutable
        // reference to the PTs somewhere in a context that was modifying the
        // PTs. We'll just have to accept this for debugging.
        let pt = unsafe { super::vspace::page_table::ReadOnlyPageTable::current() };

        let start_addr: usize = start_addr.try_into().unwrap();
        for (i, payload) in data.iter().enumerate() {
            let va = VAddr::from(start_addr + i);

            // Check access rights for start of every new page that we encounter
            // (and for the first byte that might start within a page)
            if i == 0 || (start_addr + i) % BASE_PAGE_SIZE == 0 {
                if !kpi::arch::VADDR_RANGE.contains(&start_addr) {
                    warn!("Address out of range {}", start_addr);
                    return Err(TargetError::NonFatal);
                }

                match pt.resolve(va) {
                    Ok((pa, rights)) => {
                        // Not implemented: We don't allow writing in executable
                        // `.text`. This gives some warnings in gdb because it
                        // tries to set breakpoints by writing `int 3` in random
                        // code location. This currently doesn't work, either
                        // because we don't adjust the rip properly or don't
                        // implement the swbreak function or it's generally bad
                        // to overwrite code that is potentially used by the
                        // gdbstub functionality too (like what happens if you
                        // set a breakpoint in the log crate)
                        //
                        // I believe gdb falls back to stepping if the returns
                        // NonFatal.
                        if rights.is_executable() {
                            error!("Can't write to executable page.");
                            error!("If you were trying to set a breakpoint use `hbreak` instead of `break`");
                            return Err(TargetError::NonFatal);
                        }
                        if !rights.is_writable() {
                            debug!("Target page mapped as read-only. Can't write at address.");
                            return Err(TargetError::NonFatal);
                        }
                    }
                    Err(_) => {
                        // Target page was not mapped
                        warn!("Target page not mapped.");
                        return Err(TargetError::NonFatal);
                    }
                }
            }

            // (Un)Safety: gdb writing in random memory locations? Surely this
            // won't end safely.
            let ptr: *mut u8 = va.as_mut_ptr();
            unsafe { *ptr = *payload };
        }

        Ok(())
    }

    fn single_register_access(&mut self) -> Option<SingleRegisterAccessOps<(), Self>> {
        //Some(self)
        None
    }
}

impl SingleRegisterAccess<()> for KernelDebugger {
    fn read_register(
        &mut self,
        tid: (),
        reg_id: X86_64CoreRegId,
        dst: &mut [u8],
    ) -> TargetResult<usize, Self> {
        trace!("read_register {:?}", reg_id);
        let kcb = super::kcb::get_kcb();

        if let Some(saved) = &mut kcb.arch.save_area {
            fn copy_out(dst: &mut [u8], src: &[u8]) -> TargetResult<usize, KernelDebugger> {
                dst.copy_from_slice(src);
                Ok(src.len())
            }

            match reg_id {
                X86_64CoreRegId::Gpr(00) => copy_out(dst, &saved.rax.to_le_bytes()),
                X86_64CoreRegId::Gpr(01) => copy_out(dst, &saved.rbx.to_le_bytes()),
                X86_64CoreRegId::Gpr(02) => copy_out(dst, &saved.rcx.to_le_bytes()),
                X86_64CoreRegId::Gpr(03) => copy_out(dst, &saved.rdx.to_le_bytes()),
                X86_64CoreRegId::Gpr(04) => copy_out(dst, &saved.rsi.to_le_bytes()),
                X86_64CoreRegId::Gpr(05) => copy_out(dst, &saved.rdi.to_le_bytes()),
                X86_64CoreRegId::Gpr(06) => copy_out(dst, &saved.rbp.to_le_bytes()),
                X86_64CoreRegId::Gpr(07) => copy_out(dst, &saved.rsp.to_le_bytes()),
                X86_64CoreRegId::Gpr(08) => copy_out(dst, &saved.r8.to_le_bytes()),
                X86_64CoreRegId::Gpr(09) => copy_out(dst, &saved.r9.to_le_bytes()),
                X86_64CoreRegId::Gpr(10) => copy_out(dst, &saved.r10.to_le_bytes()),
                X86_64CoreRegId::Gpr(11) => copy_out(dst, &saved.r11.to_le_bytes()),
                X86_64CoreRegId::Gpr(12) => copy_out(dst, &saved.r12.to_le_bytes()),
                X86_64CoreRegId::Gpr(13) => copy_out(dst, &saved.r13.to_le_bytes()),
                X86_64CoreRegId::Gpr(14) => copy_out(dst, &saved.r14.to_le_bytes()),
                X86_64CoreRegId::Gpr(15) => copy_out(dst, &saved.r15.to_le_bytes()),
                X86_64CoreRegId::Rip => copy_out(dst, &saved.rip.to_le_bytes()),
                X86_64CoreRegId::Eflags => {
                    let rflags: u32 = saved.rflags.try_into().unwrap();
                    copy_out(dst, &rflags.to_le_bytes())
                }
                X86_64CoreRegId::Segment(X86SegmentRegId::CS) => {
                    let cs: u32 = saved.cs.try_into().unwrap();
                    copy_out(dst, &cs.to_le_bytes())
                }
                X86_64CoreRegId::Segment(X86SegmentRegId::SS) => {
                    let ss: u32 = saved.ss.try_into().unwrap();
                    copy_out(dst, &ss.to_le_bytes())
                }
                X86_64CoreRegId::Segment(X86SegmentRegId::DS) => {
                    return Err(TargetError::NonFatal);
                }
                X86_64CoreRegId::Segment(X86SegmentRegId::ES) => {
                    return Err(TargetError::NonFatal);
                }
                X86_64CoreRegId::Segment(X86SegmentRegId::FS) => {
                    let fs: u32 = saved.fs.try_into().unwrap();
                    copy_out(dst, &fs.to_le_bytes())
                }
                X86_64CoreRegId::Segment(X86SegmentRegId::GS) => {
                    let gs: u32 = saved.gs.try_into().unwrap();
                    copy_out(dst, &gs.to_le_bytes())
                }
                //X86_64CoreRegId::St(u8) => {},
                //X86_64CoreRegId::Fpu(X87FpuInternalRegId) => {},
                //X86_64CoreRegId::Xmm(u8) => {},
                //X86_64CoreRegId::Mxcsr => {},
                missing => {
                    error!("Unimplemented register {:?}", missing);
                    return Err(TargetError::NonFatal);
                }
            }
        } else {
            Err(TargetError::NonFatal)
        }
    }

    fn write_register(
        &mut self,
        tid: (),
        reg_id: X86_64CoreRegId,
        val: &[u8],
    ) -> TargetResult<(), Self> {
        trace!("write_register {:?} {:?}", reg_id, val);
        let kcb = super::kcb::get_kcb();

        if let Some(saved) = &mut kcb.arch.save_area {
            match reg_id {
                X86_64CoreRegId::Gpr(00) => {
                    saved.rax =
                        u64::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?);
                }
                X86_64CoreRegId::Gpr(01) => {
                    saved.rbx =
                        u64::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?);
                }
                X86_64CoreRegId::Gpr(02) => {
                    saved.rcx =
                        u64::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?);
                }
                X86_64CoreRegId::Gpr(03) => {
                    saved.rdx =
                        u64::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?);
                }
                X86_64CoreRegId::Gpr(04) => {
                    saved.rsi =
                        u64::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?);
                }
                X86_64CoreRegId::Gpr(05) => {
                    saved.rdi =
                        u64::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?);
                }
                X86_64CoreRegId::Gpr(06) => {
                    saved.rbp =
                        u64::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?);
                }
                X86_64CoreRegId::Gpr(07) => {
                    saved.rsp =
                        u64::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?);
                }
                X86_64CoreRegId::Gpr(08) => {
                    saved.r8 =
                        u64::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?);
                }
                X86_64CoreRegId::Gpr(09) => {
                    saved.r9 =
                        u64::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?);
                }
                X86_64CoreRegId::Gpr(10) => {
                    saved.r10 =
                        u64::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?);
                }
                X86_64CoreRegId::Gpr(11) => {
                    saved.r11 =
                        u64::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?);
                }
                X86_64CoreRegId::Gpr(12) => {
                    saved.r12 =
                        u64::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?);
                }
                X86_64CoreRegId::Gpr(13) => {
                    saved.r13 =
                        u64::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?);
                }
                X86_64CoreRegId::Gpr(14) => {
                    saved.r14 =
                        u64::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?);
                }
                X86_64CoreRegId::Gpr(15) => {
                    saved.r15 =
                        u64::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?);
                }
                X86_64CoreRegId::Rip => {
                    assert_eq!(val.len(), 8, "gdbstub/issues/80");
                    saved.rip =
                        u64::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?);
                }
                X86_64CoreRegId::Eflags => {
                    saved.rflags =
                        u32::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?)
                            as u64;
                }
                X86_64CoreRegId::Segment(X86SegmentRegId::CS) => {
                    saved.cs =
                        u32::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?)
                            as u64;
                }

                X86_64CoreRegId::Segment(X86SegmentRegId::SS) => {
                    saved.ss =
                        u32::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?)
                            as u64;
                }
                X86_64CoreRegId::Segment(X86SegmentRegId::DS) => {
                    return Err(TargetError::NonFatal);
                }
                X86_64CoreRegId::Segment(X86SegmentRegId::ES) => {
                    return Err(TargetError::NonFatal);
                }
                X86_64CoreRegId::Segment(X86SegmentRegId::FS) => {
                    saved.fs =
                        u32::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?)
                            as u64;
                }
                X86_64CoreRegId::Segment(X86SegmentRegId::GS) => {
                    saved.gs =
                        u32::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?)
                            as u64;
                }
                //X86_64CoreRegId::St(u8) => {},
                //X86_64CoreRegId::Fpu(X87FpuInternalRegId) => {},
                //X86_64CoreRegId::Xmm(u8) => {},
                X86_64CoreRegId::Mxcsr => {
                    saved.fxsave.mxcsr =
                        u64::from_le_bytes(val.try_into().map_err(|e| TargetError::NonFatal)?)
                            as u32;
                }
                _ => {
                    error!("Unimplemented register");
                    return Err(TargetError::NonFatal);
                }
            };

            Ok(())
        } else {
            Err(TargetError::NonFatal)
        }
    }
}

impl SectionOffsets for KernelDebugger {
    /// Tells GDB where in memory the bootloader has put our kernel binary.
    fn get_section_offsets(&mut self) -> Result<Offsets<u64>, KError> {
        let kcb = super::kcb::get_kcb();
        let boot_args = kcb.arch.kernel_args();
        let kernel_reloc_offset = boot_args.kernel_elf_offset.as_u64();

        Ok(Offsets::Sections {
            text: kernel_reloc_offset,
            data: kernel_reloc_offset,
            bss: None,
        })
    }
}

fn watchkind_to_breakcondition(kind: WatchKind) -> debugregs::BreakCondition {
    match kind {
        // There is no read-only break condition in x86
        WatchKind::Read => debugregs::BreakCondition::DataReadsWrites,
        WatchKind::Write => debugregs::BreakCondition::DataWrites,
        WatchKind::ReadWrite => debugregs::BreakCondition::DataReadsWrites,
    }
}

impl HwWatchpoint for KernelDebugger {
    fn add_hw_watchpoint(
        &mut self,
        addr: u64,
        len: u64,
        kind: WatchKind,
    ) -> TargetResult<bool, Self> {
        trace!("add_hw_watchpoint {:#x} {} {:?}", addr, len, kind);
        let sa = super::kcb::get_kcb()
            .arch
            .save_area
            .as_mut()
            .expect("Need to have a save area");

        for (idx, (reg, entry)) in debugregs::BREAKPOINT_REGS
            .iter()
            .zip(self.hw_break_points.iter_mut())
            .enumerate()
        {
            let bs = match len {
                1 => debugregs::BreakSize::Bytes1,
                2 => debugregs::BreakSize::Bytes2,
                4 => debugregs::BreakSize::Bytes4,
                8 => debugregs::BreakSize::Bytes8,
                _ => {
                    warn!("Unsupported len ({}) provided by GDB, use 8 bytes.", len);
                    debugregs::BreakSize::Bytes8
                }
            };

            if entry.is_none() {
                *entry = Some(BreakState(
                    VAddr::from(addr),
                    BreakType::Watchpoint(kind),
                    BreakRequest::Hardware,
                ));
                let bc = watchkind_to_breakcondition(kind);

                // Safety: We're in CPL0.
                //
                // TODO: For more safety we should sanitize the address to make
                // sure we can't set it inside certain regions (on code that is
                // used by the gdb module etc.)
                unsafe {
                    reg.configure(addr.try_into().unwrap(), bc, bs);
                }
                sa.enabled_bps.set_bit(idx, true);
                info!("sa.enabled_bps {:#b}", sa.enabled_bps);

                return Ok(true);
            }
        }

        warn!("No more debug registers available for add_hw_watchpoint");
        Ok(false)
    }

    fn remove_hw_watchpoint(
        &mut self,
        addr: u64,
        _len: u64,
        kind: WatchKind,
    ) -> TargetResult<bool, Self> {
        trace!("remove_hw_watchpoint {:#x} {} {:?}", addr, _len, kind);
        let sa = super::kcb::get_kcb()
            .arch
            .save_area
            .as_mut()
            .expect("Need to have a save area");

        for (idx, (reg, entry)) in debugregs::BREAKPOINT_REGS
            .iter()
            .zip(self.hw_break_points.iter_mut())
            .enumerate()
        {
            if let Some(BreakState(entry_vaddr, BreakType::Watchpoint(kind), entry_req)) = entry {
                if entry_vaddr.as_u64() == addr && *entry_req == BreakRequest::Hardware {
                    unsafe { reg.disable_global() }
                    sa.enabled_bps.set_bit(idx, false);
                    info!("sa.enabled_bps {:#b}", sa.enabled_bps);

                    *entry = None;
                    return Ok(true);
                }
            }
        }

        // No break point matching the address was found
        warn!("Unable to remove hw watchpoint for addr {:#x}", addr);
        Ok(false)
    }
}

impl SwBreakpoint for KernelDebugger {
    fn add_sw_breakpoint(&mut self, addr: u64, kind: usize) -> TargetResult<bool, Self> {
        trace!("add sw breakpoint {:#x}", addr);
        self.add_breakpoint(BreakRequest::Software, addr, kind)
    }

    fn remove_sw_breakpoint(&mut self, addr: u64, kind: usize) -> TargetResult<bool, Self> {
        trace!("remove sw breakpoint {:#x}", addr);
        self.remove_breakpoint(BreakRequest::Software, addr, kind)
    }
}

impl HwBreakpoint for KernelDebugger {
    fn add_hw_breakpoint(&mut self, addr: u64, _kind: usize) -> TargetResult<bool, Self> {
        self.add_breakpoint(BreakRequest::Hardware, addr, _kind)
    }

    fn remove_hw_breakpoint(&mut self, addr: u64, _kind: usize) -> TargetResult<bool, Self> {
        self.remove_breakpoint(BreakRequest::Hardware, addr, _kind)
    }
}
