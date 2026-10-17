//! The ATmega328P core: register file, memory model, interrupt controller, and
//! the fetch/decode/execute step loop.
//!
//! # Memory model
//!
//! The AVR data space is a single byte-addressed space that overlays the
//! register file, the I/O registers, and SRAM:
//!
//! | range           | contents                                   |
//! |-----------------|--------------------------------------------|
//! | `0x0000..0x0020`| the 32 general-purpose registers `r0..r31` |
//! | `0x0020..0x0100`| I/O and extended-I/O registers             |
//! | `0x0100..0x0900`| 2 KiB internal SRAM                        |
//!
//! `SREG` (`0x5F`), `SPL` (`0x5D`) and `SPH` (`0x5E`) live in the I/O window but
//! are kept as dedicated fields ([`Sreg`], [`Avr::sp`]) so there is a single
//! source of truth; data-space accesses to those addresses route to the fields.
//! `IN`/`OUT` address the I/O window via `addr + 0x20`.
//!
//! Flash is 16 K words (32 KiB) of 16-bit instructions; the program counter
//! ([`Avr::pc`]) is a **word** index.
//!
//! # Interrupts
//!
//! A simple latched controller: [`Avr::request_interrupt`] marks a vector
//! pending. Before each instruction, if the global interrupt enable (`I`) is set
//! and a vector is pending, the core dispatches the lowest-numbered (highest
//! priority) one — pushing the return address, clearing `I`, and jumping to the
//! vector. `RETI` pops the address and re-enables interrupts.

use crate::{FirmwareError, McuCore};

use super::decode::decode;
use super::isa::{Instr, Ptr, PtrMode};
use super::periph::{Peripherals, Port};
use super::sreg::Sreg;

/// Flash size in 16-bit words (ATmega328P: 32 KiB).
const FLASH_WORDS: usize = 16 * 1024;
/// Internal SRAM size in bytes (ATmega328P: 2 KiB).
const SRAM_SIZE: usize = 2 * 1024;
/// First data-space address of SRAM.
const SRAM_START: u16 = 0x0100;
/// First data-space address of the I/O window.
const IO_START: u16 = 0x0020;
/// Highest valid data address (`RAMEND`): the stack pointer's reset value.
const RAMEND: u16 = SRAM_START + SRAM_SIZE as u16 - 1; // 0x08FF
/// Data-space addresses of `SPL`/`SPH`/`SREG`.
const SPL_ADDR: u16 = 0x5D;
const SPH_ADDR: u16 = 0x5E;
const SREG_ADDR: u16 = 0x5F;
/// Each interrupt vector is two words apart on the ATmega328P (it uses `JMP`).
const VECTOR_SIZE_WORDS: u16 = 2;
/// Default clock, matching the Arduino Uno (16 MHz).
const DEFAULT_F_CPU: f64 = 16_000_000.0;

/// An instruction-accurate ATmega328P core.
pub struct Avr {
    /// Program flash, one 16-bit instruction word per entry.
    flash: Vec<u16>,
    /// The 32 general-purpose registers `r0..r31`.
    regs: [u8; 32],
    /// The I/O window `0x20..0x100` (minus the `SP`/`SREG` fields).
    io: [u8; (SRAM_START - IO_START) as usize],
    /// 2 KiB of internal SRAM.
    sram: [u8; SRAM_SIZE],
    /// Status register.
    sreg: Sreg,
    /// Stack pointer (full 16-bit data address of the next free byte).
    sp: u16,
    /// Program counter, a **word** index into [`flash`](Avr::flash).
    pc: u16,
    /// Latched pending-interrupt bitmap: bit `n` ⇒ vector `n` pending.
    pending: u32,
    /// Elapsed CPU cycles (instruction-accurate: one bump per instruction).
    cycles: u64,
    /// Clock frequency in Hz, used to map cycles onto simulated seconds.
    f_cpu: f64,
    /// Memory-mapped peripherals (GPIO, timers/PWM, ADC, USART).
    periph: Peripherals,
}

impl Default for Avr {
    fn default() -> Self {
        Self::new()
    }
}

impl Avr {
    /// A reset core with empty flash and the Arduino Uno's 16 MHz clock.
    pub fn new() -> Self {
        let mut avr = Self {
            flash: vec![0; FLASH_WORDS],
            regs: [0; 32],
            io: [0; (SRAM_START - IO_START) as usize],
            sram: [0; SRAM_SIZE],
            sreg: Sreg::default(),
            sp: RAMEND,
            pc: 0,
            pending: 0,
            cycles: 0,
            f_cpu: DEFAULT_F_CPU,
            periph: Peripherals::new(),
        };
        avr.reset();
        avr
    }

    /// Reset the core state (registers, SREG, SP, PC, pending interrupts, and the
    /// cycle counter) without touching flash.
    pub fn reset(&mut self) {
        self.regs = [0; 32];
        self.io = [0; (SRAM_START - IO_START) as usize];
        self.sram = [0; SRAM_SIZE];
        self.sreg = Sreg::default();
        self.sp = RAMEND;
        self.pc = 0;
        self.pending = 0;
        self.cycles = 0;
        self.periph = Peripherals::new();
    }

    /// Set the CPU clock in Hz (affects only the cycles-to-seconds mapping).
    pub fn set_clock_hz(&mut self, hz: f64) {
        self.f_cpu = hz;
    }

    /// Load a program as raw 16-bit words straight into flash from word 0, then
    /// reset. A convenience for tests and hand-assembled snippets.
    pub fn load_program(&mut self, words: &[u16]) {
        self.flash.iter_mut().for_each(|w| *w = 0);
        for (i, &w) in words.iter().enumerate().take(FLASH_WORDS) {
            self.flash[i] = w;
        }
        self.reset();
    }

    // --- Inspection helpers (used by tests and, later, the debugger) ---

