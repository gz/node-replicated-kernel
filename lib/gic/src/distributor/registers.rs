//! Distributor registers and offsets (relative to PERIPHBASE).

use core::ops::Range;

/// Distributor Control Register (RW).
pub const GICD_CTLR: usize = 0x0000;

/// Interrupt Controller Type Register (RO).
pub const GICD_TYPER: usize = 0x0004;

/// Distributor Implementer Identification Register (RO).
pub const GICD_IIDR: usize = 0x0008;

/// Interrupt controller Type Register 2 (RO).
pub const GICD_TYPER2: usize = 0x000C;

/// Error Reporting Status Register, optional (RW).
pub const GICD_STATUSR: usize = 0x0010;

/// Set SPI Register (WO).
pub const GICD_SETSPI_NSR: usize = 0x0040;

/// Clear SPI Register.
pub const GICD_CLRSPI_NSR: usize = 0x0048;

/// Set SPI, Secure Register (WO).
pub const GICD_SETSPI_SR: usize = 0x0050;

/// Clear SPI, Secure Register (WO).
pub const GICD_CLRSPI_SR: usize = 0x0058;

/// Interrupt Group Registers (RW).
pub const GICD_IGROUPR: usize = 0x0080;

/// Interrupt Set-Enable Registers (RW).
pub const GICD_ISENABLER: Range<usize> = Range {
    start: 0x0100,
    end: 0x017C,
};

/// Interrupt Clear-Enable Registers (RW).
pub const GICD_ICENABLER: Range<usize> = Range {
    start: 0x0180,
    end: 0x01FC,
};

/// Interrupt Set-Pending Registers (RW).
pub const GICD_ISPENDR: Range<usize> = Range {
    start: 0x0200,
    end: 0x027C,
};

/// Interrupt Clear-Pending Registers (RW).
pub const GICD_ICPENDR: Range<usize> = Range {
    start: 0x0280,
    end: 0x02FC,
};

/// Interrupt Set-Active Registers (RW)
pub const GICD_ISACTIVER: Range<usize> = Range {
    start: 0x0300,
    end: 0x037C,
};

/// Interrupt Clear-Active Registers (RW)
pub const GICD_ICACTIVER: Range<usize> = Range {
    start: 0x0380,
    end: 0x03FC,
};

/// Interrupt Priority Registers (RW).
pub const GICD_IPRIORITYR: Range<usize> = Range {
    start: 0x0400,
    end: 0x07F8,
};

/// Interrupt Processor Targets Registers (RO).
pub const GICD_ITARGETSR: Range<usize> = Range {
    start: 0x0800,
    end: 0x081C,
};

/// Interrupt Configuration Registers.
pub const GICD_ICFGR: Range<usize> = Range {
    start: 0x0C00,
    end: 0x0CFC,
};

/// Interrupt Group Modifier Registers.
pub const GICD_IGRPMODR: Range<usize> = Range {
    start: 0x0D00,
    end: 0x0D7C,
};

/// Non-secure Access Control Registers (RW).
pub const GICD_NSACR_N: Range<usize> = Range {
    start: 0x0E00,
    end: 0x0EFC,
};

/// Software Generated Interrupt Register (WO).
pub const GICD_SGIR: usize = 0x0F00;

/// SGI Clear-Pending Registers (RW).
pub const GICD_CPENDSGIR: Range<usize> = Range {
    start: 0x0F10,
    end: 0x0F1C,
};

/// SGI Set-Pending Registers (RW).
pub const GICD_SPENDSGIR: Range<usize> = Range {
    start: 0x0F20,
    end: 0x0F2C,
};

/// Interrupt Group Registers for extended SPI range (RW).
pub const GICD_IGROUPR_E: Range<usize> = Range {
    start: 0x1000,
    end: 0x107C,
};

/// Interrupt Set-Enable for extended SPI range (RW).
pub const GICD_ISENABLER_E: Range<usize> = Range {
    start: 0x1200,
    end: 0x127C,
};

/// Interrupt Clear-Enable for extended SPI range (RW).
pub const GICD_ICENABLER_E: Range<usize> = Range {
    start: 0x1400,
    end: 0x147C,
};

/// Interrupt Set-Pend for extended SPI range (RW).
pub const GICD_ISPENDR_E: Range<usize> = Range {
    start: 0x1600,
    end: 0x167C,
};

/// Interrupt Clear-Pend for extended SPI range (RW).
pub const GICD_ICPENDR_E: Range<usize> = Range {
    start: 0x1800,
    end: 0x187C,
};

/// Interrupt Set-Active for extended SPI range (RW).
pub const GICD_ISACTIVER_E: Range<usize> = Range {
    start: 0x1A00,
    end: 0x1A7C,
};

/// Interrupt Clear-Active for extended SPI range (RW).
pub const GICD_ICACTIVER_E: Range<usize> = Range {
    start: 0x1C00,
    end: 0x1C7C,
};

/// Interrupt Priority for extended SPI range (RW).
pub const GICD_IPRIORITYR_E: Range<usize> = Range {
    start: 0x2000,
    end: 0x23FC,
};

/// Extended SPI Configuration Register (RW).
pub const GICD_ICFGR_E: Range<usize> = Range {
    start: 0x3000,
    end: 0x30FC,
};

/// Interrupt Group Modifier for extended SPI range (RW).
pub const GICD_IGRPMODR_E: Range<usize> = Range {
    start: 0x3400,
    end: 0x347C,
};

/// Non-secure Access Control Registers for extended SPI range (RW).
pub const GICD_NSACR_E: Range<usize> = Range {
    start: 0x3600,
    end: 0x367C,
};

/// Interrupt Routing Registers (RW).
pub const GICD_IROUTER: Range<usize> = Range {
    start: 0x6100,
    end: 0x7FD8,
};

/// IMPLEMENTATION DEFINED Interrupt Routing Registers for extended SPI range (RW).
pub const GICD_IROUTER_E: Range<usize> = Range {
    start: 0x8000,
    end: 0x9FFC,
};

/// Distributor Message Based Interrupt Frame (RO).
pub const GICM_TYPER: usize = 0x0008;

/// Set SPI Register, alias of GICD_SETSPI_NSR (WO).
pub const GICM_SETSPI_NSR: usize = 0x0040;

/// Clear SPI Register, alias of GICD_CLRSPI_NSR (WO).
pub const GICM_CLRSPI_NSR: usize = 0x0048;

/// Set SPI, Secure Register, alias of, GICD_SETSPI_SR (WO).
pub const GICM_SETSPI_SR: usize = 0x0050;

/// Clear SPI, Secure Register, alias of (WO).
pub const GICM_CLRSPI_SR: usize = 0x0058;

/// Distributor Message Based Interrupt Frame Implementer Identification (RO).
pub const GICM_IIDR: usize = 0x0FCC;
