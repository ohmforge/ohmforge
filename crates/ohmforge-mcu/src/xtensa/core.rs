//! The Xtensa (LX6) core: the flat register file, a flat memory model with the
//! ESP32 GPIO block decoded as MMIO, and the fetch/decode/execute step loop.
//!
//! # Register file
//!
//! Sixteen 32-bit address registers `a0..a15`. This core models the **CALL0
//! (windowless) ABI**: the register file is flat (no physical 64-entry window,
//! no `WindowBase`/`WindowStart` rotation), and `a0` is the link register that
//! `CALL0`/`CALLX0` write and `RET`/`RET.N`/`JX` return through. The windowed
//! call instructions are not modelled — see [`decode`](mod@super::decode).
//!
//! Unlike the ARM core, the program counter is a dedicated field, not part of the
//! register file, and it holds the address of the instruction currently
//! executing (Xtensa PC-relative operations — `L32R`, `J`, the branches, `CALL0`
//! — are defined against that address).
//!
//! # Memory model
//!
//! A single flat, little-endian, byte-addressed RAM mapped from address 0 (sized
//! by the [`variant`](super::variant)), plus the ESP32 GPIO block
//! ([`Esp32Gpio`]) decoded at `0x3FF4_4000`. Programs and their `L32R` literal
//! pools load at address 0; addresses outside the modelled RAM (other than the
//! GPIO window) read as zero and drop writes.
//!
//! # Interrupts
//!
//! The ESP32 interrupt matrix is **not** modelled in this subset — the core
//! free-runs the instruction stream. GPIO crosses to the circuit only through the
//! engine-agnostic [`PinIo`] exchange, so (like the AVR and ARM cores) `ohmforge-mcu`
//! gains no dependency on the analog engine.

use crate::{FirmwareError, McuCore, PinIo};

use super::decode::decode;
use super::isa::{Instr, Size};
use super::periph::Esp32Gpio;
use super::variant::XtensaVariant;

/// Link register (`a0`) — holds the return address for `CALL0`/`CALLX0`.
const LINK: usize = 0;

/// An instruction-accurate Xtensa (LX6) core, specialised to a part by an
/// [`XtensaVariant`].
pub struct Xtensa {
    /// Address registers `a0..a15` (`a0` is the CALL0 link register).
    regs: [u32; 16],
    /// Program counter: the address of the instruction currently executing.
    pc: u32,
    /// Flat RAM mapped from address 0.
    mem: Vec<u8>,
    /// The ESP32 GPIO peripheral (MMIO at `0x3FF4_4000`).
    gpio: Esp32Gpio,
    /// Elapsed CPU cycles (instruction-accurate).
    cycles: u64,
    /// Clock frequency in Hz (for the cycles→seconds mapping).
    f_cpu: f64,
    /// The part this core models.
    variant: XtensaVariant,
}

impl Default for Xtensa {
    fn default() -> Self {
        Self::new()
    }
}

impl Xtensa {
    /// A reset ESP32 core with empty memory and the ESP32's 240 MHz default clock.
    pub fn new() -> Self {
        Self::with_variant(XtensaVariant::esp32())
    }

    /// A reset core specialised to `variant`: the flat RAM is sized from it and
    /// the clock defaults to the part's `f_cpu`.
    pub fn with_variant(variant: XtensaVariant) -> Self {
        let mut core = Self {
            regs: [0; 16],
            pc: 0,
            mem: vec![0; variant.ram_bytes],
            gpio: Esp32Gpio::new(),
            cycles: 0,
            f_cpu: variant.f_cpu,
            variant,
        };
        core.reset();
        core
    }

    /// The part this core models.
    pub fn variant(&self) -> &XtensaVariant {
        &self.variant
    }

    /// Reset the core state (registers, PC, cycle counter, GPIO) without touching
    /// memory.
    pub fn reset(&mut self) {
        self.regs = [0; 16];
        self.pc = 0;
        self.cycles = 0;
        self.gpio = Esp32Gpio::new();
    }