    /// The current program counter (word index).
    pub fn pc(&self) -> u16 {
        self.pc
    }
    /// The current stack pointer.
    pub fn sp(&self) -> u16 {
        self.sp
    }
    /// The status register.
    pub fn sreg(&self) -> Sreg {
        self.sreg
    }
    /// Elapsed CPU cycles since reset.
    pub fn cycles(&self) -> u64 {
        self.cycles
    }
    /// Read general-purpose register `i` (`0..32`).
    pub fn reg(&self, i: u8) -> u8 {
        self.regs[i as usize]
    }
    /// Write general-purpose register `i` (`0..32`).
    pub fn set_reg(&mut self, i: u8, v: u8) {
        self.regs[i as usize] = v;
    }
    /// Read a data-space byte (registers / I/O / SRAM).
    ///
    /// Addresses claimed by a peripheral route to [`Peripherals::read`]; the rest
    /// of the I/O window is plain storage.
    pub fn read_data(&self, addr: u16) -> u8 {
        match addr {
            0x00..=0x1F => self.regs[addr as usize],
            SPL_ADDR => self.sp as u8,
            SPH_ADDR => (self.sp >> 8) as u8,
            SREG_ADDR => self.sreg.to_u8(),
            IO_START..=0xFF => self
                .periph
                .read(addr)
                .unwrap_or_else(|| self.io[(addr - IO_START) as usize]),
            SRAM_START..=RAMEND => self.sram[(addr - SRAM_START) as usize],
            _ => 0, // outside the modelled data space
        }
    }
    /// Write a data-space byte (registers / I/O / SRAM).
    ///
    /// Addresses claimed by a peripheral route to [`Peripherals::write`]; the
    /// rest of the I/O window is plain storage.
    pub fn write_data(&mut self, addr: u16, val: u8) {
        match addr {
            0x00..=0x1F => self.regs[addr as usize] = val,
            SPL_ADDR => self.sp = (self.sp & 0xFF00) | val as u16,
            SPH_ADDR => self.sp = (self.sp & 0x00FF) | (val as u16) << 8,
            SREG_ADDR => self.sreg = Sreg::from_u8(val),
            IO_START..=0xFF => {
                if !self.periph.write(addr, val) {
                    self.io[(addr - IO_START) as usize] = val;
                }
            }
            SRAM_START..=RAMEND => self.sram[(addr - SRAM_START) as usize] = val,
            _ => {}
        }
    }

    // --- Peripheral access (GPIO / timers / ADC / USART) ---

    /// Shared read access to the memory-mapped peripherals.
    pub fn peripherals(&self) -> &Peripherals {
        &self.periph
    }
    /// Mutable access to the memory-mapped peripherals (set inputs, read TX…).
    pub fn peripherals_mut(&mut self) -> &mut Peripherals {
        &mut self.periph
    }
    /// The resolved logic level on `port` pin `bit`.
    pub fn pin(&self, port: Port, bit: u8) -> bool {
        self.periph.pin(port, bit)
    }

    // --- Interrupts ---

    /// Mark interrupt `vector` pending. It is dispatched before a later
    /// instruction once the global interrupt enable (`I`) is set.
    pub fn request_interrupt(&mut self, vector: u8) {
        debug_assert!(vector < 32, "vector index out of range");
        self.pending |= 1 << vector;
    }

    /// Take the lowest-numbered pending vector, if any (highest priority first).
    fn take_pending(&mut self) -> Option<u8> {
        if self.pending == 0 {
            return None;
        }
        let v = self.pending.trailing_zeros() as u8;
        self.pending &= !(1 << v);
        Some(v)
    }

    /// Dispatch an interrupt: push the return address, clear `I`, and jump to the
    /// vector. Costs the usual 5 cycles.
    fn service_interrupt(&mut self, vector: u8) {
        self.push16(self.pc);
        self.sreg.i = false;
        self.pc = vector as u16 * VECTOR_SIZE_WORDS;
        self.cycles += 5;
    }

    // --- Stack ---

