// Copyright © 2021 VMware, Inc. All Rights Reserved.
// Copyright © 2022 The University of British Columbia. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A UEFI based bootloader for an x86-64 kernel.
//!
//! This code roughly does the following: looks for a kernel binary
//! in the EFI partition, loads it, then continues to construct an
//! address space for it, and finally it switches to the new address
//! space and executes the kernel entry function. In addition we
//! gather a bit of information about memory regions and pass this
//! information on to the kernel.
//!
//! When the CPU driver on the boot core begins executing, the following
//! statements hold:
//!
//!  * In CR4, we enabled the following features
//!    * PROTECTION_KEY
//!    * SMAP
//!    * SMEP
//!    * OS_XSAVE
//!    * FSGSBASE
//!    * UNMASKED_SSE
//!    * ENABLE_SSE
//!    * ENABLE_GLOBAL_PAGES
//!    * ENABLE_PAE
//!    * ENABLE_PSE
//!    * DEBUGGING_EXTENSIONS
//!    * ENABLE_MACHINE_CHECK
//!  * In IA32_EFER MSR, we enabled the following features
//!    * NSX (No execute bit): The constructed kernel page-tables already make use of the NSX bits
//!  * The kernel address space we switch to is set-up as follows:
//!    * All UEFI reported memory regions are 1:1 mapped phys <-> virt.
//!    * All UEFI reported memory regions are 1:1 mapped to the 'kernel physical space' (which is above KERNEL_BASE).
//!    * The kernel ELF binary is loaded somewhere in physical memory and relocated
//!      for running in the kernel-space above KERNEL_BASE.
//!  * A pointer to the KernelArgs struct is given as a first argument:
//!    * The memory allocated for it (and everything within) is pointing to kernel space
//!
//!  Not yet done:
//!    * the xAPIC region is remapped to XXX

#![no_std]
#![no_main]

#[macro_use]
extern crate log;
#[macro_use]
extern crate alloc;

extern crate elfloader;

use core::mem::transmute;
use core::{mem, slice};

use uefi::prelude::*;
use uefi::proto::console::gop::GraphicsOutput;
use uefi::table::boot::{AllocateType, MemoryDescriptor, MemoryType};
use uefi::table::cfg::{ACPI2_GUID, ACPI_GUID};

use crate::alloc::vec::Vec;

// The x86-64 platform specific code.
#[cfg(all(target_arch = "x86_64"))]
#[path = "arch/x86_64/mod.rs"]
pub mod arch;

// The aarch64 platform specific code.
#[cfg(all(target_arch = "aarch64"))]
#[path = "arch/aarch64/mod.rs"]
pub mod arch;

use arch::VSpace;

mod kernel;
mod memory;
mod modules;
mod vspace;

use kernel::*;
use modules::*;
use vspace::*;

use bootloader_shared::*;

#[macro_export]
macro_rules! round_up {
    ($num:expr, $s:expr) => {
        (($num + $s - 1) / $s) * $s
    };
}

/// Make sure our UEFI version is not outdated.
fn check_revision(rev: uefi::table::Revision) {
    let (major, minor) = (rev.major(), rev.minor());
    assert!(major >= 2 && minor >= 30, "Require UEFI version >= 2.30");
}

/// Allocates `pages` * `BASE_PAGE_SIZE` bytes of physical memory
/// and return the address.
pub fn allocate_pages(st: &SystemTable<Boot>, pages: usize, typ: MemoryType) -> arch::PAddr {
    let num = st
        .boot_services()
        .allocate_pages(AllocateType::AnyPages, typ, pages)
        .expect(format!("Allocation of {} failed for type {:?}", pages, typ).as_str());

    // TODO: The UEFI Specification does not say if the pages we get are zeroed or not
    // (UEFI Specification 2.8, EFI_BOOT_SERVICES.AllocatePages())
    unsafe {
        st.boot_services()
            .set_mem(num as *mut u8, pages * arch::BASE_PAGE_SIZE, 0u8)
    };

    arch::PAddr::from(num)
}

/// Find out how many pages we require to load the memory map
/// into it.
///
/// Plan for some 32 more descriptors than originally estimated,
/// due to UEFI API crazyness. Also round to page-size.
fn estimate_memory_map_size(st: &SystemTable<Boot>) -> (usize, usize) {
    let mm_size_estimate = st.boot_services().memory_map_size();
    // Plan for some 32 more descriptors than originally estimated,
    // due to UEFI API crazyness, round to page-size
    let sz = round_up!(
        mm_size_estimate.map_size + 32 * mm_size_estimate.entry_size,
        arch::BASE_PAGE_SIZE
    );
    assert_eq!(sz % arch::BASE_PAGE_SIZE, 0, "Not multiple of page-size.");

    (sz, sz / mem::size_of::<MemoryDescriptor>())
}

/// Initialize the screen to the highest possible resolution.
fn _setup_screen(st: &SystemTable<Boot>) {
    if let Ok(gop) = st.boot_services().locate_protocol::<GraphicsOutput>() {
        let gop = unsafe { &mut *gop.get() };
        let _mode = gop
            .modes()
            .max_by(|ref x, ref y| x.info().resolution().cmp(&y.info().resolution()))
            .unwrap();
    } else {
        warn!("UEFI Graphics Output Protocol is not supported.");
    }
}