    /// Set the CPU clock in Hz (affects only the cycles→seconds mapping).
    pub fn set_clock_hz(&mut self, hz: f64) {
        self.f_cpu = hz;
    }

    /// Load a program as a raw byte image into memory from address 0, then reset
    /// (`PC` → 0). A convenience for tests and hand-assembled snippets.
    ///
    /// Because Xtensa `L32R` reads a PC-relative literal pool that sits *before*
    /// the instruction, a program that uses it places its literals at the low
    /// addresses and its entry point above them; call [`set_pc`](Xtensa::set_pc)
    /// to start execution at that entry.
    pub fn load_program(&mut self, image: &[u8]) {
        self.mem.iter_mut().for_each(|b| *b = 0);
        let n = self.mem.len().min(image.len());
        self.mem[..n].copy_from_slice(&image[..n]);
        self.reset();
    }

    /// Set the program counter (used by tests and board setup to start at a
    /// program's entry point above its literal pool).
    pub fn set_pc(&mut self, pc: u32) {
        self.pc = pc;
    }

    // --- Inspection helpers ---

    /// The current program counter (the executing instruction's address).
    pub fn pc(&self) -> u32 {
        self.pc
    }
    /// Elapsed CPU cycles since reset.
    pub fn cycles(&self) -> u64 {
        self.cycles
    }
    /// Read address register `a<i>` (`0..16`).
    pub fn reg(&self, i: u8) -> u32 {
        self.regs[i as usize]
    }
    /// Write address register `a<i>` (`0..16`) — for tests / board setup.
    pub fn set_reg(&mut self, i: u8, v: u32) {
        self.regs[i as usize] = v;
    }

    /// Shared access to the GPIO peripheral.
    pub fn gpio(&self) -> &Esp32Gpio {
        &self.gpio
    }
    /// Mutable access to the GPIO peripheral.
    pub fn gpio_mut(&mut self) -> &mut Esp32Gpio {
        &mut self.gpio
    }
    /// The resolved logic level on `GPIO<n>`.
    pub fn gpio_level(&self, gpio: u8) -> bool {
        self.gpio.pin_level(gpio)
    }

    // --- Memory access (flat RAM + GPIO MMIO) ---

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

    /// Read a 32-bit word, routing the GPIO window to its peripheral.
    pub fn read_u32(&self, addr: u32) -> u32 {
        if Esp32Gpio::claims(addr) {
            return self.gpio.read(addr);
        }
        self.read_u16(addr) as u32 | (self.read_u16(addr.wrapping_add(2)) as u32) << 16
    }

    /// Write a 16-bit halfword (little-endian) to flat memory.
    pub fn write_u16(&mut self, addr: u32, v: u16) {
        self.write_u8_mem(addr, v as u8);
        self.write_u8_mem(addr.wrapping_add(1), (v >> 8) as u8);
    }

    /// Write a 32-bit word, routing the GPIO window to its peripheral.
    pub fn write_u32(&mut self, addr: u32, v: u32) {
        if Esp32Gpio::claims(addr) {
            self.gpio.write(addr, v);
            return;
        }
        self.write_u16(addr, v as u16);
        self.write_u16(addr.wrapping_add(2), (v >> 16) as u16);
    }

    /// A sized memory read used by the load instructions, sign-extending a
    /// sub-word load when `signed`.
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

    // --- Execute ---