    fn push8(&mut self, v: u8) {
        self.write_data(self.sp, v);
        self.sp = self.sp.wrapping_sub(1);
    }
    fn pop8(&mut self) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        self.read_data(self.sp)
    }
    /// Push a 16-bit value low byte first, matching `RET`/`RETI`'s pop order.
    fn push16(&mut self, v: u16) {
        self.push8(v as u8);
        self.push8((v >> 8) as u8);
    }
    fn pop16(&mut self) -> u16 {
        let hi = self.pop8() as u16;
        let lo = self.pop8() as u16;
        hi << 8 | lo
    }

    /// Read the register pair `Rd+1:Rd` as a little-endian 16-bit word (used by
    /// `ADIW`/`SBIW`).
    fn word_reg(&self, d: u8) -> u16 {
        self.regs[d as usize] as u16 | (self.regs[d as usize + 1] as u16) << 8
    }
    /// Write a 16-bit word into the register pair `Rd+1:Rd`.
    fn set_word_reg(&mut self, d: u8, v: u16) {
        self.regs[d as usize] = v as u8;
        self.regs[d as usize + 1] = (v >> 8) as u8;
    }

    // --- Pointer registers (X/Y/Z) ---

    fn ptr_get(&self, p: Ptr) -> u16 {
        let lo = p.lo();
        self.regs[lo] as u16 | (self.regs[lo + 1] as u16) << 8
    }
    fn ptr_set(&mut self, p: Ptr, v: u16) {
        let lo = p.lo();
        self.regs[lo] = v as u8;
        self.regs[lo + 1] = (v >> 8) as u8;
    }
    /// Resolve the effective address of an indirect access, applying the
    /// pre-decrement / post-increment side effect on the pointer register.
    fn ptr_addr(&mut self, p: Ptr, mode: PtrMode) -> u16 {
        match mode {
            PtrMode::None => self.ptr_get(p),
            PtrMode::PostInc => {
                let a = self.ptr_get(p);
                self.ptr_set(p, a.wrapping_add(1));
                a
            }
            PtrMode::PreDec => {
                let a = self.ptr_get(p).wrapping_sub(1);
                self.ptr_set(p, a);
                a
            }
        }
    }

    // --- Instruction fetch & length ---

    fn flash_at(&self, pc: u16) -> u16 {
        self.flash.get(pc as usize).copied().unwrap_or(0)
    }

    /// The word length of the instruction at `pc` (for skip targets).
    fn instr_len_at(&self, pc: u16) -> u16 {
        decode(self.flash_at(pc), self.flash_at(pc.wrapping_add(1))).1 as u16
    }

    /// Skip the instruction at the current `pc` (used by `CPSE`/`SBIx`/`SBRx`),
    /// returning the extra cycles the skip costs.
    fn skip_next(&mut self) -> u64 {
        let len = self.instr_len_at(self.pc);
        self.pc = self.pc.wrapping_add(len);
        len as u64 // 1 extra cycle per word skipped
    }

    // --- Flag-setting arithmetic ---

    /// `a + b (+ carry)` with ADD/ADC flag semantics; returns the result.
    //
    // The carry/half-carry/overflow expressions are written in the three-term
    // form the ATmega datasheet gives them in, so `clippy::nonminimal_bool`'s
    // "simplifications" would only obscure the correspondence.
    #[allow(clippy::nonminimal_bool)]
    fn add_flags(&mut self, a: u8, b: u8, carry: bool) -> u8 {
        let r = a.wrapping_add(b).wrapping_add(carry as u8);
        let (a7, b7, r7) = (a & 0x80 != 0, b & 0x80 != 0, r & 0x80 != 0);
        let (a3, b3, r3) = (a & 0x08 != 0, b & 0x08 != 0, r & 0x08 != 0);
        self.sreg.h = (a3 && b3) || (b3 && !r3) || (!r3 && a3);
        self.sreg.c = (a7 && b7) || (b7 && !r7) || (!r7 && a7);
        self.sreg.v = (a7 && b7 && !r7) || (!a7 && !b7 && r7);
        self.sreg.n = r7;
        self.sreg.z = r == 0;
        self.sreg.s = self.sreg.n ^ self.sreg.v;
        r
    }

    /// `a - b (- carry)` with SUB/SBC flag semantics; returns the result.
    /// `keep_z` implements the carry-chain `Z` (SBC/SBCI/CPC keep a previously
    /// cleared `Z`), used for multi-byte arithmetic.
    #[allow(clippy::nonminimal_bool)] // datasheet three-term flag forms; see add_flags
    fn sub_flags(&mut self, a: u8, b: u8, carry: bool, keep_z: bool) -> u8 {
        let r = a.wrapping_sub(b).wrapping_sub(carry as u8);
        let (a7, b7, r7) = (a & 0x80 != 0, b & 0x80 != 0, r & 0x80 != 0);
        let (a3, b3, r3) = (a & 0x08 != 0, b & 0x08 != 0, r & 0x08 != 0);
        self.sreg.h = (!a3 && b3) || (b3 && r3) || (r3 && !a3);
        self.sreg.c = (!a7 && b7) || (b7 && r7) || (r7 && !a7);
        self.sreg.v = (a7 && !b7 && !r7) || (!a7 && b7 && r7);
        self.sreg.n = r7;
        self.sreg.z = if keep_z {
            r == 0 && self.sreg.z
        } else {
            r == 0
        };
        self.sreg.s = self.sreg.n ^ self.sreg.v;
        r
    }

    /// Set the common `N`/`Z`/`V`(=0)/`S` flags for the logical ops.
    fn logic_flags(&mut self, r: u8) {
        self.sreg.v = false;
        self.sreg.n = r & 0x80 != 0;
        self.sreg.z = r == 0;
        self.sreg.s = self.sreg.n ^ self.sreg.v;
    }

    // --- Step / execute ---

    /// Execute one decoded instruction, returning the cycles it consumed. `pc`
    /// has already been advanced past the instruction's own words.
    fn execute(&mut self, instr: Instr) -> u64 {
        match instr {
            Instr::Nop => 1,

            // Two-register ALU.
            Instr::Add(d, r) => {
                self.regs[d as usize] =
                    self.add_flags(self.regs[d as usize], self.regs[r as usize], false);
                1
            }
            Instr::Adc(d, r) => {
                let c = self.sreg.c;
                self.regs[d as usize] =
                    self.add_flags(self.regs[d as usize], self.regs[r as usize], c);
                1
            }
            Instr::Sub(d, r) => {
                self.regs[d as usize] =
                    self.sub_flags(self.regs[d as usize], self.regs[r as usize], false, false);
                1
            }
            Instr::Sbc(d, r) => {
                let c = self.sreg.c;
                self.regs[d as usize] =
                    self.sub_flags(self.regs[d as usize], self.regs[r as usize], c, true);
                1
            }
            Instr::And(d, r) => {
                let v = self.regs[d as usize] & self.regs[r as usize];
                self.logic_flags(v);
                self.regs[d as usize] = v;
                1
            }
            Instr::Or(d, r) => {
                let v = self.regs[d as usize] | self.regs[r as usize];
                self.logic_flags(v);
                self.regs[d as usize] = v;
                1
            }
            Instr::Eor(d, r) => {
                let v = self.regs[d as usize] ^ self.regs[r as usize];
                self.logic_flags(v);
                self.regs[d as usize] = v;
                1
            }
            Instr::Cp(d, r) => {
                self.sub_flags(self.regs[d as usize], self.regs[r as usize], false, false);
                1
            }
            Instr::Cpc(d, r) => {
                let c = self.sreg.c;
                self.sub_flags(self.regs[d as usize], self.regs[r as usize], c, true);
                1
            }
            Instr::Cpse(d, r) => {
                if self.regs[d as usize] == self.regs[r as usize] {
                    1 + self.skip_next()
                } else {
                    1
                }
            }
            Instr::Mov(d, r) => {
                self.regs[d as usize] = self.regs[r as usize];
                1
            }
            Instr::Movw(d, r) => {
                self.regs[d as usize] = self.regs[r as usize];
                self.regs[d as usize + 1] = self.regs[r as usize + 1];
                1
            }

            // Register / immediate.
            Instr::Subi(d, k) => {
                self.regs[d as usize] = self.sub_flags(self.regs[d as usize], k, false, false);
                1
            }
            Instr::Sbci(d, k) => {
                let c = self.sreg.c;
                self.regs[d as usize] = self.sub_flags(self.regs[d as usize], k, c, true);
                1
            }
            Instr::Andi(d, k) => {
                let v = self.regs[d as usize] & k;
                self.logic_flags(v);
                self.regs[d as usize] = v;
                1
            }
            Instr::Ori(d, k) => {
                let v = self.regs[d as usize] | k;
                self.logic_flags(v);
                self.regs[d as usize] = v;
                1
            }
            Instr::Cpi(d, k) => {
                self.sub_flags(self.regs[d as usize], k, false, false);
                1
            }
            Instr::Ldi(d, k) => {
                self.regs[d as usize] = k;
                1
            }
            Instr::Adiw(d, k) => {
                let val = self.word_reg(d);
                let res = val.wrapping_add(k as u16);
                let (rdh7, r15) = (val & 0x8000 != 0, res & 0x8000 != 0);
                self.sreg.c = !r15 && rdh7;
                self.sreg.v = !rdh7 && r15;
                self.sreg.n = r15;
                self.sreg.z = res == 0;
                self.sreg.s = self.sreg.n ^ self.sreg.v;
                self.set_word_reg(d, res);
                2
            }
            Instr::Sbiw(d, k) => {
                let val = self.word_reg(d);
                let res = val.wrapping_sub(k as u16);
                let (rdh7, r15) = (val & 0x8000 != 0, res & 0x8000 != 0);
                self.sreg.c = r15 && !rdh7;
                self.sreg.v = rdh7 && !r15;
                self.sreg.n = r15;
                self.sreg.z = res == 0;
                self.sreg.s = self.sreg.n ^ self.sreg.v;
                self.set_word_reg(d, res);
                2
            }

            // Single register.
            Instr::Com(d) => {
                let v = !self.regs[d as usize];
                self.sreg.c = true;
                self.sreg.v = false;
                self.sreg.n = v & 0x80 != 0;
                self.sreg.z = v == 0;
                self.sreg.s = self.sreg.n ^ self.sreg.v;
                self.regs[d as usize] = v;
                1
            }
            Instr::Neg(d) => {
                let a = self.regs[d as usize];
                let v = 0u8.wrapping_sub(a);
                self.sreg.h = (v & 0x08 != 0) || (a & 0x08 != 0);
                self.sreg.c = v != 0;
                self.sreg.v = v == 0x80;
                self.sreg.n = v & 0x80 != 0;
                self.sreg.z = v == 0;
                self.sreg.s = self.sreg.n ^ self.sreg.v;
                self.regs[d as usize] = v;
                1
            }
            Instr::Inc(d) => {
                let v = self.regs[d as usize].wrapping_add(1);
                self.sreg.v = v == 0x80;
                self.sreg.n = v & 0x80 != 0;
                self.sreg.z = v == 0;
                self.sreg.s = self.sreg.n ^ self.sreg.v;
                self.regs[d as usize] = v;
                1
            }
            Instr::Dec(d) => {
                let v = self.regs[d as usize].wrapping_sub(1);
                self.sreg.v = v == 0x7F;
                self.sreg.n = v & 0x80 != 0;
                self.sreg.z = v == 0;
                self.sreg.s = self.sreg.n ^ self.sreg.v;
                self.regs[d as usize] = v;
                1
            }
            Instr::Swap(d) => {
                self.regs[d as usize] = self.regs[d as usize].rotate_left(4);
                1
            }
            Instr::Lsr(d) => {
                let a = self.regs[d as usize];
                let v = a >> 1;
                self.sreg.c = a & 1 != 0;
                self.sreg.n = false;
                self.sreg.z = v == 0;
                self.sreg.v = self.sreg.n ^ self.sreg.c;
                self.sreg.s = self.sreg.n ^ self.sreg.v;
                self.regs[d as usize] = v;
                1
            }
            Instr::Asr(d) => {
                let a = self.regs[d as usize];
                let v = a >> 1 | (a & 0x80);
                self.sreg.c = a & 1 != 0;
                self.sreg.n = v & 0x80 != 0;
                self.sreg.z = v == 0;
                self.sreg.v = self.sreg.n ^ self.sreg.c;
                self.sreg.s = self.sreg.n ^ self.sreg.v;
                self.regs[d as usize] = v;
                1
            }
            Instr::Ror(d) => {
                let a = self.regs[d as usize];
                let v = a >> 1 | (self.sreg.c as u8) << 7;
                self.sreg.c = a & 1 != 0;
                self.sreg.n = v & 0x80 != 0;
                self.sreg.z = v == 0;
                self.sreg.v = self.sreg.n ^ self.sreg.c;
                self.sreg.s = self.sreg.n ^ self.sreg.v;
                self.regs[d as usize] = v;
                1
            }

            // Data transfer.
            Instr::Lds(d, addr) => {
                self.regs[d as usize] = self.read_data(addr);
                2
            }
            Instr::Sts(addr, r) => {
                self.write_data(addr, self.regs[r as usize]);
                2
            }
            Instr::Ld(d, p, mode) => {
                let a = self.ptr_addr(p, mode);
                self.regs[d as usize] = self.read_data(a);
                2
            }
            Instr::St(p, mode, r) => {
                let a = self.ptr_addr(p, mode);
                self.write_data(a, self.regs[r as usize]);
                2
            }
            Instr::Ldd(d, p, q) => {
                let a = self.ptr_get(p).wrapping_add(q as u16);
                self.regs[d as usize] = self.read_data(a);
                2
            }
            Instr::Std(p, q, r) => {
                let a = self.ptr_get(p).wrapping_add(q as u16);
                self.write_data(a, self.regs[r as usize]);
                2
            }
            Instr::In(d, a) => {
                self.regs[d as usize] = self.read_data(a as u16 + IO_START);
                1
            }
            Instr::Out(a, r) => {
                self.write_data(a as u16 + IO_START, self.regs[r as usize]);
                1
            }
            Instr::Push(r) => {
                self.push8(self.regs[r as usize]);
                2
            }
            Instr::Pop(d) => {
                self.regs[d as usize] = self.pop8();
                2
            }

            // Bit manipulation.
            Instr::Sbi(a, b) => {
                let addr = a as u16 + IO_START;
                let v = self.read_data(addr) | 1 << b;
                self.write_data(addr, v);
                2
            }
            Instr::Cbi(a, b) => {
                let addr = a as u16 + IO_START;
                let v = self.read_data(addr) & !(1 << b);
                self.write_data(addr, v);
                2
            }
            Instr::Sbic(a, b) => {
                if self.read_data(a as u16 + IO_START) & 1 << b == 0 {
                    1 + self.skip_next()
                } else {
                    1
                }
            }
            Instr::Sbis(a, b) => {
                if self.read_data(a as u16 + IO_START) & 1 << b != 0 {
                    1 + self.skip_next()
                } else {
                    1
                }
            }
            Instr::Sbrc(r, b) => {
                if self.regs[r as usize] & 1 << b == 0 {
                    1 + self.skip_next()
                } else {
                    1
                }
            }
            Instr::Sbrs(r, b) => {
                if self.regs[r as usize] & 1 << b != 0 {
                    1 + self.skip_next()
                } else {
                    1
                }
            }
            Instr::Bset(s) => {
                self.sreg.set(s, true);
                1
            }
            Instr::Bclr(s) => {
                self.sreg.set(s, false);
                1
            }
            Instr::Bld(d, b) => {
                if self.sreg.t {
                    self.regs[d as usize] |= 1 << b;
                } else {
                    self.regs[d as usize] &= !(1 << b);
                }
                1
            }
            Instr::Bst(d, b) => {
                self.sreg.t = self.regs[d as usize] & 1 << b != 0;
                1
            }

            // Control flow.
            Instr::Rjmp(k) => {
                self.pc = self.pc.wrapping_add(k as u16);
                2
            }
            Instr::Rcall(k) => {
                self.push16(self.pc);
                self.pc = self.pc.wrapping_add(k as u16);
                3
            }
            Instr::Ijmp => {
                self.pc = self.ptr_get(Ptr::Z);
                2
            }
            Instr::Icall => {
                self.push16(self.pc);
                self.pc = self.ptr_get(Ptr::Z);
                3
            }
            Instr::Jmp(addr) => {
                self.pc = addr as u16;
                3
            }
            Instr::Call(addr) => {
                self.push16(self.pc);
                self.pc = addr as u16;
                4
            }
            Instr::Ret => {
                self.pc = self.pop16();
                4
            }
            Instr::Reti => {
                self.pc = self.pop16();
                self.sreg.i = true;
                4
            }
            Instr::Brbs(s, k) => {
                if self.sreg.get(s) {
                    self.pc = self.pc.wrapping_add(k as u16);
                    2
                } else {
                    1
                }
            }
            Instr::Brbc(s, k) => {
                if !self.sreg.get(s) {
                    self.pc = self.pc.wrapping_add(k as u16);
                    2
                } else {
                    1
                }
            }

            Instr::Unknown(word) => {
                debug_assert!(
                    false,
                    "unimplemented AVR opcode {word:#06x} at pc {:#06x}",
                    self.pc
                );
                1
            }
        }
    }
}

