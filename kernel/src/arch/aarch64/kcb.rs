// Copyright © 2022 The University of British Columbia. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::boxed::Box;
use core::pin::Pin;
use core::ptr;

use cortex_a::{asm::barrier, registers::*};
use tock_registers::interfaces::{Readable, Writeable};

use crate::memory::per_core::PerCoreMemory;
use crate::memory::BASE_PAGE_SIZE;
use crate::stack::{OwnedStack, Stack};

/// Try to retrieve the per-core memory allocator by reading the gs register.
///
/// This may return None if the memory allocators is not yet set (i.e., during
/// initialization).
///
/// # Safety
/// - This gets a handle to PerCoreMemory (ideally, we should ensure that there
///   is no outstanding mut alias to it e.g., during initialization see comments
///   in mod.rs)
pub(crate) fn try_per_core_mem() -> Option<&'static PerCoreMemory> {
    unsafe {
        let kcb = TPIDR_EL1.get();
        if kcb != 0 {
            let pcm = (kcb + memoffset::offset_of!(AArch64Kcb, mem) as u64) as *const PerCoreMemory;
            assert!(pcm != ptr::null_mut());
            let static_pcm = &*pcm as &'static PerCoreMemory;
            // log::info!("Found per-core memory at {:p}", static_pcm);
            return Some(static_pcm);
        }
    }
    None
}

/// Reference to per-core memory state.
///
/// Must only call this after initialization has completed. So basically ok to
/// call in everywhere in the kernel except care must be taken in `arch`,
/// `crate::memory` and some places in `arch::irq`.
///
/// # Panics
/// - If the per-core memory is not yet initialized (only in debug mode).
pub(crate) fn per_core_mem() -> &'static PerCoreMemory {
    panic!("not yet implemented");
}

/// Retrieve the AArch64Kcb by reading the gs register.
///
///
/// # Panic
/// This will fail in case the KCB is not yet set (i.e., early on during
/// initialization).
pub(crate) fn get_kcb<'a>() -> &'a mut AArch64Kcb {
    unsafe {
        // Safety:
        // - TODO(safety+soundness): not safe, should return a non-mut reference
        //   with mutable stuff (it's just save_area that's left) wrapped in
        //   RefCell or similar (treat the same as a thread-local)
        let kcb_raw = TPIDR_EL1.get();
        let kcb = kcb_raw as *mut AArch64Kcb;
        assert!(kcb != ptr::null_mut(), "KCB not found in gs register.");
        let kptr = ptr::NonNull::new_unchecked(kcb);
        &mut *kptr.as_ptr()
    }
}

/// Architecture specific core control block.
///
/// Contains the arch-specific hardware state of a given aarch64 core.
/// `repr(C)` because assembly code references entries of this struct.
#[repr(C)]
pub(crate) struct AArch64Kcb {
    /// Pointer to the syscall stack (this is referenced in assembly) and should
    /// therefore always remain at offset 0 of the Kcb struct!
    ///
    /// The memory it points to shouldn't be accessed/modified at any point in
    /// the code (through this pointer).
    pub(super) kernel_stack_top: *mut u8,

    /// Pointer to the save area of the core, this is referenced on trap/syscall
    /// entries to save the CPU state into it and therefore has to remain at
    /// offset 0x8 in this struct.
    ///
    /// State from the save_area may be copied into the `current_executor` save
    /// area to handle upcalls (in the general state it is stored/resumed from
    /// here).
    pub(super) save_area: Option<Pin<Box<kpi::arch::SaveArea>>>,

    /// The state of the memory allocator on this core.
    pub(crate) mem: &'static PerCoreMemory,

    /// A handle to the kernel stack memory location.
    ///
    /// This member should probably not be touched from normal code.
    kernel_stack: Option<OwnedStack>,
}

impl AArch64Kcb {
    pub(crate) fn new(mem: &'static PerCoreMemory) -> Self {
        Self {
            kernel_stack_top: ptr::null_mut(),
            save_area: None,
            mem,
            kernel_stack: None,
        }
    }

    pub(super) fn install(&mut self) {
        self.initialize_tpidr();
        log::info!("allocating stack...");
        let stack = OwnedStack::new(128 * BASE_PAGE_SIZE);
        log::info!("setting stack");
        self.set_kernel_stack(stack);
        log::info!("setting save area...");
        self.set_save_area(Box::pin(kpi::arch::SaveArea::empty()));
    }

    fn initialize_tpidr(&mut self) {
        let kcb: ptr::NonNull<AArch64Kcb> = ptr::NonNull::from(self);
        log::info!("setting kcb to {:p}", kcb);
        TPIDR_EL1.set(kcb.as_ptr() as u64)
    }

    fn set_kernel_stack(&mut self, stack: OwnedStack) {
        self.kernel_stack_top = stack.base();
        log::trace!("Kernel stack top set to: {:p}", self.kernel_stack_top);
        self.kernel_stack = Some(stack);
    }

    /// Install a CPU register save-area.
    ///
    /// Register are store here in case we get an interrupt/sytem call
    fn set_save_area(&mut self, save_area: Pin<Box<kpi::arch::SaveArea>>) {
        self.save_area = Some(save_area);
    }

    /// Get a pointer to the cores save-area.
    pub(crate) fn get_save_area_ptr(&self) -> *const kpi::arch::SaveArea {
        // TODO(unsafe): this probably doesn't need an unsafe, but I couldn't figure
        // out how to get that pointer out of the Option<Pin<Box>>>
        unsafe {
            core::mem::transmute::<_, *const kpi::arch::SaveArea>(
                &*(*self.save_area.as_ref().unwrap()),
            )
        }
    }
}