    /// Execute one decoded instruction, returning the cycles it consumed. `PC`
    /// has already been advanced past this instruction; branch targets are formed
    /// against [`cur_instr_addr`](Xtensa::pc) captured at fetch (held in `base`).
    fn execute(&mut self, instr: Instr, base: u32) -> u64 {
        match instr {
            // RRR arithmetic / logical.
            Instr::Add { r, s, t } => {
                self.regs[r as usize] = self.regs[s as usize].wrapping_add(self.regs[t as usize]);
                1
            }
            Instr::Sub { r, s, t } => {
                self.regs[r as usize] = self.regs[s as usize].wrapping_sub(self.regs[t as usize]);
                1
            }
            Instr::And { r, s, t } => {
                self.regs[r as usize] = self.regs[s as usize] & self.regs[t as usize];
                1
            }
            Instr::Or { r, s, t } => {
                self.regs[r as usize] = self.regs[s as usize] | self.regs[t as usize];
                1
            }
            Instr::Xor { r, s, t } => {
                self.regs[r as usize] = self.regs[s as usize] ^ self.regs[t as usize];
                1
            }

            // RRI8 immediate ALU / move.
            Instr::Movi { t, imm } => {
                self.regs[t as usize] = imm as u32;
                1
            }
            Instr::Addi { t, s, imm } => {
                self.regs[t as usize] = self.regs[s as usize].wrapping_add(imm as u32);
                1
            }

            // Loads / stores.
            Instr::Load {
                t,
                s,
                off,
                size,
                signed,
            } => {
                let addr = self.regs[s as usize].wrapping_add(off);
                self.regs[t as usize] = self.load(addr, size, signed);
                2
            }
            Instr::Store { t, s, off, size } => {
                let addr = self.regs[s as usize].wrapping_add(off);
                self.store(addr, size, self.regs[t as usize]);
                2
            }
            Instr::L32r { t, imm16 } => {
                // The literal sits at or below the aligned PC: the offset is
                // always negative (`0xFFFC0000 | (imm16 << 2)`).
                let off = 0xFFFC_0000u32 | ((imm16 as u32) << 2);
                let ea = (base & !0b11).wrapping_add(off);
                self.regs[t as usize] = self.read_u32(ea);
                2
            }

            // Control flow (targets are relative to the branch's own address).
            Instr::J { offset } => {
                self.pc = base.wrapping_add(4).wrapping_add(offset as u32);
                2
            }
            Instr::Call0 { offset } => {
                self.regs[LINK] = base.wrapping_add(3);
                self.pc = (base & !0b11).wrapping_add(4).wrapping_add(offset as u32);
                2
            }
            Instr::Callx0 { s } => {
                let target = self.regs[s as usize];
                self.regs[LINK] = base.wrapping_add(3);
                self.pc = target;
                2
            }
            Instr::Ret => {
                self.pc = self.regs[LINK];
                2
            }
            Instr::Jx { s } => {
                self.pc = self.regs[s as usize];
                2
            }
            Instr::Beqz { s, offset } => self.branch_if(self.regs[s as usize] == 0, base, offset),
            Instr::Bnez { s, offset } => self.branch_if(self.regs[s as usize] != 0, base, offset),
            Instr::Bltz { s, offset } => {
                self.branch_if((self.regs[s as usize] as i32) < 0, base, offset)
            }
            Instr::Bgez { s, offset } => {
                self.branch_if((self.regs[s as usize] as i32) >= 0, base, offset)
            }

            // Code-density narrow forms.
            Instr::MovN { t, s } => {
                self.regs[t as usize] = self.regs[s as usize];
                1
            }
            Instr::Nop => 1,

            Instr::Unknown(word) => {
                debug_assert!(
                    false,
                    "unimplemented Xtensa opcode {word:#08x} at pc {:#010x}",
                    base
                );
                1
            }
        }
    }

    /// Take a branch to `base + 4 + offset` when `taken`, returning the cycle cost.
    fn branch_if(&mut self, taken: bool, base: u32, offset: i32) -> u64 {
        if taken {
            self.pc = base.wrapping_add(4).wrapping_add(offset as u32);
            2
        } else {
            1
        }
    }
}

impl McuCore for Xtensa {
    fn step(&mut self) {
        let base = self.pc;
        let b0 = self.read_u8(base);
        let b1 = self.read_u8(base.wrapping_add(1));
        let b2 = self.read_u8(base.wrapping_add(2));
        let (instr, len) = decode(b0, b1, b2);
        self.pc = base.wrapping_add(len as u32);
        let cycles = self.execute(instr, base);
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
        Ok(())
    }

