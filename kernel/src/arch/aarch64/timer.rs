// Copyright © 2022 The University of British Columbia. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0 OR MIT

/// Default when to raise the next timer irq (in rdtsc ticks)
pub(crate) const DEFAULT_TIMER_DEADLINE: u64 = 2_000_000_000;

use armv8::aarch64::registers::*;

pub(crate) fn init_timer() {}

pub(crate) fn now() -> u64 {
    CntpctEl0::with_reg_val().get_raw()
}

/// Register a periodic timer to advance replica
///
/// TODO(api): Ideally this should come from Instant::now() +
/// Duration::from_millis(10) and for that we need a way to reliably
/// convert between TSC and Instant
pub(crate) fn set(deadline: u64) {
    let mut reg = CntpCvalEl0::default();
    reg.comparevalue_insert(deadline);
    reg.write();

    // Enable timer and interrupt
    let mut reg = CntpCtlEl0::with_reg_val();
    reg.enable_insert(1);
    reg.imask_insert(1);
    reg.write();
}

pub(crate) fn debug() {
    // The CNTFRQ_EL0 register must be programmed to the clock frequency of the
    // system counter
    //
    // Typically, this is done only during the system boot process, by using the
    // System register interface to write the system counter frequency to the
    // CNTFRQ_EL0 register.
    let cntfrq = CntfrqEl0::with_reg_val();
    log::info!(
        "cntfrq: hex {:#x} bin {:#b} dec {}",
        cntfrq.get_raw(),
        cntfrq.get_raw(),
        cntfrq.get_raw()
    );

    let reg = CntvTvalEl0::with_reg_val();
    log::info!(
        "CntvTvalEl0: hex {:#x} bin {:#b} dec {}",
        reg.get_raw(),
        reg.get_raw(),
        reg.get_raw()
    );

    // The PE includes a physical counter that contains the count value of the
    // system counter. The CNTPCT_EL0 register holds the current physical
    // counter value
    //
    // Reads of CNTPCT_EL0 can occur speculatively and out of order relative to
    // other instructions executed on the same PE.
    let reg = CntpctEl0::with_reg_val();
    log::info!(
        "CntpctEl0: hex {:#x} bin {:#b} dec {}",
        reg.get_raw(),
        reg.get_raw(),
        reg.get_raw()
    );

    // Each timer:
    // • Is based around a 64-bit CompareValue that provides a 64-bit unsigned upcounter.
    let reg = CntpCvalEl0::with_reg_val();
    log::info!(
        "CntpCvalEl0: hex {:#x} bin {:#b} dec {}",
        reg.get_raw(),
        reg.get_raw(),
        reg.get_raw()
    );

    // • Provides an alternative view of the CompareValue, called the TimerValue,
    // that appears to operate as a 32-bit downcounter
    let reg = CntpTvalEl0::with_reg_val();
    log::info!(
        "CntpTvalEl0: hex {:#x} bin {:#b} dec {}",
        reg.get_raw(),
        reg.get_raw(),
        reg.get_raw()
    );

    // Has, in addition, a 32-bit Control register.
    let reg = CntpCtlEl0::with_reg_val();
    log::info!(
        "CntpCtlEl0: hex {:#x} bin {:#b} dec {}",
        reg.get_raw(),
        reg.get_raw(),
        reg.get_raw()
    );

    // The status of the timer. This bit indicates whether the timer condition is met
    log::info!("CntpCtlEl0: istatus {}", CntpCtlEl0::istatus_read());
    // Timer interrupt mask bit.
    log::info!("CntpCtlEl0: imask {}", CntpCtlEl0::imask_read());
    // Enables the timer.
    log::info!("CntpCtlEl0: enable {}", CntpCtlEl0::enable_read());
}
