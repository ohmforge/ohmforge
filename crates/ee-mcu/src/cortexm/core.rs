//! The ARM Cortex-M0+ core: the register file, a flat memory model, an
//! NVIC-lite exception mechanism, and the fetch/decode/execute step loop.
//!
//! # Register file
//!
//! Sixteen 32-bit registers `r0..r15`, where `r13` is the stack pointer, `r14`
//! the link register, and `r15` the program counter. A read of `r15` yields the
//! Thumb **`PC` value** — the current instruction's address + 4 — so PC-relative
//! loads and branches compute the right target; a write to `r15` branches
//! (clearing bit 0, since the core is always in Thumb state). The APSR
//! condition flags `N`/`Z`/`C`/`V` live in [`Apsr`].
//!
//! # Memory model
//!
//! A single flat, little-endian, byte-addressed RAM mapped from address 0, plus
//! the RP2040 **SIO** GPIO block ([`Sio`]) decoded in the single-cycle I/O space.
//! Firmware (and the reset vector table) load at address 0. Accesses are assumed
//! aligned, matching ARMv6-M's requirements for the modelled instructions.
//!
//! # Exceptions (NVIC-lite)
//!
//! A latched controller mirroring the AVR core's: [`CortexM::request_interrupt`]
//! marks an external IRQ pending, and a `SVC` raises the SVCall exception.
//! Before an instruction, if nothing masks it, the core takes the pending
//! exception — **stacking** the eight-word exception frame
//! (`r0`–`r3`, `r12`, `LR`, return address, `xPSR`) on the stack, loading `PC`
//! from the vector table, and setting `LR` to an `EXC_RETURN` magic value.
//! Branching (`BX`) to an `EXC_RETURN` value unstacks the frame and resumes.

use crate::{FirmwareError, McuCore, PinIo};

use super::decode::decode;
use super::isa::{AddrMode, Instr, Mem, Size, LR, PC, SP};
use super::periph::{Sio, PIN_COUNT};

/// Size of the flat RAM, in bytes (256 KiB — ample for firmware + stack).
const MEM_SIZE: usize = 256 * 1024;
/// The stack pointer's reset default when no vector table provides one: the top
/// of RAM, eight-byte aligned.
const DEFAULT_SP: u32 = MEM_SIZE as u32;
/// Exception number of `SVCall`.
const EXC_SVCALL: u32 = 11;
/// The first external interrupt's exception number (`IRQ0` ⇒ exception 16).
const EXC_EXTERNAL_BASE: u32 = 16;
/// `EXC_RETURN` value for "return to Thread mode, main stack" — the only mode
/// this core uses. A `BX` to any value with this top byte is an exception
/// return.
const EXC_RETURN_THREAD_MSP: u32 = 0xFFFF_FFF9;

/// The APSR condition flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Apsr {
    /// Negative (bit 31 of the result).
    pub n: bool,
    /// Zero (result was zero).
    pub z: bool,
    /// Carry / unsigned overflow (or the shifter carry-out).
    pub c: bool,
    /// Signed overflow.
    pub v: bool,
}

impl Apsr {
    /// Pack the flags into the top nibble of an `xPSR` value (`N:Z:C:V` at bits
    /// 31:28), with the Thumb (`T`) bit set — the core is always in Thumb state.
    fn to_xpsr(self) -> u32 {
        (self.n as u32) << 31
            | (self.z as u32) << 30
            | (self.c as u32) << 29
            | (self.v as u32) << 28
            | 1 << 24 // T bit
    }

    /// Restore the condition flags from a stacked `xPSR`.
    fn load_xpsr(&mut self, v: u32) {
        self.n = v & (1 << 31) != 0;
        self.z = v & (1 << 30) != 0;
        self.c = v & (1 << 29) != 0;
        self.v = v & (1 << 28) != 0;
    }
}

/// An instruction-accurate ARM Cortex-M0+ (ARMv6-M) core.
pub struct CortexM {
    /// `r0..r15`. `r15` (`PC`) here holds the address of the instruction being
    /// fetched; reads via [`reg`](CortexM::reg) return the Thumb `PC` value.
    regs: [u32; 16],
    /// Condition flags.
    apsr: Apsr,
    /// `PRIMASK`: when set, all maskable exceptions are disabled.
    primask: bool,
    /// Flat RAM mapped from address 0.
    mem: Vec<u8>,
    /// The SIO GPIO peripheral.
    sio: Sio,
    /// Latched pending external IRQs (bit `n` ⇒ IRQ `n`).
    pending_irq: u32,
    /// Pending internal exceptions by exception number (e.g. `SVCall`).
    pending_exc: u32,
    /// `NVIC` interrupt-enable bitmap (`ISER`), bit `n` ⇒ IRQ `n` enabled.
    nvic_enabled: u32,
    /// The address of the instruction currently executing (for PC-relative ops).
    cur_instr_addr: u32,
    /// Elapsed CPU cycles (instruction-accurate).
    cycles: u64,
    /// Clock frequency in Hz (for the cycles→seconds mapping).
    f_cpu: f64,
}

impl Default for CortexM {
    fn default() -> Self {
        Self::new()
    }
}

impl CortexM {
    /// A reset core with empty memory, clocked at 1 MHz by default (boards set
    /// their own clock).
    pub fn new() -> Self {
        let mut core = Self {
            regs: [0; 16],
            apsr: Apsr::default(),
            primask: false,
            mem: vec![0; MEM_SIZE],
            sio: Sio::new(),
            pending_irq: 0,
            pending_exc: 0,
            nvic_enabled: 0,
            cur_instr_addr: 0,
            cycles: 0,
            f_cpu: 1_000_000.0,
        };
        core.reset();
        core
    }