impl McuCore for Avr {
    fn step(&mut self) {
        // Service a pending interrupt before fetching the next instruction.
        if self.sreg.i {
            if let Some(v) = self.take_pending() {
                self.service_interrupt(v);
                self.periph.tick(5); // the 5 dispatch cycles drive peripherals too
                return;
            }
        }
        let w0 = self.flash_at(self.pc);
        let w1 = self.flash_at(self.pc.wrapping_add(1));
        let (instr, len) = decode(w0, w1);
        self.pc = self.pc.wrapping_add(len as u16);
        let cycles = self.execute(instr);
        self.cycles += cycles;
        self.periph.tick(cycles);
    }

    fn step_to(&mut self, t: f64) {
        // Instruction-accurate: run whole instructions until elapsed time
        // reaches `t`. Every instruction costs at least one cycle, so time always
        // advances and this terminates even inside a tight loop.
        while (self.cycles as f64) / self.f_cpu < t {
            self.step();
        }
    }

    fn load_firmware(&mut self, image: &[u8]) -> Result<(), FirmwareError> {
        if image.is_empty() {
            return Err(FirmwareError::BadImage);
        }
        // ELF images start with the magic 0x7F 'E' 'L' 'F' — recognised but not
        // yet supported.
        if image.starts_with(&[0x7F, b'E', b'L', b'F']) {
            return Err(FirmwareError::UnsupportedFormat);
        }
        self.flash.iter_mut().for_each(|w| *w = 0);
        if image[0] == b':' {
            self.load_ihex(image)?;
        } else {
            self.load_raw(image)?;
        }
        self.reset();
        Ok(())
    }
}