    fn pin_count(&self) -> usize {
        self.variant.pin_count()
    }

    fn next_event(&self) -> Option<u64> {
        // The modelled GPIO block has no scheduled events; the core free-runs.
        None
    }

    fn step_to_cycle(&mut self, until_cycle: u64, io: &mut PinIo) -> u64 {
        self.gpio.sample_inputs(io);
        while self.cycles < until_cycle {
            self.step();
        }
        self.gpio.publish_drives(io);
        self.cycles
    }
}

impl Xtensa {
    /// A single co-simulation step exchanging pin state: sample inputs, execute
    /// one instruction, publish drives. The fine-grained counterpart to
    /// [`step_to_cycle`](McuCore::step_to_cycle).
    pub fn step_with_io(&mut self, io: &mut PinIo) {
        self.gpio.sample_inputs(io);
        self.step();
        self.gpio.publish_drives(io);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a core, load a raw byte image, and start execution at `entry`.
    fn with_program(image: &[u8], entry: u32) -> Xtensa {
        let mut c = Xtensa::new();
        c.load_program(image);
        c.set_pc(entry);
        c
    }

    fn run(c: &mut Xtensa, n: usize) {
        for _ in 0..n {
            c.step();
        }
    }

    #[test]
    fn movi_and_add_set_registers() {
        // MOVI a2, 10 ; MOVI a3, 22 ; ADD a4, a2, a3 → a4 = 32.
        // MOVI a2,10 = 22 A0 0A ; MOVI a3,22 = 32 A0 16 ; ADD a4,a2,a3 = 30 42 80.
        let mut c = with_program(&[0x22, 0xA0, 0x0A, 0x32, 0xA0, 0x16, 0x30, 0x42, 0x80], 0);
        run(&mut c, 3);
        assert_eq!(c.reg(2), 10);
        assert_eq!(c.reg(3), 22);
        assert_eq!(c.reg(4), 32);
    }

    #[test]
    fn movi_sign_extends_and_addi_subtracts() {
        // MOVI a2, -1 (0xFFFFFFFF) ; ADDI a3, a2, 2 → a3 = 1.
        // MOVI a2,-1 = 22 AF FF ; ADDI a3,a2,2 = 32 C2 02.
        let mut c = with_program(&[0x22, 0xAF, 0xFF, 0x32, 0xC2, 0x02], 0);
        run(&mut c, 2);
        assert_eq!(c.reg(2), 0xFFFF_FFFF);
        assert_eq!(c.reg(3), 1);
    }

    #[test]
    fn logical_ops_and_mov_n() {
        // MOVI a2,0xFF ; MOVI a3,0x0F ; AND a4,a2,a3 ; OR a5,a2,a3 ; XOR a6,a2,a3.
        // Then MOV.N a7, a4.
        let prog = [
            0x22, 0xA0, 0xFF, // MOVI a2, 0xFF
            0x32, 0xA0, 0x0F, // MOVI a3, 0x0F
            0x30, 0x42, 0x10, // AND a4, a2, a3
            0x30, 0x52, 0x20, // OR  a5, a2, a3
            0x30, 0x62, 0x30, // XOR a6, a2, a3
            0x7D, 0x04, // MOV.N a7, a4 (op0=0xD,r=0,s=4,t=7 → 0x047D → bytes 7D 04)
        ];
        let mut c = with_program(&prog, 0);
        run(&mut c, 6);
        assert_eq!(c.reg(4), 0x0F, "AND");
        assert_eq!(c.reg(5), 0xFF, "OR");
        assert_eq!(c.reg(6), 0xF0, "XOR");
        assert_eq!(c.reg(7), 0x0F, "MOV.N copied a4");
    }

    #[test]
    fn store_then_load_round_trips_through_memory() {
        // MOVI a2, 0x40 (base) ; MOVI a3, 0x5A (data) ; S32I a3,a2,0 ; L32I a4,a2,0.
        let prog = [
            0x22, 0xA0, 0x40, // MOVI a2, 0x40
            0x32, 0xA0, 0x5A, // MOVI a3, 0x5A
            0x32, 0x62, 0x00, // S32I a3, a2, 0
            0x42, 0x22, 0x00, // L32I a4, a2, 0
        ];
        let mut c = with_program(&prog, 0);
        run(&mut c, 4);
        assert_eq!(c.read_u32(0x40), 0x5A);
        assert_eq!(c.reg(4), 0x5A);
    }

    #[test]
    fn l32r_loads_a_pool_literal() {
        // Literal 0xDEADBEEF at address 0; L32R a2 at address 4 reads it.
        // L32R a2, -4 = 21 FF FF.
        let mut image = vec![0u8; 8];
        image[0..4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        image[4..7].copy_from_slice(&[0x21, 0xFF, 0xFF]);
        let mut c = with_program(&image, 4);
        c.step(); // L32R
        assert_eq!(c.reg(2), 0xDEAD_BEEF);
    }

    #[test]
    fn bnez_builds_a_countdown_loop() {
        // MOVI a2, 5 ; MOVI a3, 0
        // loop: ADDI a3, a3, 1 ; ADDI a2, a2, -1 ; BNEZ a2, loop
        // Addresses: 0 MOVI 3 MOVI 6 ADDI 9 ADDI 12 BNEZ
        // BNEZ a2, loop(=6): offset = 6 - (12 + 4) = -10 → imm12 = 0xFF6.
        // w = 6 | (1<<4) | (1<<6) | (2<<8) | (0xFF6<<12) = 0xFF6256 → bytes 56 62 FF.
        let prog = [
            0x22, 0xA0, 0x05, // MOVI a2, 5
            0x32, 0xA0, 0x00, // MOVI a3, 0
            0x32, 0xC3, 0x01, // ADDI a3, a3, 1
            0x22, 0xC2, 0xFF, // ADDI a2, a2, -1
            0x56, 0x62, 0xFF, // BNEZ a2, loop
        ];
        let mut c = with_program(&prog, 0);
        // 2 setup + 5 * 3 loop-body instructions.
        run(&mut c, 2 + 5 * 3);
        assert_eq!(c.reg(2), 0, "counter reached zero");
        assert_eq!(c.reg(3), 5, "loop body ran five times");
    }

    #[test]
    fn j_jumps_relative() {
        // MOVI a2, 1 ; J +3 (skip the next MOVI) ; MOVI a2, 9 ; MOVI a3, 7.
        // J at addr 3, skip the MOVI at 6 (3 bytes) to land at 9: target = 9,
        // offset = 9 - (3 + 4) = 2 → imm18 = 2. w = 6 | (2<<6) = 0x86 → 86 00 00.
        let prog = [
            0x22, 0xA0, 0x01, // 0: MOVI a2, 1
            0x86, 0x00, 0x00, // 3: J +2 → addr 9
            0x22, 0xA0, 0x09, // 6: MOVI a2, 9 (skipped)
            0x32, 0xA0, 0x07, // 9: MOVI a3, 7
        ];
        let mut c = with_program(&prog, 0);
        c.step(); // MOVI a2, 1
        c.step(); // J
        assert_eq!(c.pc(), 9, "jumped over the skipped MOVI");
        c.step(); // MOVI a3, 7
        assert_eq!(c.reg(2), 1, "the skipped MOVI never ran");
        assert_eq!(c.reg(3), 7);
    }

    #[test]
    fn call0_and_ret_round_trip() {
        // CALL0 to a tiny routine that sets a4, then RET back. CALL0 targets are
        // word-aligned relative to the aligned PC (`target = (PC & ~3) + 4 +
        // (sext(imm18) << 2)`), so the subroutine sits at address 12 (reachable
        // with raw imm18 = 2 from the CALL0 at address 0).
        //  0: CALL0 → 12                    85 00 00  (offset = 8; a0 ← 3)
        //  3: MOVI a3, 7 (after return)     32 A0 07
        //  6: J . (spin)                    06 FF FF  (offset = -4)
        //  9: (padding, never executed)     00 00 00
        // 12: MOVI a4, 0x55                 42 A0 55
        // 15: RET                           80 00 00
        let prog = [
            0x85, 0x00, 0x00, // 0: CALL0 → 12
            0x32, 0xA0, 0x07, // 3: MOVI a3, 7
            0x06, 0xFF, 0xFF, // 6: J . (spin)
            0x00, 0x00, 0x00, // 9: padding
            0x42, 0xA0, 0x55, // 12: MOVI a4, 0x55
            0x80, 0x00, 0x00, // 15: RET
        ];
        let mut c = with_program(&prog, 0);
        c.step(); // CALL0 → pc = 12, a0 = 3
        assert_eq!(c.pc(), 12);
        assert_eq!(c.reg(0), 3, "a0 holds the return address");
        c.step(); // MOVI a4, 0x55
        assert_eq!(c.reg(4), 0x55);
        c.step(); // RET → pc = a0 = 3
        assert_eq!(c.pc(), 3);
        c.step(); // MOVI a3, 7
        assert_eq!(c.reg(3), 7);
    }

    #[test]
    fn step_to_advances_by_the_clock() {
        // J . (spin), 2 cycles each.
        let prog = [0x06, 0xFF, 0xFF];
        let mut c = with_program(&prog, 0);
        c.set_clock_hz(1_000_000.0);
        c.step_to(100e-6); // 100 µs → ~100 cycles
        assert!(c.cycles() >= 100, "ran at least 100 cycles: {}", c.cycles());
    }

    #[test]
    fn firmware_toggles_a_gpio_pin() {
        use super::super::periph::GPIO_BASE;
        // The GPIO blink inner loop, stepped directly on the core.
        //  literal GPIO_BASE at addr 0 ; code from addr 4.
        let mut image = vec![0u8; 0x18];
        image[0..4].copy_from_slice(&GPIO_BASE.to_le_bytes());
        let code: [(usize, [u8; 3]); 6] = [
            (0x04, [0x21, 0xFF, 0xFF]), // L32R a2, -4   → a2 = GPIO_BASE
            (0x07, [0x32, 0xA0, 0x04]), // MOVI a3, 4    → mask = 1<<2 (GPIO2)
            (0x0A, [0x32, 0x62, 0x09]), // S32I a3,a2,0x24 → ENABLE_W1TS (GPIO2 output)
            (0x0D, [0x32, 0x62, 0x02]), // S32I a3,a2,0x08 → OUT_W1TS (high)
            (0x10, [0x32, 0x62, 0x03]), // S32I a3,a2,0x0C → OUT_W1TC (low)
            (0x13, [0x86, 0xFD, 0xFF]), // J 0x0D (loop)
        ];
        for (addr, bytes) in code {
            image[addr..addr + 3].copy_from_slice(&bytes);
        }
        let mut c = with_program(&image, 0x04);
        c.step(); // L32R
        assert_eq!(c.reg(2), GPIO_BASE);
        c.step(); // MOVI mask
        c.step(); // ENABLE output
        assert!(!c.gpio_level(2), "enabled but still low");
        c.step(); // OUT_W1TS → high
        assert!(c.gpio_level(2), "GPIO2 driven high");
        c.step(); // OUT_W1TC → low
        assert!(!c.gpio_level(2), "GPIO2 driven low");
        c.step(); // J back to loop
        assert_eq!(c.pc(), 0x0D);
        c.step(); // OUT_W1TS → high again
        assert!(c.gpio_level(2), "GPIO2 toggled high again");
    }

    #[test]
    fn load_firmware_rejects_elf_and_empty() {
        let mut c = Xtensa::new();
        assert_eq!(
            c.load_firmware(&[0x7F, b'E', b'L', b'F', 0, 0]),
            Err(FirmwareError::UnsupportedFormat)
        );
        assert_eq!(c.load_firmware(&[]), Err(FirmwareError::BadImage));
    }
}