    /// Reset the core: clear the registers/flags, set `SP` to the top of RAM and
    /// `PC` to 0, without touching memory.
    pub fn reset(&mut self) {
        self.regs = [0; 16];
        self.apsr = Apsr::default();
        self.primask = false;
        self.pending_irq = 0;
        self.pending_exc = 0;
        self.nvic_enabled = 0;
        self.cur_instr_addr = 0;
        self.cycles = 0;
        self.regs[SP as usize] = DEFAULT_SP;
        self.regs[PC as usize] = 0;
    }

    /// Set the CPU clock in Hz (affects only the cycles→seconds mapping).
    pub fn set_clock_hz(&mut self, hz: f64) {
        self.f_cpu = hz;
    }

    /// Load a program as raw little-endian halfwords into memory from address 0,
    /// then reset (`SP` → top of RAM, `PC` → 0). A convenience for tests and
    /// hand-assembled snippets — it does **not** apply the reset vector table.
    pub fn load_program(&mut self, halfwords: &[u16]) {
        self.mem.iter_mut().for_each(|b| *b = 0);
        for (i, &hw) in halfwords.iter().enumerate() {
            let a = i * 2;
            if a + 1 < self.mem.len() {
                self.mem[a] = hw as u8;
                self.mem[a + 1] = (hw >> 8) as u8;
            }
        }
        self.reset();
    }

    /// Apply the ARMv6-M reset vector table at address 0: `SP` ← word 0, `PC` ←
    /// word 1 (with the Thumb bit stripped).
    fn apply_vector_table(&mut self) {
        let sp = self.read_u32(0);
        let entry = self.read_u32(4);
        if sp != 0 {
            self.regs[SP as usize] = sp;
        }
        if entry != 0 {
            self.regs[PC as usize] = entry & !1;
        }
    }

    // --- Inspection helpers ---

    /// The `PC` value (Thumb: the fetch address + 4). Use
    /// [`pc_raw`](CortexM::pc_raw) for the next-fetch address.
    pub fn pc(&self) -> u32 {
        self.reg(PC)
    }
    /// The raw next-instruction fetch address (`r15` as stored).
    pub fn pc_raw(&self) -> u32 {
        self.regs[PC as usize]
    }
    /// The stack pointer.
    pub fn sp(&self) -> u32 {
        self.regs[SP as usize]
    }
    /// The condition flags.
    pub fn apsr(&self) -> Apsr {
        self.apsr
    }
    /// Elapsed CPU cycles since reset.
    pub fn cycles(&self) -> u64 {
        self.cycles
    }

    /// Read register `i`. `r15` yields the Thumb `PC` value (fetch address + 4).
    pub fn reg(&self, i: u8) -> u32 {
        if i == PC {
            self.cur_instr_addr.wrapping_add(4)
        } else {
            self.regs[i as usize]
        }
    }

    /// Write register `i`. Writing `r15` branches (clearing the Thumb bit).
    fn set_reg(&mut self, i: u8, v: u32) {
        if i == PC {
            self.regs[PC as usize] = v & !1;
        } else {
            self.regs[i as usize] = v;
        }
    }

    /// Set register `i` directly (tests / board setup). Writing `r15` sets the
    /// raw fetch address.
    pub fn set_reg_raw(&mut self, i: u8, v: u32) {
        self.regs[i as usize] = v;
    }

    /// Shared access to the SIO GPIO peripheral.
    pub fn sio(&self) -> &Sio {
        &self.sio
    }
    /// Mutable access to the SIO GPIO peripheral.
    pub fn sio_mut(&mut self) -> &mut Sio {
        &mut self.sio
    }

    // --- Memory access (flat RAM + SIO) ---

    /// Read a byte from flat memory (0 outside the modelled RAM).
    pub fn read_u8(&self, addr: u32) -> u8 {
        self.mem.get(addr as usize).copied().unwrap_or(0)
    }
    fn write_u8_mem(&mut self, addr: u32, v: u8) {
        if let Some(slot) = self.mem.get_mut(addr as usize) {
            *slot = v;
        }
    }

    /// Read a 16-bit halfword (little-endian).
    pub fn read_u16(&self, addr: u32) -> u16 {
        self.read_u8(addr) as u16 | (self.read_u8(addr.wrapping_add(1)) as u16) << 8
    }

    /// Read a 32-bit word, routing the SIO window to the GPIO peripheral.
    pub fn read_u32(&self, addr: u32) -> u32 {
        if Sio::claims(addr) {
            return self.sio.read(addr);
        }
        self.read_u16(addr) as u32 | (self.read_u16(addr.wrapping_add(2)) as u32) << 16
    }

    /// Write a 16-bit halfword (little-endian) to flat memory.
    pub fn write_u16(&mut self, addr: u32, v: u16) {
        self.write_u8_mem(addr, v as u8);
        self.write_u8_mem(addr.wrapping_add(1), (v >> 8) as u8);
    }

    /// Write a word, routing the SIO window to the GPIO peripheral.
    pub fn write_u32(&mut self, addr: u32, v: u32) {
        if Sio::claims(addr) {
            self.sio.write(addr, v);
            return;
        }
        self.write_u16(addr, v as u16);
        self.write_u16(addr.wrapping_add(2), (v >> 16) as u16);
    }

    /// A sized memory read used by the load instructions, sign-extending when
    /// `signed`.
    fn load(&self, addr: u32, size: Size, signed: bool) -> u32 {
        match size {
            Size::Byte => {
                let b = self.read_u8(addr);
                if signed {
                    b as i8 as i32 as u32
                } else {
                    b as u32
                }
            }
            Size::Half => {
                let h = self.read_u16(addr);
                if signed {
                    h as i16 as i32 as u32
                } else {
                    h as u32
                }
            }
            Size::Word => self.read_u32(addr),
        }
    }