impl Avr {
    /// Load a raw binary image: little-endian 16-bit words straight into flash.
    fn load_raw(&mut self, image: &[u8]) -> Result<(), FirmwareError> {
        if image.len() > FLASH_WORDS * 2 {
            return Err(FirmwareError::BadImage);
        }
        for (i, pair) in image.chunks(2).enumerate() {
            let lo = pair[0] as u16;
            let hi = *pair.get(1).unwrap_or(&0) as u16;
            self.flash[i] = lo | hi << 8;
        }
        Ok(())
    }

    /// Write one flash byte at a byte address (used by the Intel HEX loader).
    fn set_flash_byte(&mut self, byte_addr: u32, val: u8) -> Result<(), FirmwareError> {
        let word = (byte_addr / 2) as usize;
        if word >= FLASH_WORDS {
            return Err(FirmwareError::BadImage);
        }
        if byte_addr & 1 == 0 {
            self.flash[word] = (self.flash[word] & 0xFF00) | val as u16;
        } else {
            self.flash[word] = (self.flash[word] & 0x00FF) | (val as u16) << 8;
        }
        Ok(())
    }

    /// Parse an Intel HEX image (ASCII). Supports record types 00 (data), 01
    /// (EOF), and 04 (extended linear address); other types are rejected.
    fn load_ihex(&mut self, image: &[u8]) -> Result<(), FirmwareError> {
        let text = core::str::from_utf8(image).map_err(|_| FirmwareError::BadImage)?;
        let mut base: u32 = 0;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let bytes = parse_ihex_record(line)?;
            // bytes = [len, addr_hi, addr_lo, type, data..., checksum]
            let len = bytes[0] as usize;
            if bytes.len() != len + 5 {
                return Err(FirmwareError::BadImage);
            }
            // Record checksum: the whole record (sans the ':') sums to 0 mod 256.
            if bytes.iter().fold(0u8, |a, &b| a.wrapping_add(b)) != 0 {
                return Err(FirmwareError::BadImage);
            }
            let addr = (bytes[1] as u32) << 8 | bytes[2] as u32;
            match bytes[3] {
                0x00 => {
                    for (i, &b) in bytes[4..4 + len].iter().enumerate() {
                        self.set_flash_byte(base + addr + i as u32, b)?;
                    }
                }
                0x01 => break, // end of file
                0x04 => {
                    // Extended linear address: upper 16 bits of the base.
                    if len != 2 {
                        return Err(FirmwareError::BadImage);
                    }
                    base = ((bytes[4] as u32) << 8 | bytes[5] as u32) << 16;
                }
                _ => return Err(FirmwareError::BadImage),
            }
        }
        Ok(())
    }
}

