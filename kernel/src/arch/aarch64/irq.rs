// Copyright © 2021 VMware, Inc. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Functionality to configure and deal with interrupts.

use armv8::aarch64::registers::*;
use log::info;

use core::arch::asm;

pub enum Daifset {
    Debug,
    SError,
    Irq,
    Fiq,
}

pub enum Daifclr {
    Debug,
    SError,
    Irq,
    Fiq,
}

impl Daifset {
    pub fn write(val: Self) {
        match val {
            Daifset::Debug => unsafe {
                asm!("msr daifset, {n}", n = const 0b1000, options(nostack, nomem))
            },
            Daifset::SError => unsafe {
                asm!("msr daifset, {n}", n = const 0b0100, options(nostack, nomem))
            },
            Daifset::Irq => unsafe {
                asm!("msr daifset, {n}", n = const 0b0010, options(nostack, nomem))
            },
            Daifset::Fiq => unsafe {
                asm!("msr daifset, {n}", n = const 0b0001, options(nostack, nomem))
            },
        }
    }
}

impl Daifclr {
    pub fn write(val: Self) {
        match val {
            Daifclr::Debug => unsafe {
                asm!("msr daifclr, {n}", n = const 0b1000, options(nostack, nomem))
            },
            Daifclr::SError => unsafe {
                asm!("msr daifclr, {n}", n = const 0b0100, options(nostack, nomem))
            },
            Daifclr::Irq => unsafe {
                asm!("msr daifclr, {n}", n = const 0b0010, options(nostack, nomem))
            },
            Daifclr::Fiq => unsafe {
                asm!("msr daifclr, {n}", n = const 0b0001, options(nostack, nomem))
            },
        }
    }
}

pub(super) fn init_gic() {
    info!("GIC");
}

pub(super) fn debug_gic() {
    Daifset::write(Daifset::Irq);
    Daifset::write(Daifset::Fiq);
    Daifset::write(Daifset::Debug);
    Daifset::write(Daifset::SError);
    unsafe {
        asm!("msr daifset, {n}", n = const 0b1111, options(nostack, nomem));
    }

    info!("GIC");
    let reg = IccCtlrEl1::with_reg_val();
    log::info!(
        "IccCtlrEl1: hex {:#x} bin {:#b} dec {}",
        reg.get_raw(),
        reg.get_raw(),
        reg.get_raw()
    );
    let mut reg = IccIgrpen0El1::with_reg_val();
    reg.enable_insert(1);
    reg.write();
    log::info!(
        "IccIgrpen0El1: hex {:#x} bin {:#b} dec {}",
        reg.get_raw(),
        reg.get_raw(),
        reg.get_raw()
    );

    let mut reg = IccIgrpen1El1::with_reg_val();
    reg.enable_insert(1);
    reg.write();
    log::info!(
        "IccIgrpen1El1: hex {:#x} bin {:#b} dec {}",
        reg.get_raw(),
        reg.get_raw(),
        reg.get_raw()
    );

    let reg = IccSreEl1::with_reg_val();
    log::info!(
        "IccSreEl1: hex {:#x} bin {:#b} dec {}",
        reg.get_raw(),
        reg.get_raw(),
        reg.get_raw()
    );

    let reg = IccIar0El1::with_reg_val();
    log::info!(
        "IccIar0El1: hex {:#x} bin {:#b} dec {}",
        reg.get_raw(),
        reg.get_raw(),
        reg.get_raw()
    );

    let reg = IccIar1El1::with_reg_val();
    log::info!(
        "IccIar1El1: hex {:#x} bin {:#b} dec {}",
        reg.get_raw(),
        reg.get_raw(),
        reg.get_raw()
    );

    let mut reg = IccPmrEl1::with_reg_val();
    reg.priority_insert(0b0);
    reg.write();
    log::info!(
        "IccPmr1El1: hex {:#x} bin {:#b} dec {}",
        reg.get_raw(),
        reg.get_raw(),
        reg.get_raw()
    );

    let mut reg = IccRprEl1::with_reg_val();
    log::info!(
        "IccRpr1El1: hex {:#x} bin {:#b} dec {}",
        reg.get_raw(),
        reg.get_raw(),
        reg.get_raw()
    );

    let cbar = read_cbar();
    log::info!("CBAR: {:#x}", cbar);
    let gic = gic::distributor::Distributor::new(
        super::memory::paddr_to_kernel_vaddr(cbar.into()).as_usize(),
    );
    gic.init();
}

fn read_cbar() -> u64 {
    let r;
    unsafe { asm!("mrs {}, s3_1_c15_c3_0", out(reg) r, options(nomem, nostack, preserves_flags)) };
    r
}

pub(crate) fn enable() {
    unsafe {
        //x86::irq::enable();
    }
}

pub(crate) fn disable() {
    unsafe {
        //x86::irq::disable();
    }
}