    /// A sized memory write used by the store instructions.
    fn store(&mut self, addr: u32, size: Size, v: u32) {
        match size {
            Size::Byte => self.write_u8_mem(addr, v as u8),
            Size::Half => self.write_u16(addr, v as u16),
            Size::Word => self.write_u32(addr, v),
        }
    }

    // --- Exceptions (NVIC-lite) ---

    /// Mark external interrupt `irq` pending. It is taken before a later
    /// instruction once enabled in the NVIC and not masked by `PRIMASK`.
    pub fn request_interrupt(&mut self, irq: u8) {
        debug_assert!(irq < 32, "IRQ index out of range");
        self.pending_irq |= 1 << irq;
    }

    /// Enable external interrupt `irq` in the (lite) NVIC.
    pub fn enable_irq(&mut self, irq: u8) {
        self.nvic_enabled |= 1 << irq;
    }

    /// The highest-priority exception ready to be taken (lowest number wins),
    /// or `None`. `SVCall` and the like are always ready; an external IRQ must be
    /// enabled, and nothing is taken while `PRIMASK` masks interrupts.
    fn next_exception(&self) -> Option<u32> {
        if self.pending_exc != 0 {
            return Some(EXC_SVCALL); // the only internal exception modelled
        }
        if self.primask {
            return None;
        }
        let ready = self.pending_irq & self.nvic_enabled;
        if ready != 0 {
            Some(EXC_EXTERNAL_BASE + ready.trailing_zeros())
        } else {
            None
        }
    }

    /// Take `exc`: stack the eight-word exception frame, load `PC` from the
    /// vector table, and set `LR` to the `EXC_RETURN` magic value.
    fn enter_exception(&mut self, exc: u32) {
        // Clear the pending latch for the exception being taken.
        if exc == EXC_SVCALL {
            self.pending_exc &= !(1 << EXC_SVCALL);
        } else {
            self.pending_irq &= !(1 << (exc - EXC_EXTERNAL_BASE));
        }
        // Stack {r0, r1, r2, r3, r12, LR, return_addr, xPSR}, SP descending.
        let return_addr = self.regs[PC as usize];
        let xpsr = self.apsr.to_xpsr();
        let frame = [
            self.regs[0],
            self.regs[1],
            self.regs[2],
            self.regs[3],
            self.regs[12],
            self.regs[LR as usize],
            return_addr,
            xpsr,
        ];
        let mut sp = self.regs[SP as usize];
        for &w in frame.iter().rev() {
            sp = sp.wrapping_sub(4);
            self.write_u32(sp, w);
        }
        self.regs[SP as usize] = sp;
        self.regs[LR as usize] = EXC_RETURN_THREAD_MSP;
        self.regs[PC as usize] = self.read_u32(exc * 4) & !1;
        self.cycles += 16; // approximate entry cost
    }

    /// Return from an exception (a `BX`/`POP` to an `EXC_RETURN` value): unstack
    /// the frame and resume the interrupted code.
    fn exception_return(&mut self) {
        let sp = self.regs[SP as usize];
        // Unstack {r0, r1, r2, r3, r12, LR, return_addr, xPSR} in ascending order.
        self.regs[0] = self.read_u32(sp);
        self.regs[1] = self.read_u32(sp.wrapping_add(4));
        self.regs[2] = self.read_u32(sp.wrapping_add(8));
        self.regs[3] = self.read_u32(sp.wrapping_add(12));
        self.regs[12] = self.read_u32(sp.wrapping_add(16));
        self.regs[LR as usize] = self.read_u32(sp.wrapping_add(20));
        let return_addr = self.read_u32(sp.wrapping_add(24));
        let xpsr = self.read_u32(sp.wrapping_add(28));
        self.regs[SP as usize] = sp.wrapping_add(32);
        self.regs[PC as usize] = return_addr & !1;
        self.apsr.load_xpsr(xpsr);
        self.cycles += 12; // approximate return cost
    }

    // --- Flag-setting arithmetic & shifts ---

    /// `a + b + carry` with the ARM carry/overflow semantics.
    fn add_with_carry(a: u32, b: u32, carry_in: bool) -> (u32, bool, bool) {
        let (s1, c1) = a.overflowing_add(b);
        let (s2, c2) = s1.overflowing_add(carry_in as u32);
        let carry = c1 || c2;
        let sa = a >> 31;
        let sb = b >> 31;
        let sr = s2 >> 31;
        let overflow = sa == sb && sa != sr;
        (s2, carry, overflow)
    }

    /// Set N/Z from a result; C and V unchanged (the logical-op flag update).
    fn set_nz(&mut self, r: u32) {
        self.apsr.n = r & 0x8000_0000 != 0;
        self.apsr.z = r == 0;
    }

    /// Set N/Z/C/V from an add/sub result.
    fn set_nzcv(&mut self, r: u32, c: bool, v: bool) {
        self.apsr.n = r & 0x8000_0000 != 0;
        self.apsr.z = r == 0;
        self.apsr.c = c;
        self.apsr.v = v;
    }
}

/// `LSL` with shifter carry-out. A zero shift passes the carry through unchanged.
fn lsl_c(val: u32, shift: u32, carry_in: bool) -> (u32, bool) {
    match shift {
        0 => (val, carry_in),
        1..=31 => (val << shift, (val >> (32 - shift)) & 1 != 0),
        32 => (0, val & 1 != 0),
        _ => (0, false),
    }
}

/// `LSR` with shifter carry-out.
fn lsr_c(val: u32, shift: u32, carry_in: bool) -> (u32, bool) {
    match shift {
        0 => (val, carry_in),
        1..=31 => (val >> shift, (val >> (shift - 1)) & 1 != 0),
        32 => (0, val >> 31 != 0),
        _ => (0, false),
    }
}