/// Parse the hex digits of one Intel HEX record (after the leading ':') into
/// bytes. Returns [`FirmwareError::BadImage`] on any non-hex or odd-length input.
fn parse_ihex_record(line: &str) -> Result<Vec<u8>, FirmwareError> {
    let body = line.strip_prefix(':').ok_or(FirmwareError::BadImage)?;
    if body.len() % 2 != 0 || body.is_empty() {
        return Err(FirmwareError::BadImage);
    }
    let mut out = Vec::with_capacity(body.len() / 2);
    let b = body.as_bytes();
    for pair in b.chunks(2) {
        let hi = hex_digit(pair[0])?;
        let lo = hex_digit(pair[1])?;
        out.push(hi << 4 | lo);
    }
    Ok(out)
}

fn hex_digit(c: u8) -> Result<u8, FirmwareError> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(FirmwareError::BadImage),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a core, load a hand-assembled program, and return it reset.
    fn with_program(words: &[u16]) -> Avr {
        let mut avr = Avr::new();
        avr.load_program(words);
        avr
    }

    /// Run `n` instruction steps.
    fn run(avr: &mut Avr, n: usize) {
        for _ in 0..n {
            avr.step();
        }
    }

    #[test]
    fn ldi_and_add_set_registers_and_flags() {
        // LDI r16, 0x0A ; LDI r17, 0x16 ; ADD r16, r17  → r16 = 0x20.
        let mut avr = with_program(&[0xE00A, 0xE116, 0x0F01]);
        run(&mut avr, 3);
        assert_eq!(avr.reg(16), 0x20);
        assert!(!avr.sreg().z && !avr.sreg().c);
    }

    #[test]
    fn add_sets_carry_and_zero_on_wrap() {
        // LDI r16, 0xFF ; LDI r17, 0x01 ; ADD r16, r17 → 0x00, carry + zero.
        let mut avr = with_program(&[0xEF0F, 0xE011, 0x0F01]);
        run(&mut avr, 3);
        assert_eq!(avr.reg(16), 0x00);
        assert!(avr.sreg().z, "zero flag");
        assert!(avr.sreg().c, "carry out of bit 7");
        assert!(avr.sreg().h, "half-carry out of bit 3");
    }

    #[test]
    fn sub_sets_borrow() {
        // LDI r16, 0x10 ; LDI r17, 0x20 ; SUB r16, r17 → 0xF0, borrow (carry) set.
        let mut avr = with_program(&[0xE100, 0xE210, 0x1B01]);
        run(&mut avr, 3);
        assert_eq!(avr.reg(16), 0xF0);
        assert!(avr.sreg().c, "borrow sets carry");
        assert!(avr.sreg().n, "result is negative");
        assert!(!avr.sreg().z);
    }

    #[test]
    fn eor_clears_a_register() {
        // LDI r16, 0x5A ; EOR r16, r16 → 0x00, Z set.
        let mut avr = with_program(&[0xE50A, 0x2700]);
        run(&mut avr, 2);
        assert_eq!(avr.reg(16), 0);
        assert!(avr.sreg().z);
    }

    #[test]
    fn com_and_neg() {
        // LDI r16, 0x01 ; COM r16 → 0xFE, carry set.
        let mut avr = with_program(&[0xE001, 0x9500]);
        run(&mut avr, 2);
        assert_eq!(avr.reg(16), 0xFE);
        assert!(avr.sreg().c);
        // LDI r17, 0x01 ; NEG r17 → 0xFF.
        let mut avr = with_program(&[0xE011, 0x9511]);
        run(&mut avr, 2);
        assert_eq!(avr.reg(17), 0xFF);
    }

    #[test]
    fn movw_moves_a_register_pair() {
        // LDI r16,0x34 ; LDI r17,0x12 ; MOVW r30, r16 → Z = 0x1234.
        let mut avr = with_program(&[0xE304, 0xE112, 0x01F8]);
        run(&mut avr, 3);
        assert_eq!(avr.reg(30), 0x34);
        assert_eq!(avr.reg(31), 0x12);
    }

    #[test]
    fn sram_round_trip_via_sts_and_lds() {
        // LDI r16,0xAB ; STS 0x0200, r16 ; LDS r17, 0x0200. STS/LDS are 2-word
        // instructions, so three steps execute all three instructions.
        let mut avr = with_program(&[0xEA0B, 0x9300, 0x0200, 0x9110, 0x0200]);
        run(&mut avr, 3);
        assert_eq!(avr.read_data(0x0200), 0xAB);
        assert_eq!(avr.reg(17), 0xAB);
    }

    #[test]
    fn indirect_store_and_load_with_pointer_update() {
        // Set X = 0x0200, store 0x7E with ST X+, then LD back from 0x0200.
        // LDI r26,0x00 ; LDI r27,0x02 ; LDI r16,0x7E ; ST X+, r16 ; LD r17, -X.
        let mut avr = with_program(&[0xE0A0, 0xE0B2, 0xE70E, 0x930D, 0x911E]);
        run(&mut avr, 5);
        assert_eq!(avr.read_data(0x0200), 0x7E);
        assert_eq!(avr.reg(17), 0x7E, "-X points back at 0x0200");
    }

    #[test]
    fn in_out_round_trip_through_io_space() {
        // LDI r16, 0x3C ; OUT 0x04, r16 ; IN r17, 0x04. I/O address 0x04 maps to
        // data address 0x24.
        let mut avr = with_program(&[0xE30C, 0xB904, 0xB114]);
        run(&mut avr, 3);
        assert_eq!(avr.read_data(0x24), 0x3C); // I/O 0x04 → data 0x24
        assert_eq!(avr.reg(17), 0x3C);
    }

    #[test]
    fn push_and_pop_use_the_stack() {
        // LDI r16,0x99 ; PUSH r16 ; POP r17.
        let mut avr = with_program(&[0xE909, 0x930F, 0x911F]);
        let sp0 = avr.sp();
        avr.step(); // LDI
        avr.step(); // PUSH
        assert_eq!(avr.sp(), sp0 - 1);
        avr.step(); // POP
        assert_eq!(avr.sp(), sp0);
        assert_eq!(avr.reg(17), 0x99);
    }

    #[test]
    fn countdown_loop_runs_to_completion() {
        // A loop that decrements r16 from 10 to 0, incrementing r17 each pass:
        //   LDI r16, 10
        //   LDI r17, 0
        // loop:
        //   INC r17
        //   DEC r16
        //   BRNE loop        ; branch while r16 != 0 (Z clear)
        //   (fall through)
        // Addresses (word): 0:LDI 1:LDI 2:INC 3:DEC 4:BRNE(-3 → back to 2)
        // BRNE = BRBC 1, offset -3: k7 = -3 → encoding 0xF7E9.
        let mut avr = with_program(&[0xE00A, 0xE010, 0x9513, 0x950A, 0xF7E9]);
        // Run enough steps: 2 setup + 10*(INC+DEC+BRNE) but last BRNE not taken.
        run(&mut avr, 2 + 10 * 3);
        assert_eq!(avr.reg(16), 0, "counter reached zero");
        assert_eq!(avr.reg(17), 10, "loop body ran ten times");
        assert!(avr.sreg().z, "DEC to zero set Z, ending the loop");
    }

    #[test]
    fn sums_an_array_from_sram() {
        // Place [1,2,3,4,5] at 0x0200, then sum via LD Z+ into r18.
        //   LDI r30,0x00 ; LDI r31,0x02       ; Z = 0x0200
        //   LDI r18,0x00                      ; sum = 0
        //   LDI r19,0x05                      ; count = 5
        // loop:
        //   LD  r0, Z+                        ; load element
        //   ADD r18, r0                       ; sum += element
        //   DEC r19
        //   BRNE loop
        // Words:
        //  0 LDI r30,0 = 0xE0E0
        //  1 LDI r31,2 = 0xE0F2
        //  2 LDI r18,0 = 0xE020
        //  3 LDI r19,5 = 0xE035
        //  4 LD r0,Z+  = 0x9001
        //  5 ADD r18,r0= 0x0D20
        //  6 DEC r19   = 0x953A
        //  7 BRNE -4 (back to 4) = BRBC 1, k=-4 → 0xF7E1
        let mut avr = with_program(&[
            0xE0E0, 0xE0F2, 0xE020, 0xE035, 0x9001, 0x0D20, 0x953A, 0xF7E1,
        ]);
        for (i, v) in [1u8, 2, 3, 4, 5].iter().enumerate() {
            avr.write_data(0x0200 + i as u16, *v);
        }
        run(&mut avr, 4 + 5 * 4); // 4 setup + 5 iterations of 4 insns
        assert_eq!(avr.reg(18), 15, "1+2+3+4+5");
        assert_eq!(avr.ptr_get(Ptr::Z), 0x0205, "Z advanced past the array");
    }

    #[test]
    fn adiw_and_sbiw_update_a_register_pair() {
        // LDI r24,0xFF ; LDI r25,0x00 ; ADIW r24,1 → 0x0100.
        let mut avr = with_program(&[0xEF8F, 0xE090, 0x9601]);
        run(&mut avr, 3);
        assert_eq!(avr.word_reg(24), 0x0100, "0x00FF + 1");
        assert!(!avr.sreg().z);
        // LDI r24,0x01 ; LDI r25,0x00 ; SBIW r24,1 → 0x0000, Z set.
        let mut avr = with_program(&[0xE081, 0xE090, 0x9701]);
        run(&mut avr, 3);
        assert_eq!(avr.word_reg(24), 0, "1 - 1");
        assert!(avr.sreg().z, "SBIW to zero sets Z");
    }

    #[test]
    fn sbiw_delay_loop_consumes_the_expected_cycles() {
        // LDI r24,0x10 ; LDI r25,0x00 ; loop: SBIW r24,1 ; BRNE loop.
        // Word 2 is SBIW, word 3 is BRNE .-1 (back to 2): BRBC 1, k=-1 → 0xF7F1.
        let mut avr = with_program(&[0xE180, 0xE090, 0x9701, 0xF7F1]);
        avr.set_clock_hz(1_000_000.0);
        // 0x10 = 16 iterations: SBIW (2 cyc) + BRNE (2 taken / 1 not) each.
        run(&mut avr, 2 + 16 * 2);
        assert_eq!(avr.word_reg(24), 0, "loop counted down to zero");
    }

    #[test]
    fn firmware_toggles_a_gpio_pin() {
        use super::Port;
        // SBI DDRB,5 (PB5 output) ; LDI r16,0x20 ; OUT PINB,r16 (toggle PB5).
        // DDRB I/O addr = 0x04, PINB I/O addr = 0x03.
        let mut avr = with_program(&[0x9A25, 0xE200, 0xB903]);
        avr.step(); // SBI DDRB,5
        assert!(!avr.pin(Port::B, 5), "starts low");
        avr.step(); // LDI r16,0x20
        avr.step(); // OUT PINB,r16 → toggles PB5 high
        assert!(avr.pin(Port::B, 5), "PB5 toggled high");
    }

    #[test]
    fn sei_cli_toggle_the_interrupt_flag() {
        // SEI ; CLI.
        let mut avr = with_program(&[0x9478, 0x94F8]);
        avr.step();
        assert!(avr.sreg().i, "SEI sets I");
        avr.step();
        assert!(!avr.sreg().i, "CLI clears I");
    }

    #[test]
    fn interrupt_dispatches_to_its_vector_and_reti_returns() {
        // Program: at reset we SEI then spin on RJMP to self.
        //   0: SEI              (0x9478)
        //   1: RJMP .-1 (self)  (0xCFFF → offset -1 keeps pc at 1)
        // Put a RETI at the vector for interrupt #2 (word addr 2*2 = 4).
        //   4: RETI             (0x9518)
        let mut avr = with_program(&[0x9478, 0xCFFF, 0x0000, 0x0000, 0x9518]);
        avr.step(); // SEI → I set, pc = 1
        avr.step(); // RJMP self → pc stays 1
        assert_eq!(avr.pc(), 1);

        let sp_before = avr.sp();
        avr.request_interrupt(2);
        avr.step(); // dispatch: push pc(=1), clear I, jump to vector 2 → pc = 4
        assert_eq!(avr.pc(), 4, "vectored to interrupt 2 (word addr 4)");
        assert!(!avr.sreg().i, "I cleared on dispatch");
        assert_eq!(avr.sp(), sp_before - 2, "return address pushed (2 bytes)");

        avr.step(); // RETI → pop pc(=1), set I
        assert_eq!(avr.pc(), 1, "returned to the interrupted instruction");
        assert!(avr.sreg().i, "RETI re-enabled interrupts");
        assert_eq!(avr.sp(), sp_before, "stack balanced");
    }

    #[test]
    fn pending_interrupt_waits_for_global_enable() {
        // Without SEI the interrupt must not fire.
        //   0: NOP ; 1: RJMP self.
        let mut avr = with_program(&[0x0000, 0xCFFF]);
        avr.request_interrupt(5);
        avr.step(); // NOP
        avr.step(); // RJMP self, still pc=1, no dispatch (I clear)
        assert_eq!(avr.pc(), 1);
        assert!(!avr.sreg().i);
    }

    #[test]
    fn step_to_advances_by_the_clock() {
        // A spin loop; step_to should run ~f_cpu * t cycles' worth of insns.
        let mut avr = with_program(&[0xCFFF]); // RJMP self, 2 cycles each
        avr.set_clock_hz(1_000_000.0); // 1 MHz → 1 cycle = 1 µs
        avr.step_to(100e-6); // 100 µs → ~100 cycles → ~50 RJMPs
        assert!(
            avr.cycles() >= 100,
            "ran at least 100 cycles, got {}",
            avr.cycles()
        );
        assert!(
            avr.cycles() < 110,
            "did not overshoot wildly: {}",
            avr.cycles()
        );
    }

    #[test]
    fn load_firmware_raw_and_ihex_agree() {
        // Raw image for: LDI r16, 0x2A (0xE20A), little-endian bytes 0x0A 0xE2.
        let mut raw = Avr::new();
        raw.load_firmware(&[0x0A, 0xE2]).unwrap();
        raw.step();
        assert_eq!(raw.reg(16), 0x2A);

        // The same program as Intel HEX: one data record at addr 0 + EOF.
        // bytes: len=02 addr=0000 type=00 data=0A E2 → checksum.
        // sum = 02+00+00+00+0A+E2 = 0xEE → checksum = 0x12.
        let mut hex = Avr::new();
        hex.load_firmware(b":020000000AE212\n:00000001FF\n")
            .unwrap();
        hex.step();
        assert_eq!(hex.reg(16), 0x2A, "HEX and raw loaders agree");
    }

    #[test]
    fn load_firmware_rejects_elf_and_oversize() {
        let mut avr = Avr::new();
        assert_eq!(
            avr.load_firmware(&[0x7F, b'E', b'L', b'F', 0, 0]),
            Err(FirmwareError::UnsupportedFormat)
        );
        assert_eq!(avr.load_firmware(&[]), Err(FirmwareError::BadImage));
    }
}
