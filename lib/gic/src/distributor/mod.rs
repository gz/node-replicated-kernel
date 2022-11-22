//! Driver for a GICv3 distributor.

use core::{default::Default, fmt::Display};

use bit_field::BitField;
use driverkit::{DriverControl, DriverState};
use log::info;

pub mod registers;

use registers::*;

pub struct Distributor {
    state: DriverState,
    base: usize,
}

#[derive(Debug, Eq, PartialEq)]
pub struct Identification {
    pub implementer: u16,
    pub revision: u8,
    pub variant: u8,
    pub product_id: u8,
}

impl Display for Identification {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Implementer: 0x{:x}, Revision: 0x{:x}, Variant: 0x{:x}, Product ID: 0x{:x}",
            self.implementer, self.revision, self.variant, self.product_id
        )
    }
}

pub struct Type {
    /// The GIC implementation supports two Security states if true (single if false).
    pub security_extn: bool,
    /// Extended SPI range implemented if true.
    pub extended_espi: bool,
    /// Reports the number of PEs that can be used when affinity routing is not enabled, minus 1.
    /// If the implementation does not support ARE being zero, this field is 000.
    pub cpus: u8,
    /// Indicates the maximum SPI supported.
    pub lines: u16,
}

impl Display for Type {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Lines: {} CPUs: {} Extended SPI: {} Security Extension: {}",
            self.lines, self.cpus, self.extended_espi, self.security_extn
        )
    }
}

impl Distributor {
    pub fn new(base: usize) -> Self {
        Self {
            state: DriverState::Uninitialized,
            base,
        }
    }

    fn iidr(&self) -> u32 {
        self.read_register::<u32>(GICD_IIDR)
    }

    fn typer(&self) -> u32 {
        self.read_register::<u32>(GICD_TYPER)
    }

    pub fn capabilities(&self) -> Type {
        let typer = self.typer();
        Type {
            security_extn: typer.get_bit(10),
            extended_espi: typer.get_bit(8),
            cpus: typer.get_bits(5..=7) as u8,
            lines: 32 * (typer.get_bits(0..=4) + 1) as u16,
        }
    }

    pub fn identify(&self) -> Identification {
        let iidr = unsafe { self.read_register::<u32>(GICD_IIDR) };
        Identification {
            implementer: iidr.get_bits(0..12) as u16,
            revision: iidr.get_bits(12..16) as u8,
            variant: iidr.get_bits(16..20) as u8,
            product_id: iidr.get_bits(24..32) as u8,
        }
    }

    pub fn init(&self) {
        info!("Distributor GICv3 initializing {}", self.identify());
        let caps = self.capabilities();
        info!("Distributor capabilities: {}", caps);

        // Put all interrupts into Group 0? and disable them
        for idx in 0..32 {
            self.write_register::<u32>(GICD_IGROUPR + idx as usize * 4, 0);
            self.write_register::<u32>(GICD_ICENABLER.start + idx as usize * 4, u32::MAX);
        }
    }

    fn read_register<T>(&self, offset: usize) -> T {
        let addr = self.base + offset;
        let val: T = unsafe { core::ptr::read_volatile(addr as *const T) };
        val
    }

    fn write_register<T>(&mut self, offset: usize, val: T) {
        let addr = self.base + offset;
        unsafe { core::ptr::write_volatile(addr as *mut T, val) };
    }
}

impl Default for Distributor {
    fn default() -> Self {
        Self {
            state: DriverState::Uninitialized,
            base: 0,
        }
    }
}

impl DriverControl for Distributor {
    /// Attach to the device
    fn attach(&mut self) {
        self.set_state(DriverState::Attached(0));
    }

    /// Detach from the device
    fn detach(&mut self) {
        self.set_state(DriverState::Detached);
    }

    /// Destroy the device.
    fn destroy(mut self) {
        self.set_state(DriverState::Destroyed);
    }

    /// Query driver state
    fn state(&self) -> DriverState {
        self.state
    }

    /// Set the state of the driver
    fn set_state(&mut self, st: DriverState) {
        self.state = st;
    }
}