/// `ASR` with shifter carry-out.
fn asr_c(val: u32, shift: u32, carry_in: bool) -> (u32, bool) {
    match shift {
        0 => (val, carry_in),
        1..=31 => (
            ((val as i32) >> shift) as u32,
            (val >> (shift - 1)) & 1 != 0,
        ),
        _ => {
            // A shift of 32 or more fills with the sign bit.
            let sign = val & 0x8000_0000 != 0;
            (if sign { 0xFFFF_FFFF } else { 0 }, sign)
        }
    }
}

/// `ROR` with shifter carry-out. A zero shift passes the carry through.
fn ror_c(val: u32, shift: u32, carry_in: bool) -> (u32, bool) {
    if shift == 0 {
        return (val, carry_in);
    }
    let s = shift % 32;
    if s == 0 {
        (val, val >> 31 != 0)
    } else {
        (val.rotate_right(s), (val >> (s - 1)) & 1 != 0)
    }
}

/// Evaluate an ARM condition code against the flags.
fn cond_holds(cond: u8, a: Apsr) -> bool {
    match cond {
        0b0000 => a.z,                  // EQ
        0b0001 => !a.z,                 // NE
        0b0010 => a.c,                  // CS/HS
        0b0011 => !a.c,                 // CC/LO
        0b0100 => a.n,                  // MI
        0b0101 => !a.n,                 // PL
        0b0110 => a.v,                  // VS
        0b0111 => !a.v,                 // VC
        0b1000 => a.c && !a.z,          // HI
        0b1001 => !a.c || a.z,          // LS
        0b1010 => a.n == a.v,           // GE
        0b1011 => a.n != a.v,           // LT
        0b1100 => !a.z && (a.n == a.v), // GT
        0b1101 => a.z || (a.n != a.v),  // LE
        _ => true,                      // AL (and any reserved)
    }
}