/// Intialize the serial console.
fn _serial_init(st: &SystemTable<Boot>) {
    use uefi::proto::console::serial::{ControlBits, Serial};
    if let Ok(serial) = st.boot_services().locate_protocol::<Serial>() {
        let serial = unsafe { &mut *serial.get() };

        let _old_ctrl_bits = serial
            .get_control_bits()
            .expect("Failed to get device control bits");

        let mut ctrl_bits = ControlBits::empty();
        ctrl_bits |= ControlBits::HARDWARE_FLOW_CONTROL_ENABLE;
        ctrl_bits |= ControlBits::SOFTWARE_LOOPBACK_ENABLE;

        serial
            .set_control_bits(ctrl_bits)
            .expect("Failed to set device control bits");

        const OUTPUT: &[u8] = b"Serial output check";
        const MSG_LEN: usize = OUTPUT.len();
        serial
            .write(OUTPUT)
            .expect("Failed to write to serial port");
    } else {
        warn!("No serial device found.");
    }
}

/// Start function of the bootloader.
/// The symbol name is defined through `/Entry:uefi_start` in `x86_64-uefi.json`.
#[no_mangle]
pub extern "C" fn uefi_start(handle: uefi::Handle, mut st: SystemTable<Boot>) -> Status {
    uefi_services::init(&mut st).expect("Can't initialize UEFI");
    log::set_max_level(log::LevelFilter::Info);
    log::set_max_level(log::LevelFilter::Debug);
    //setup_screen(&st);
    //serial_init(&st);

    debug!(
        "UEFI {}.{}",
        st.uefi_revision().major(),
        st.uefi_revision().minor()
    );
    info!("UEFI Bootloader starting...");
    check_revision(st.uefi_revision());

    let modules = load_modules_on_all_sfs(&st, "\\");

    let (kernel_blob, cmdline_blob) = {
        let mut kernel_blob = None;
        let mut cmdline_blob = None;
        for (name, m) in modules.iter() {
            if name == "kernel" {
                // This needs to be in physical space, because we relocate it in the bootloader
                kernel_blob = unsafe { Some(m.as_pslice()) };
            }
            if name == "cmdline.in" {
                // This needs to be in kernel-space because we ultimately access it in the kernel
                cmdline_blob = unsafe { Some(m.as_pslice()) };
                trace!("cmdline.in blob is at {:#x}", m.binary_paddr);
            }
        }

        (
            kernel_blob.expect("Didn't find kernel binary."),
            cmdline_blob.expect("Didn't find cmdline.in"),
        )
    };

    // Next create an address space for our kernel
    let mut kernel = Kernel {
        offset: arch::VAddr::from(arch::KERNEL_OFFSET),
        mapping: Vec::new(),
        vspace: arch::VSpace::new(),
        tls: None,
    };

    // Parse the ELF file and load it into the new address space
    let binary = elfloader::ElfBinary::new(kernel_blob).unwrap();
    info!("Loading kernel binary...");
    binary.load(&mut kernel).expect("Can't load the kernel");

    // On big machines with the init stack tends to put big structures
    // on the stack so we reserve a fair amount of space:
    let stack_pages: usize = 768;
    let stack_region: arch::PAddr = allocate_pages(&st, stack_pages, MemoryType(KERNEL_STACK));
    let stack_protector: arch::PAddr = stack_region;
    let stack_base: arch::PAddr = stack_region + arch::BASE_PAGE_SIZE;

    let stack_size: usize = (stack_pages - 1) * arch::BASE_PAGE_SIZE;
    let stack_top: arch::PAddr = stack_base + stack_size as u64;
    assert_eq!(stack_protector + arch::BASE_PAGE_SIZE, stack_base);

    kernel.vspace.map_identity_with_offset(
        arch::VAddr::from(arch::KERNEL_OFFSET as u64),
        stack_protector,
        stack_protector + arch::BASE_PAGE_SIZE,
        MapAction::ReadUser, // TODO: should be MapAction::None
    );
    kernel.vspace.map_identity_with_offset(
        arch::VAddr::from(arch::KERNEL_OFFSET as u64),
        stack_base,
        stack_top,
        MapAction::ReadWriteKernel,
    );
    debug!(
        "Init stack memory: {:#x} -- {:#x} (protector at {:#x} -- {:#x})",
        stack_base.as_u64(),
        stack_top.as_u64(),
        stack_protector,
        stack_protector + arch::BASE_PAGE_SIZE,
    );
    assert!(mem::size_of::<KernelArgs>() < arch::BASE_PAGE_SIZE);
    let kernel_args_paddr = allocate_pages(&st, 1, MemoryType(KERNEL_ARGS));

    // Make sure we still have access to the UEFI mappings:
    // Get the current memory map and 1:1 map all physical memory
    // dump_translation_root_register();
    arch::map_physical_memory(&st, &mut kernel);
    trace!("Replicated UEFI memory map");
    arch::cpu::assert_required_cpu_features();
    arch::cpu::setup_cpu_features();

    unsafe {
        // Preparing to jump to the kernel
        // * Switch to the kernel address space
        // * Exit boot services
        // * Switch stack and do a jump to kernel ELF entry point
        // Get an estimate of the memory map size:
        let (mm_size, no_descs) = estimate_memory_map_size(&st);
        assert_eq!(mm_size % arch::BASE_PAGE_SIZE, 0);
        let mm_paddr = allocate_pages(
            &st,
            mm_size / arch::BASE_PAGE_SIZE,
            MemoryType(UEFI_MEMORY_MAP),
        );
        let mm_slice =
            slice::from_raw_parts_mut(paddr_to_uefi_vaddr(mm_paddr).as_mut_ptr::<u8>(), mm_size);
        trace!("Memory map allocated.");

        // Construct a KernelArgs struct that gets passed to the kernel
        // This could theoretically be pushed on the stack too
        // but for now we just allocate a separate page (and don't care about
        // wasted memory)
        let mut kernel_args =
            transmute::<arch::VAddr, &mut KernelArgs>(paddr_to_uefi_vaddr(kernel_args_paddr));
        trace!("Kernel args allocated at {:#x}.", kernel_args_paddr);
        kernel_args.mm_iter = Vec::with_capacity(no_descs);

        // Initialize the KernelArgs
        kernel_args.command_line = core::str::from_utf8_unchecked(cmdline_blob);
        kernel_args.mm = (mm_paddr + arch::KERNEL_OFFSET, mm_size);
        kernel_args.pml4 = arch::PAddr::from(kernel.vspace.roottable());
        kernel_args.stack = (stack_base + arch::KERNEL_OFFSET, stack_size);
        kernel_args.kernel_elf_offset = kernel.offset;
        kernel_args.tls_info = kernel.tls;
        kernel_args.modules = arrayvec::ArrayVec::new();
        // Add modules to kernel args, ensure 'kernel' is first:
        for (name, module) in modules.iter() {
            if name == "kernel" {
                kernel_args.modules.push(module.clone());
            }
        }
        for (name, module) in modules {
            if name != "kernel" {
                kernel_args.modules.push(module);
            }
        }
        for entry in st.config_table() {
            if entry.guid == ACPI2_GUID {
                kernel_args.acpi2_rsdp = arch::PAddr::from(entry.address as u64);
            } else if entry.guid == ACPI_GUID {
                kernel_args.acpi1_rsdp = arch::PAddr::from(entry.address as u64);
            }
        }

        if let Ok(gop) = st.boot_services().locate_protocol::<GraphicsOutput>() {
            let gop = &mut *gop.get();

            let mut frame_buffer = gop.frame_buffer();
            let frame_buf_ptr = frame_buffer.as_mut_ptr();
            let size = frame_buffer.size();
            let _frame_buf_paddr = arch::PAddr::from(frame_buf_ptr as u64);

            kernel_args.frame_buffer = Some(core::slice::from_raw_parts_mut(
                frame_buf_ptr.add(arch::KERNEL_OFFSET),
                size,
            ));
            kernel_args.mode_info = Some(gop.current_mode_info());
        } else {
            kernel_args.frame_buffer = None;
            kernel_args.mode_info = None;
        }

        info!(
            "Kernel will start to execute from: {:p}",
            kernel.offset + binary.entry_point()
        );

        info!(
            "Kernel stack at: 0x{:x}",
            arch::KERNEL_OFFSET as u64 + stack_top.as_u64() - (arch::BASE_PAGE_SIZE as u64)
        );

        info!(
            "Kernel arguments at: 0x{:x}",
            paddr_to_kernel_vaddr(kernel_args_paddr).as_u64()
        );

        info!("Page tables at: 0x{:x}", kernel.vspace.roottable());

        // kernel.vspace.dump_translation_table();

        info!("Exiting boot services. About to jump...");
        let (_st, mmiter) = st
            .exit_boot_services(handle, mm_slice)
            .expect("Can't exit the boot service");
        // FYI: Print no longer works here... so let's hope we make
        // it to the kernel serial init

        kernel_args.mm_iter.extend(mmiter);

        // It's unclear from the spec if `exit_boot_services` already disables interrupts
        // so we we make sure they are disabled (otherwise we triple fault since
        // we don't have an IDT setup in the beginning)
        arch::cpu::disable_interrupts();

        // Switch to the kernel address space
        arch::cpu::set_translation_table(kernel.vspace.roottable());

        // Finally switch to the kernel stack and entry function
        arch::cpu::jump_to_kernel(
            arch::KERNEL_OFFSET as u64 + stack_top.as_u64() - (arch::BASE_PAGE_SIZE as u64),
            kernel.offset.as_u64() + binary.entry_point(),
            paddr_to_kernel_vaddr(kernel_args_paddr).as_u64(),
        );

        unreachable!("UEFI Bootloader: We are not supposed to return here from the kernel?");
    }
}