impl CortexM {
    /// Execute one decoded instruction, returning the cycles it consumed. `PC`
    /// (the raw fetch address) has already been advanced past this instruction.
    fn execute(&mut self, instr: Instr) -> u64 {
        match instr {
            Instr::Nop => 1,

            Instr::MovImm { rd, imm, setflags } => {
                self.set_reg(rd, imm);
                if setflags {
                    self.set_nz(imm);
                }
                1
            }
            Instr::MovReg { rd, rm, setflags } => {
                let v = self.reg(rm);
                if setflags {
                    self.set_nz(v);
                }
                self.set_reg(rd, v);
                if rd == PC {
                    2
                } else {
                    1
                }
            }

            Instr::AddReg {
                rd,
                rn,
                rm,
                setflags,
            } => {
                let (r, c, v) = Self::add_with_carry(self.reg(rn), self.reg(rm), false);
                if setflags {
                    self.set_nzcv(r, c, v);
                }
                self.set_reg(rd, r);
                if rd == PC {
                    2
                } else {
                    1
                }
            }
            Instr::AddImm {
                rd,
                rn,
                imm,
                setflags,
            } => {
                let (r, c, v) = Self::add_with_carry(self.reg(rn), imm, false);
                if setflags {
                    self.set_nzcv(r, c, v);
                }
                self.set_reg(rd, r);
                1
            }
            Instr::SubReg {
                rd,
                rn,
                rm,
                setflags,
            } => {
                let (r, c, v) = Self::add_with_carry(self.reg(rn), !self.reg(rm), true);
                if setflags {
                    self.set_nzcv(r, c, v);
                }
                self.set_reg(rd, r);
                1
            }
            Instr::SubImm {
                rd,
                rn,
                imm,
                setflags,
            } => {
                let (r, c, v) = Self::add_with_carry(self.reg(rn), !imm, true);
                if setflags {
                    self.set_nzcv(r, c, v);
                }
                self.set_reg(rd, r);
                1
            }
            Instr::Rsb { rd, rn } => {
                let (r, c, v) = Self::add_with_carry(!self.reg(rn), 0, true);
                self.set_nzcv(r, c, v);
                self.set_reg(rd, r);
                1
            }
            Instr::Adc { rd, rm } => {
                let (r, c, v) = Self::add_with_carry(self.reg(rd), self.reg(rm), self.apsr.c);
                self.set_nzcv(r, c, v);
                self.set_reg(rd, r);
                1
            }
            Instr::Sbc { rd, rm } => {
                let (r, c, v) = Self::add_with_carry(self.reg(rd), !self.reg(rm), self.apsr.c);
                self.set_nzcv(r, c, v);
                self.set_reg(rd, r);
                1
            }

            Instr::CmpReg { rn, rm } => {
                let (r, c, v) = Self::add_with_carry(self.reg(rn), !self.reg(rm), true);
                self.set_nzcv(r, c, v);
                1
            }
            Instr::CmpImm { rn, imm } => {
                let (r, c, v) = Self::add_with_carry(self.reg(rn), !imm, true);
                self.set_nzcv(r, c, v);
                1
            }
            Instr::Cmn { rn, rm } => {
                let (r, c, v) = Self::add_with_carry(self.reg(rn), self.reg(rm), false);
                self.set_nzcv(r, c, v);
                1
            }
            Instr::Tst { rn, rm } => {
                let r = self.reg(rn) & self.reg(rm);
                self.set_nz(r);
                1
            }

            Instr::And { rd, rm } => {
                let r = self.reg(rd) & self.reg(rm);
                self.set_nz(r);
                self.set_reg(rd, r);
                1
            }
            Instr::Orr { rd, rm } => {
                let r = self.reg(rd) | self.reg(rm);
                self.set_nz(r);
                self.set_reg(rd, r);
                1
            }
            Instr::Eor { rd, rm } => {
                let r = self.reg(rd) ^ self.reg(rm);
                self.set_nz(r);
                self.set_reg(rd, r);
                1
            }
            Instr::Bic { rd, rm } => {
                let r = self.reg(rd) & !self.reg(rm);
                self.set_nz(r);
                self.set_reg(rd, r);
                1
            }
            Instr::Mvn { rd, rm } => {
                let r = !self.reg(rm);
                self.set_nz(r);
                self.set_reg(rd, r);
                1
            }
            Instr::Mul { rd, rm } => {
                let r = self.reg(rd).wrapping_mul(self.reg(rm));
                self.set_nz(r);
                self.set_reg(rd, r);
                1
            }

            Instr::LslImm { rd, rm, imm } => {
                let (r, c) = lsl_c(self.reg(rm), imm as u32, self.apsr.c);
                self.set_nz(r);
                self.apsr.c = c;
                self.set_reg(rd, r);
                1
            }
            Instr::LsrImm { rd, rm, imm } => {
                let shift = if imm == 0 { 32 } else { imm as u32 };
                let (r, c) = lsr_c(self.reg(rm), shift, self.apsr.c);
                self.set_nz(r);
                self.apsr.c = c;
                self.set_reg(rd, r);
                1
            }
            Instr::AsrImm { rd, rm, imm } => {
                let shift = if imm == 0 { 32 } else { imm as u32 };
                let (r, c) = asr_c(self.reg(rm), shift, self.apsr.c);
                self.set_nz(r);
                self.apsr.c = c;
                self.set_reg(rd, r);
                1
            }
            Instr::LslReg { rd, rm } => {
                let (r, c) = lsl_c(self.reg(rd), self.reg(rm) & 0xFF, self.apsr.c);
                self.set_nz(r);
                self.apsr.c = c;
                self.set_reg(rd, r);
                1
            }
            Instr::LsrReg { rd, rm } => {
                let (r, c) = lsr_c(self.reg(rd), self.reg(rm) & 0xFF, self.apsr.c);
                self.set_nz(r);
                self.apsr.c = c;
                self.set_reg(rd, r);
                1
            }
            Instr::AsrReg { rd, rm } => {
                let (r, c) = asr_c(self.reg(rd), self.reg(rm) & 0xFF, self.apsr.c);
                self.set_nz(r);
                self.apsr.c = c;
                self.set_reg(rd, r);
                1
            }
            Instr::RorReg { rd, rm } => {
                let (r, c) = ror_c(self.reg(rd), self.reg(rm) & 0xFF, self.apsr.c);
                self.set_nz(r);
                self.apsr.c = c;
                self.set_reg(rd, r);
                1
            }

            Instr::Extend {
                rd,
                rm,
                size,
                signed,
            } => {
                let v = self.reg(rm);
                let r = match (size, signed) {
                    (Size::Half, true) => v as u16 as i16 as i32 as u32,
                    (Size::Half, false) => v as u16 as u32,
                    (Size::Byte, true) => v as u8 as i8 as i32 as u32,
                    _ => v as u8 as u32,
                };
                self.set_reg(rd, r);
                1
            }
            Instr::Rev { rd, rm, op } => {
                let v = self.reg(rm);
                let r = match op {
                    0 => v.swap_bytes(),
                    1 => {
                        // REV16: byte-reverse within each halfword.
                        ((v & 0x00FF_00FF) << 8) | ((v & 0xFF00_FF00) >> 8)
                    }
                    _ => {
                        // REVSH: reverse low halfword, sign-extend.
                        let low = (v as u16).swap_bytes();
                        low as i16 as i32 as u32
                    }
                };
                self.set_reg(rd, r);
                1
            }

            Instr::Mem(m) => self.exec_mem(m),

            Instr::Adr { rd, imm } => {
                let base = self.reg(PC) & !0b11; // aligned PC
                self.set_reg(rd, base.wrapping_add(imm));
                1
            }
            Instr::AddSpImm { rd, imm } => {
                let r = self.reg(SP).wrapping_add(imm);
                self.set_reg(rd, r);
                1
            }
            Instr::AddSp { imm } => {
                let r = self.reg(SP).wrapping_add(imm);
                self.set_reg(SP, r);
                1
            }
            Instr::SubSp { imm } => {
                let r = self.reg(SP).wrapping_sub(imm);
                self.set_reg(SP, r);
                1
            }

            Instr::Push { regs, lr } => self.exec_push(regs, lr),
            Instr::Pop { regs, pc } => self.exec_pop(regs, pc),
            Instr::Stm { rn, regs } => self.exec_stm(rn, regs),
            Instr::Ldm { rn, regs } => self.exec_ldm(rn, regs),

            Instr::B { offset } => {
                let target = self.reg(PC).wrapping_add(offset as u32);
                self.set_reg(PC, target);
                3
            }
            Instr::Bcond { cond, offset } => {
                if cond_holds(cond, self.apsr) {
                    let target = self.reg(PC).wrapping_add(offset as u32);
                    self.set_reg(PC, target);
                    3
                } else {
                    1
                }
            }
            Instr::Bl { offset } => {
                // LR ← address of the next instruction, Thumb bit set.
                self.regs[LR as usize] = self.regs[PC as usize] | 1;
                let target = self.reg(PC).wrapping_add(offset as u32);
                self.set_reg(PC, target);
                4
            }
            Instr::Bx { rm } => {
                let target = self.reg(rm);
                if target >= 0xF000_0000 {
                    self.exception_return();
                } else {
                    self.set_reg(PC, target);
                }
                3
            }
            Instr::Blx { rm } => {
                let target = self.reg(rm);
                self.regs[LR as usize] = self.regs[PC as usize] | 1;
                self.set_reg(PC, target);
                3
            }
            Instr::Svc { .. } => {
                self.pending_exc |= 1 << EXC_SVCALL;
                1
            }

            Instr::Unknown(word) => {
                debug_assert!(
                    false,
                    "unimplemented ARMv6-M opcode {word:#06x} at pc {:#010x}",
                    self.cur_instr_addr
                );
                1
            }
        }
    }

    /// A load or store (`Instr::Mem`).
    fn exec_mem(&mut self, m: Mem) -> u64 {
        // The base: an aligned PC for a literal access, else the register.
        let base = if m.rn == PC {
            self.reg(PC) & !0b11
        } else {
            self.reg(m.rn)
        };
        let addr = match m.addr {
            AddrMode::Imm(off) => base.wrapping_add(off),
            AddrMode::Reg(rm) => base.wrapping_add(self.reg(rm)),
        };
        if m.load {
            let v = self.load(addr, m.size, m.signed);
            self.set_reg(m.rt, v);
            2
        } else {
            self.store(addr, m.size, self.reg(m.rt));
            2
        }
    }

    fn exec_push(&mut self, regs: u8, lr: bool) -> u64 {
        let count = regs.count_ones() + lr as u32;
        let mut sp = self.reg(SP);
        // Push LR first (highest address), then r7..r0 descending.
        if lr {
            sp = sp.wrapping_sub(4);
            self.write_u32(sp, self.regs[LR as usize]);
        }
        for r in (0..8).rev() {
            if regs & (1 << r) != 0 {
                sp = sp.wrapping_sub(4);
                self.write_u32(sp, self.regs[r as usize]);
            }
        }
        self.set_reg(SP, sp);
        1 + count as u64
    }

    fn exec_pop(&mut self, regs: u8, pc: bool) -> u64 {
        let count = regs.count_ones() + pc as u32;
        let mut sp = self.reg(SP);
        for r in 0..8 {
            if regs & (1 << r) != 0 {
                self.regs[r as usize] = self.read_u32(sp);
                sp = sp.wrapping_add(4);
            }
        }
        if pc {
            let target = self.read_u32(sp);
            sp = sp.wrapping_add(4);
            self.set_reg(SP, sp);
            if target >= 0xF000_0000 {
                self.exception_return();
            } else {
                self.set_reg(PC, target);
            }
            return 3 + count as u64;
        }
        self.set_reg(SP, sp);
        1 + count as u64
    }

    fn exec_stm(&mut self, rn: u8, regs: u8) -> u64 {
        let mut addr = self.reg(rn);
        for r in 0..8 {
            if regs & (1 << r) != 0 {
                self.write_u32(addr, self.regs[r as usize]);
                addr = addr.wrapping_add(4);
            }
        }
        self.set_reg(rn, addr);
        1 + regs.count_ones() as u64
    }

    fn exec_ldm(&mut self, rn: u8, regs: u8) -> u64 {
        let mut addr = self.reg(rn);
        for r in 0..8 {
            if regs & (1 << r) != 0 {
                self.regs[r as usize] = self.read_u32(addr);
                addr = addr.wrapping_add(4);
            }
        }
        // The base is written back only when it is not in the load list.
        if regs & (1 << rn) == 0 {
            self.set_reg(rn, addr);
        }
        1 + regs.count_ones() as u64
    }
}

impl McuCore for CortexM {
    fn step(&mut self) {
        // Take a pending exception before fetching the next instruction.
        if let Some(exc) = self.next_exception() {
            self.enter_exception(exc);
            return;
        }
        let addr = self.regs[PC as usize];
        self.cur_instr_addr = addr;
        let w0 = self.read_u16(addr);
        let w1 = self.read_u16(addr.wrapping_add(2));
        let (instr, len) = decode(w0, w1);
        self.regs[PC as usize] = addr.wrapping_add(len as u32);
        let cycles = self.execute(instr);
        self.cycles += cycles;
    }

    fn step_to(&mut self, t: f64) {
        while (self.cycles as f64) / self.f_cpu < t {
            self.step();
        }
    }

    fn load_firmware(&mut self, image: &[u8]) -> Result<(), FirmwareError> {
        if image.is_empty() {
            return Err(FirmwareError::BadImage);
        }
        // ELF is recognised but not yet supported.
        if image.starts_with(&[0x7F, b'E', b'L', b'F']) {
            return Err(FirmwareError::UnsupportedFormat);
        }
        if image.len() > self.mem.len() {
            return Err(FirmwareError::BadImage);
        }
        self.mem.iter_mut().for_each(|b| *b = 0);
        self.mem[..image.len()].copy_from_slice(image);
        self.reset();
        self.apply_vector_table();
        Ok(())
    }

    fn pin_count(&self) -> usize {
        PIN_COUNT
    }

    fn next_event(&self) -> Option<u64> {
        // The modelled SIO has no scheduled events; the core free-runs.
        None
    }

    fn step_to_cycle(&mut self, until_cycle: u64, io: &mut PinIo) -> u64 {
        self.sio.sample_inputs(io);
        while self.cycles < until_cycle {
            self.step();
        }
        self.sio.publish_drives(io);
        self.cycles
    }
}

impl CortexM {
    /// A single co-simulation step exchanging pin state: sample inputs, execute
    /// one instruction, publish drives. The fine-grained counterpart to
    /// [`step_to_cycle`](McuCore::step_to_cycle).
    pub fn step_with_io(&mut self, io: &mut PinIo) {
        self.sio.sample_inputs(io);
        self.step();
        self.sio.publish_drives(io);
    }

    /// The resolved logic level on `GPIO<n>` (convenience for tests/boards).
    pub fn gpio_level(&self, gpio: u8) -> bool {
        self.sio.pin_level(gpio)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_program(halfwords: &[u16]) -> CortexM {
        let mut c = CortexM::new();
        c.load_program(halfwords);
        c
    }

    fn run(c: &mut CortexM, n: usize) {
        for _ in 0..n {
            c.step();
        }
    }

    #[test]
    fn movs_sets_register_and_flags() {
        // MOVS r0, #0x2A ; MOVS r1, #0.
        let mut c = with_program(&[0x202A, 0x2100]);
        c.step(); // MOVS r0,#0x2A
        assert_eq!(c.reg(0), 0x2A);
        assert!(!c.apsr().z, "0x2A is non-zero");
        c.step(); // MOVS r1,#0
        assert_eq!(c.reg(1), 0);
        assert!(c.apsr().z, "MOVS #0 sets Z");
    }

    #[test]
    fn adds_sets_carry_and_zero_on_wrap() {
        // MOVS r0,#1 ; MVNS r1,r0 (r1=0xFFFFFFFE) ; MOVS r2,#2 ; ADDS r1,r1,r2
        //  0xFFFFFFFE + 2 = 0 with carry + zero, no signed overflow.
        // Encodings: MOVS r0,#1 =0x2001 ; MVNS r1,r0 =0x43C1 ; MOVS r2,#2 =0x2202
        //            ADDS r1,r1,r2 =0x1889 (rd=1,rn=1,rm=2).
        let mut c = with_program(&[0x2001, 0x43C1, 0x2202, 0x1889]);
        run(&mut c, 4);
        assert_eq!(c.reg(1), 0);
        assert!(c.apsr().z, "result zero");
        assert!(c.apsr().c, "carry out");
        assert!(!c.apsr().v, "no signed overflow");
    }

    #[test]
    fn subs_sets_borrow_flags() {
        // MOVS r0,#0x10 ; MOVS r1,#0x20 ; SUBS r0,r0,r1 → 0xFFFFFFF0, N set, no C.
        // SUBS r0,r0,r1 = 0x1A40 (rd=0,rn=0,rm=1).
        let mut c = with_program(&[0x2010, 0x2120, 0x1A40]);
        run(&mut c, 3);
        assert_eq!(c.reg(0), 0xFFFF_FFF0);
        assert!(c.apsr().n, "negative result");
        assert!(!c.apsr().c, "borrow clears C");
    }

    #[test]
    fn logic_and_shift_ops() {
        // MOVS r0,#0xFF ; MOVS r1,#0x0F ; ANDS r0,r1 → 0x0F.
        // ANDS r0,r1 = 0x4008.
        let mut c = with_program(&[0x20FF, 0x210F, 0x4008]);
        run(&mut c, 3);
        assert_eq!(c.reg(0), 0x0F);

        // MOVS r0,#1 ; LSLS r0,r0,#4 → 0x10, C from the shift is 0.
        // LSLS r0,r0,#4 = 0x0100.
        let mut c = with_program(&[0x2001, 0x0100]);
        run(&mut c, 2);
        assert_eq!(c.reg(0), 0x10);
    }

    #[test]
    fn ldr_literal_loads_the_pool_word() {
        // LDR r0, [pc, #4] then branch past the pool.
        //  0: LDR r0,[pc,#4]  (0x4801) → PC-aligned(0)+4 + 4 = addr 8
        //  2: B .+2 (skip the pool)    (0xE001 → to 0x8? compute below)
        //  4,5: literal 0xDEADBEEF at addr 8..
        // Simpler: place the literal at word offset 2 (byte 8).
        //  0: 0x4801  LDR r0,[pc,#4]  ; base=align(PC)=align(0+4)=4; +4 = 8
        //  2: 0x0000  (MOVS r0,r0 / filler)
        //  4..: literal
        let mut c = CortexM::new();
        c.load_program(&[0x4801, 0x0000, 0x0000, 0x0000]);
        // Put 0xDEADBEEF at byte address 8.
        c.write_u32(8, 0xDEAD_BEEF);
        c.step(); // LDR
        assert_eq!(c.reg(0), 0xDEAD_BEEF);
    }

    #[test]
    fn str_then_ldr_round_trips_through_memory() {
        // MOVS r0,#0x55 ; MOVS r1,#0x40 (base addr 0x40) ; STR r0,[r1,#0] ;
        // LDR r2,[r1,#0].
        //  STR r0,[r1,#0] = 0x6008 ; LDR r2,[r1,#0] = 0x680A.
        let mut c = with_program(&[0x2055, 0x2140, 0x6008, 0x680A]);
        run(&mut c, 4);
        assert_eq!(c.read_u32(0x40), 0x55);
        assert_eq!(c.reg(2), 0x55);
    }

    #[test]
    fn push_and_pop_balance_the_stack() {
        // MOVS r0,#0xAB ; PUSH {r0} ; MOVS r0,#0 ; POP {r1}.
        // PUSH {r0} = 0xB401 ; POP {r1} = 0xBC02.
        let mut c = with_program(&[0x20AB, 0xB401, 0x2000, 0xBC02]);
        let sp0 = c.sp();
        c.step(); // MOVS
        c.step(); // PUSH
        assert_eq!(c.sp(), sp0 - 4);
        c.step(); // MOVS r0,#0
        c.step(); // POP {r1}
        assert_eq!(c.sp(), sp0, "stack balanced");
        assert_eq!(c.reg(1), 0xAB);
    }

    #[test]
    fn conditional_branch_builds_a_countdown_loop() {
        // MOVS r0,#5 ; MOVS r1,#0
        // loop: ADDS r1,#1 ; SUBS r0,#1 ; BNE loop
        //  0: MOVS r0,#5  0x2005
        //  2: MOVS r1,#0  0x2100
        //  4: ADDS r1,#1  0x3101
        //  6: SUBS r0,#1  0x3801
        //  8: BNE loop (→ addr 4): PC(=8+4=12)+off=4 → off=-8 → imm8=-4 →0xD1FC
        let mut c = with_program(&[0x2005, 0x2100, 0x3101, 0x3801, 0xD1FC]);
        run(&mut c, 2 + 5 * 3);
        assert_eq!(c.reg(0), 0, "counted to zero");
        assert_eq!(c.reg(1), 5, "loop body ran five times");
        assert!(c.apsr().z);
    }

    #[test]
    fn bl_and_bx_lr_call_and_return() {
        //  0: BL +4 (to addr 8)   0xF000 0xF802  (offset = 4 → to PC(4)+4=8)
        //  4: MOVS r1,#2          0x2102   (executed after return)
        //  6: B . (self)          0xE7FE
        //  8: MOVS r0,#7          0x2007   (the "subroutine")
        // 10: BX LR               0x4770
        let mut c = with_program(&[0xF000, 0xF802, 0x2102, 0xE7FE, 0x2007, 0x4770]);
        c.step(); // BL → pc=8, lr=5 (addr4|1)
        assert_eq!(c.pc_raw(), 8);
        assert_eq!(c.reg(LR), 5);
        c.step(); // MOVS r0,#7
        assert_eq!(c.reg(0), 7);
        c.step(); // BX LR → return to addr 4
        assert_eq!(c.pc_raw(), 4);
        c.step(); // MOVS r1,#2
        assert_eq!(c.reg(1), 2);
    }

    #[test]
    fn svc_dispatches_to_its_vector_and_returns() {
        // Vector table: SP at word 0, reset at word 1, SVCall (exc 11) at word 11
        //  (byte 44). We hand-build memory: program at 0x100, SVC handler at
        //  0x200.
        let mut c = CortexM::new();
        // Program at 0x100: SVC #0 ; B . (self).
        c.write_u16(0x100, 0xDF00); // SVC #0
        c.write_u16(0x102, 0xE7FE); // B .
                                    // SVC handler at 0x200: MOVS r0,#0x99 ; BX LR (EXC_RETURN).
        c.write_u16(0x200, 0x2099); // MOVS r0,#0x99
        c.write_u16(0x202, 0x4770); // BX LR
                                    // Vector table entry for exception 11 → 0x200 | thumb.
        c.write_u32(EXC_SVCALL * 4, 0x200 | 1);
        // Start executing at 0x100 with a valid stack.
        c.set_reg_raw(SP, DEFAULT_SP);
        c.set_reg_raw(PC, 0x100);

        c.step(); // SVC → marks SVCall pending
        let sp_after_svc = c.sp();
        c.step(); // take the exception: stack frame, jump to handler
        assert_eq!(c.pc_raw(), 0x200, "vectored to the SVC handler");
        assert_eq!(c.sp(), sp_after_svc - 32, "eight-word frame stacked");
        assert_eq!(c.reg(LR), EXC_RETURN_THREAD_MSP, "LR = EXC_RETURN");
        c.step(); // MOVS r0,#0x99
        assert_eq!(c.reg(0), 0x99);
        c.step(); // BX LR → exception return
        assert_eq!(c.pc_raw(), 0x102, "returned to the instruction after SVC");
        assert_eq!(c.sp(), sp_after_svc, "stack unwound");
    }

    #[test]
    fn external_irq_dispatch_and_return() {
        // Program at 0: B . (self). IRQ0 handler (exc 16) at 0x300. The handler
        // writes r4 — a register the exception frame does NOT stack/restore — so
        // its effect survives the exception return (r0..r3 would be restored).
        let mut c = CortexM::new();
        c.write_u16(0, 0xE7FE); // B . at addr 0
        c.write_u16(0x300, 0x24C3); // MOVS r4,#0xC3
        c.write_u16(0x302, 0x4770); // BX LR
        c.write_u32(EXC_EXTERNAL_BASE * 4, 0x300 | 1);
        c.set_reg_raw(SP, DEFAULT_SP);
        c.set_reg_raw(PC, 0);

        c.enable_irq(0);
        c.request_interrupt(0);
        c.step(); // take IRQ0
        assert_eq!(c.pc_raw(), 0x300);
        c.step(); // MOVS r4,#0xC3
        c.step(); // BX LR → return to the spinning B
        assert_eq!(c.reg(4), 0xC3, "handler ran");
        assert_eq!(c.pc_raw(), 0, "returned to the interrupted B");
    }

    #[test]
    fn step_to_advances_by_the_clock() {
        let mut c = with_program(&[0xE7FE]); // B . (self), 3 cycles each
        c.set_clock_hz(1_000_000.0);
        c.step_to(100e-6);
        assert!(c.cycles() >= 100, "ran at least 100 cycles: {}", c.cycles());
    }

    #[test]
    fn load_firmware_applies_the_vector_table() {
        // A raw image: SP=0x20000, reset=0x9 (addr 8 | thumb), then at byte 8 a
        // MOVS r0,#0x7. Words (LE): [0]=0x00020000 [1]=0x00000009 [2]=MOVS.
        let image = {
            let mut v = vec![0u8; 12];
            v[0..4].copy_from_slice(&0x0002_0000u32.to_le_bytes());
            v[4..8].copy_from_slice(&0x0000_0009u32.to_le_bytes());
            v[8..10].copy_from_slice(&0x2007u16.to_le_bytes()); // MOVS r0,#7
            v
        };
        let mut c = CortexM::new();
        c.load_firmware(&image).unwrap();
        assert_eq!(c.sp(), 0x0002_0000, "SP from vector[0]");
        assert_eq!(c.pc_raw(), 8, "PC from vector[1], thumb bit stripped");
        c.step();
        assert_eq!(c.reg(0), 7);
    }
}
