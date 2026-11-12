//! ColdFire V4e integer core (ISA_A + ISA_B + ISA_C) with the EMAC unit, as
//! found in the MCF5441x. No FPU and no MMU: the MCF54418 has neither, and a
//! float opcode raises a line-F exception as it would on the device.
//!
//! Encodings and semantics follow the ColdFire Family Programmer's Reference
//! Manual (CFPRM) and the MCF5441x Reference Manual chapter 5 (EMAC). The
//! EMAC is modelled on its physical registers, ACCn and ACCextn, so a MACSR
//! mode change reinterprets them rather than repacking anything -- which is
//! where the reference emulator's patched Unicorn went wrong three times
//! (docs: "the patched Unicorn got the EMAC's arithmetic wrong in both
//! modes the audio engine uses").

use crate::bus::Bus;

pub const CF_C: u16 = 0x01;
pub const CF_V: u16 = 0x02;
pub const CF_Z: u16 = 0x04;
pub const CF_N: u16 = 0x08;
pub const CF_X: u16 = 0x10;
pub const SR_S: u16 = 0x2000;
pub const SR_T: u16 = 0x8000;

pub const MACSR_EV: u32 = 0x001;
pub const MACSR_V: u32 = 0x002;
pub const MACSR_Z: u32 = 0x004;
pub const MACSR_N: u32 = 0x008;
pub const MACSR_RT: u32 = 0x010;
pub const MACSR_FI: u32 = 0x020;
pub const MACSR_SU: u32 = 0x040;
pub const MACSR_OMC: u32 = 0x080;
pub const MACSR_PAV0: u32 = 0x100;

/// Exception vectors the core raises itself.
pub const VEC_ACCESS: u8 = 2;
pub const VEC_ADDRESS: u8 = 3;
pub const VEC_ILLEGAL: u8 = 4;
pub const VEC_DIVZERO: u8 = 5;
pub const VEC_PRIV: u8 = 8;
pub const VEC_TRACE: u8 = 9;
pub const VEC_LINEA: u8 = 10;
pub const VEC_LINEF: u8 = 11;
pub const VEC_FORMAT: u8 = 14;
pub const VEC_TRAP0: u8 = 32;

/// Why `run` returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The instruction budget ran out.
    Budget,
    /// PC reached an address with a hook set; nothing at PC has executed.
    Hook(u32),
    /// The core is in STOP and no interrupt can wake it before the budget.
    Stopped,
    /// A peripheral event is due (`io.deadline` reached).
    Event,
    /// An exception whose vector is empty or points nowhere sensible.
    Fault(u8, u32),
    /// The firmware asked for a software reset (`Io::reset_request`).
    Reset,
}

#[derive(Clone, Copy)]
pub(crate) enum Loc {
    D(usize),
    A(usize),
    M(u32),
    I(u32),
}

// `reg` and the block compiler index Rn as 0-15 from the struct's start.
const _: () = assert!(std::mem::offset_of!(Cpu, d) == 0 && std::mem::offset_of!(Cpu, a) == 32);

/// `repr(C)` keeps `a` straight after `d`, so `reg` can index Rn as 0-15.
#[repr(C)]
pub struct Cpu {
    pub d: [u32; 8],
    pub a: [u32; 8],
    pub pc: u32,
    pub sr: u16,
    /// The inactive stack pointer, when CACR[EUSP] banks A7.
    pub other_sp: u32,
    pub vbr: u32,
    pub cacr: u32,
    /// Every other MOVEC register, by Rc.
    pub ctl: std::collections::BTreeMap<u16, u32>,

    pub macsr: u32,
    pub mask: u32,
    /// ACC0-3 as 48-bit values in the current MACSR mode (sign-extended,
    /// or zero-extended in unsigned integer mode). The physical registers,
    /// ACCn and ACCextn, are derived from these (`acc_phys`) and a mode
    /// change reinterprets them (`set_macsr`), as on the device.
    pub accv: [i64; 4],

    pub stopped: bool,
    /// Address of the instruction being executed.
    pub op_pc: u32,
    pub bus: Bus,

    /// One bit per halfword of the hooked code window.
    hook_bits: Vec<u64>,
    hook_base: u32,
    /// Let the instruction at this hooked PC execute once without stopping.
    /// Tied to the address, so an interrupt taken first cannot consume it.
    pub skip_hook: Option<u32>,
    pub last_fault: Option<(u8, u32)>,
    /// Count of exceptions raised by vector, for diagnostics.
    pub exc_counts: [u64; 256],
    /// Guest PC samples, at block entries about every 64 instructions.
    pub profile: Option<std::collections::HashMap<u32, u64>>,
    next_sample: u64,
    /// A ring of the last executed PCs, when enabled (debugging).
    pub history: Option<(Vec<u32>, usize)>,
    /// The block compiler, compiling in line (tests, cfdiff)...
    pub jit: Option<Box<crate::jit::Jit>>,
    /// ...or on its own thread (the machine). Neither: interpreter only.
    pub jitw: Option<crate::jit::Worker>,
    /// Entries into compiled code.
    pub jit_runs: u64,
    /// `run`'s clock limit, for compiled loops deciding to go round again.
    pub limit: u64,
    /// The first few faults: (vector, pc, opcode, clock).
    pub fault_log: Vec<(u8, u32, u16, u64)>,
}

const HOOK_WINDOW: u32 = 0x0080_0000; // 8 MB of code from hook_base

#[inline(always)]
fn sz_mask(sz: u32) -> u32 {
    match sz {
        1 => 0xFF,
        2 => 0xFFFF,
        _ => 0xFFFF_FFFF,
    }
}

#[inline(always)]
fn sz_msb(sz: u32) -> u32 {
    match sz {
        1 => 0x80,
        2 => 0x8000,
        _ => 0x8000_0000,
    }
}

#[inline(always)]
fn sext(v: u32, sz: u32) -> u32 {
    match sz {
        1 => v as u8 as i8 as i32 as u32,
        2 => v as u16 as i16 as i32 as u32,
        _ => v,
    }
}

impl Cpu {
    pub fn new(bus: Bus) -> Cpu {
        Cpu {
            d: [0; 8],
            a: [0; 8],
            pc: 0,
            sr: 0x2700,
            other_sp: 0,
            vbr: 0,
            cacr: 0,
            ctl: Default::default(),
            macsr: 0,
            mask: 0xFFFF_FFFF,
            accv: [0; 4],
            stopped: false,
            op_pc: 0,
            bus,
            hook_bits: vec![0; (HOOK_WINDOW / 2 / 64) as usize],
            hook_base: 0x4000_0000,
            skip_hook: None,
            last_fault: None,
            exc_counts: [0; 256],
            fault_log: Vec::new(),
            history: None,
            jit: None,
            jitw: None,
            jit_runs: 0,
            limit: 0,
            profile: None,
            next_sample: 0,
        }
    }

    // -- hooks ------------------------------------------------------------

    pub fn set_hook(&mut self, pc: u32, on: bool) {
        // A hooked PC must start a block.
        self.bus.icache_flush();
        let o = pc.wrapping_sub(self.hook_base);
        assert!(o < HOOK_WINDOW, "hook 0x{pc:08x} outside the hook window");
        let bit = (o >> 1) as usize;
        if on {
            self.hook_bits[bit >> 6] |= 1 << (bit & 63);
        } else {
            self.hook_bits[bit >> 6] &= !(1 << (bit & 63));
        }
    }

    #[inline(always)]
    pub(crate) fn hooked(&self, pc: u32) -> bool {
        let o = pc.wrapping_sub(self.hook_base);
        if o >= HOOK_WINDOW {
            return false;
        }
        let bit = (o >> 1) as usize;
        self.hook_bits[bit >> 6] & (1 << (bit & 63)) != 0
    }

    // -- status register --------------------------------------------------

    #[inline(always)]
    pub fn ipl(&self) -> u8 {
        ((self.sr >> 8) & 7) as u8
    }

    #[inline(always)]
    pub fn supervisor(&self) -> bool {
        self.sr & SR_S != 0
    }

    /// Install a new SR, banking A7 if CACR[EUSP] says the core has two.
    pub fn set_sr(&mut self, v: u16) {
        let v = v & 0xB71F;
        if self.cacr & 0x20 != 0 && (v ^ self.sr) & SR_S != 0 {
            std::mem::swap(&mut self.a[7], &mut self.other_sp);
        }
        self.sr = v;
    }

    #[inline(always)]
    pub(crate) fn set_ccr(&mut self, v: u16) {
        self.sr = (self.sr & 0xFF00) | (v & 0x1F);
    }

    #[inline(always)]
    pub(crate) fn flag(&self, f: u16) -> bool {
        self.sr & f != 0
    }

    /// Rn as 0-15: D0-D7 then A0-A7.
    #[inline(always)]
    pub(crate) fn reg(&self, n: usize) -> u32 {
        // SAFETY: `repr(C)` puts `d` at offset 0 and `a` directly after it,
        // so the first 16 words of the struct are D0-D7, A0-A7.
        unsafe { *(self as *const Cpu as *const u32).add(n & 15) }
    }

    /// Set Rn as 0-15 (see `reg`).
    #[inline(always)]
    pub(crate) fn set_reg(&mut self, n: usize, v: u32) {
        if n & 8 != 0 {
            self.a[n & 7] = v;
        } else {
            self.d[n & 7] = v;
        }
    }

    #[inline(always)]
    pub(crate) fn set_nz(&mut self, v: u32, sz: u32) {
        let m = sz_mask(sz);
        let mut c = self.sr & !(CF_N | CF_Z | CF_V | CF_C);
        if v & m == 0 {
            c |= CF_Z;
        }
        if v & sz_msb(sz) != 0 {
            c |= CF_N;
        }
        self.sr = c;
    }

    pub(crate) fn cond(&self, cc: u16) -> bool {
        let c = self.flag(CF_C);
        let v = self.flag(CF_V);
        let z = self.flag(CF_Z);
        let n = self.flag(CF_N);
        match cc & 15 {
            0 => true,
            1 => false,
            2 => !c && !z,
            3 => c || z,
            4 => !c,
            5 => c,
            6 => !z,
            7 => z,
            8 => !v,
            9 => v,
            10 => !n,
            11 => n,
            12 => n == v,
            13 => n != v,
            14 => !z && n == v,
            _ => z || n != v,
        }
    }

    // -- memory -----------------------------------------------------------

    #[inline(always)]
    pub fn rd(&mut self, a: u32, sz: u32) -> u32 {
        match sz {
            1 => self.bus.read8(a) as u32,
            2 => self.bus.read16(a) as u32,
            _ => self.bus.read32(a),
        }
    }

    #[inline(always)]
    pub fn wr(&mut self, a: u32, sz: u32, v: u32) {
        match sz {
            1 => self.bus.write8(a, v as u8),
            2 => self.bus.write16(a, v as u16),
            _ => self.bus.write32(a, v),
        }
    }

    #[inline(always)]
    pub(crate) fn fetch16(&mut self) -> u16 {
        let v = self.bus.ifetch16(self.pc);
        self.pc = self.pc.wrapping_add(2);
        v
    }

    #[inline(always)]
    pub(crate) fn fetch32(&mut self) -> u32 {
        let v = self.bus.ifetch32(self.pc);
        self.pc = self.pc.wrapping_add(4);
        v
    }

    pub fn push32(&mut self, v: u32) {
        self.a[7] = self.a[7].wrapping_sub(4);
        let sp = self.a[7];
        self.bus.write32(sp, v);
    }

    pub fn pop32(&mut self) -> u32 {
        let sp = self.a[7];
        let v = self.bus.read32(sp);
        self.a[7] = sp.wrapping_add(4);
        v
    }

    // -- effective addresses ---------------------------------------------

    /// The brief extension word format: (d8, base, Xi.size*scale).
    pub(crate) fn index_ea(&mut self, base: u32) -> u32 {
        let ext = self.fetch16();
        let r = ((ext >> 12) & 7) as usize;
        let xi = if ext & 0x8000 != 0 { self.a[r] } else { self.d[r] };
        let xi = if ext & 0x0800 != 0 { xi } else { xi as u16 as i16 as i32 as u32 };
        let scale = (ext >> 9) & 3;
        base.wrapping_add(ext as u8 as i8 as i32 as u32)
            .wrapping_add(xi << scale)
    }

    /// Resolve an effective address for an operand of `sz` bytes. Applies
    /// (An)+ and -(An) side effects, so call it exactly once per operand.
    pub(crate) fn ea(&mut self, mode: u16, reg: u16, sz: u32) -> Loc {
        let r = reg as usize;
        match mode {
            0 => Loc::D(r),
            1 => Loc::A(r),
            2 => Loc::M(self.a[r]),
            // ColdFire keeps no word alignment for A7: a byte access through
            // (A7)+ or -(A7) moves it by one (the 680x0 rule of two does not
            // apply; QEMU gates it on the 680x0 feature).
            3 => {
                let a = self.a[r];
                self.a[r] = a.wrapping_add(sz);
                Loc::M(a)
            }
            4 => {
                let a = self.a[r].wrapping_sub(sz);
                self.a[r] = a;
                Loc::M(a)
            }
            5 => {
                let d = self.fetch16() as i16 as i32 as u32;
                Loc::M(self.a[r].wrapping_add(d))
            }
            6 => {
                let b = self.a[r];
                Loc::M(self.index_ea(b))
            }
            _ => match reg {
                0 => Loc::M(self.fetch16() as i16 as i32 as u32),
                1 => Loc::M(self.fetch32()),
                2 => {
                    let base = self.pc;
                    let d = self.fetch16() as i16 as i32 as u32;
                    Loc::M(base.wrapping_add(d))
                }
                3 => {
                    let base = self.pc;
                    Loc::M(self.index_ea(base))
                }
                4 => {
                    let v = match sz {
                        1 => self.fetch16() as u32 & 0xFF,
                        2 => self.fetch16() as u32,
                        _ => self.fetch32(),
                    };
                    Loc::I(v)
                }
                _ => Loc::I(0),
            },
        }
    }

    /// The address of a control-mode EA (LEA, PEA, JMP, JSR, MOVEM).
    pub(crate) fn ea_addr(&mut self, mode: u16, reg: u16) -> Option<u32> {
        match mode {
            2 | 5 | 6 => match self.ea(mode, reg, 4) {
                Loc::M(a) => Some(a),
                _ => None,
            },
            7 if reg <= 3 => match self.ea(mode, reg, 4) {
                Loc::M(a) => Some(a),
                _ => None,
            },
            _ => None,
        }
    }

    #[inline(always)]
    pub(crate) fn read_loc(&mut self, l: Loc, sz: u32) -> u32 {
        match l {
            Loc::D(r) => self.d[r] & sz_mask(sz),
            Loc::A(r) => self.a[r] & sz_mask(sz),
            Loc::M(a) => self.rd(a, sz),
            Loc::I(v) => v & sz_mask(sz),
        }
    }

    #[inline(always)]
    pub(crate) fn write_loc(&mut self, l: Loc, sz: u32, v: u32) {
        match l {
            Loc::D(r) => {
                let m = sz_mask(sz);
                self.d[r] = (self.d[r] & !m) | (v & m);
            }
            Loc::A(r) => self.a[r] = v,
            Loc::M(a) => self.wr(a, sz, v),
            Loc::I(_) => {}
        }
    }

    // -- exceptions -------------------------------------------------------

    /// Take exception `vec` with `pc` in the frame. `level` raises the
    /// interrupt mask (interrupts); synchronous exceptions leave it.
    pub fn exception(&mut self, vec: u8, pc: u32, level: Option<u8>) {
        self.exc_counts[vec as usize] += 1;
        if let Some(t) = self.bus.timing.as_mut() {
            self.bus.io.now += t.exception();
        }
        let old_sr = self.sr;
        let mut new_sr = (old_sr | SR_S) & !SR_T;
        if let Some(l) = level {
            new_sr = (new_sr & !0x0700) | ((l as u16 & 7) << 8);
        }
        self.set_sr(new_sr);
        // The frame is longword aligned; the format field records by how
        // much the stack pointer was misaligned so RTE can undo it.
        let sp = self.a[7];
        let fmt = 4 + (sp & 3);
        self.a[7] = sp & !3;
        self.push32(pc);
        let w0 = (fmt << 28) | ((vec as u32) << 18) | old_sr as u32;
        self.push32(w0);
        let handler = self.bus.read32(self.vbr.wrapping_add(vec as u32 * 4));
        self.pc = handler;
        self.stopped = false;
    }

    pub(crate) fn rte(&mut self) {
        let sp = self.a[7];
        let w0 = self.bus.read32(sp);
        let pc = self.bus.read32(sp.wrapping_add(4));
        let fmt = w0 >> 28;
        let adj = if (4..=7).contains(&fmt) { fmt - 4 } else { 0 };
        self.a[7] = sp.wrapping_add(8 + adj);
        self.set_sr(w0 as u16);
        self.pc = pc;
    }

    /// Deliver an interrupt now if the mask allows it. -> taken.
    pub fn interrupt(&mut self, vec: u8, level: u8) -> bool {
        if level < 7 && level <= self.ipl() {
            return false;
        }
        let pc = self.pc;
        self.exception(vec, pc, Some(level));
        true
    }

    /// Strict mode (`Bus::strict`): the instruction at `bus.pc` touched an
    /// unmapped page or the SRAM mirror. It has completed; take an access
    /// error with its PC in the frame, as `illegal` does.
    #[cold]
    fn strict_fault(&mut self) {
        let Some((pc, _addr)) = self.bus.strict_pending.take() else { return };
        if self.fault_log.len() < 32 {
            let op = self.bus.peek16(pc);
            self.fault_log.push((VEC_ACCESS, pc, op, self.bus.io.now));
        }
        self.last_fault = Some((VEC_ACCESS, pc));
        self.exception(VEC_ACCESS, pc, None);
    }

    pub(crate) fn illegal(&mut self, vec: u8) {
        let pc = self.op_pc;
        if self.fault_log.len() < 32 {
            let op = self.bus.peek16(pc);
            self.fault_log.push((vec, pc, op, self.bus.io.now));
        }
        self.last_fault = Some((vec, pc));
        self.exception(vec, pc, None);
    }

    // -- the run loop -----------------------------------------------------

    /// Instructions executed so far: the machine's clock.
    #[inline(always)]
    pub fn icount(&self) -> u64 {
        self.bus.io.now
    }

    /// Execute until the clock reaches `until`, a peripheral event falls
    /// due, a hook is hit, or the core stops with nothing to wake it.
    pub fn run(&mut self, until: u64) -> Stop {
        self.limit = until;
        loop {
            // Interrupts are sampled between instructions.
            let lvl = self.bus.io.irq_level;
            if lvl != 0 && (lvl == 7 || lvl > self.ipl()) {
                if let Some((vec, level)) = self.bus.io.ack_irq(self.ipl()) {
                    let pc = self.pc;
                    self.exception(vec, pc, Some(level));
                }
            }
            let now = self.bus.io.now;
            if now >= until {
                return Stop::Budget;
            }
            if now >= self.bus.io.deadline {
                return Stop::Event;
            }
            if self.stopped {
                return Stop::Stopped;
            }
            if self.bus.timing.is_some() {
                self.timing_phase();
            }
            let pc = self.pc;
            // Fast path: a built block. None starts at a hooked PC (hooks
            // flush the cache, and the slow path below never stores one),
            // so a hit needs no hook check.
            if self.history.is_none() {
                self.bus.icache_settle();
                if let Some(i) = self.bus.block_at(pc) {
                    self.run_block(pc, i);
                    if self.bus.strict_pending.is_some() {
                        self.strict_fault();
                    }
                    if let Some((v, p)) = self.last_fault.take() {
                        let h = self.bus.peek32(self.vbr.wrapping_add(v as u32 * 4));
                        if h == 0 || h == 0xFFFF_FFFF {
                            return Stop::Fault(v, p);
                        }
                    }
                    continue;
                }
            }
            let hooked = self.hooked(pc);
            if hooked {
                if self.skip_hook != Some(pc) {
                    return Stop::Hook(pc);
                }
                self.skip_hook = None;
            }
            if !hooked && self.history.is_none() && self.bus.in_code_window(pc) {
                let i = crate::fast::build_block(self, pc);
                self.run_block(pc, i);
            } else {
                self.op_pc = pc;
                self.bus.pc = pc;
                self.bus.io.now = now + 1;
                if let Some((h, i)) = &mut self.history {
                    let n = h.len();
                    h[*i % n] = pc;
                    *i += 1;
                }
                self.execute_timed();
            }
            if self.bus.strict_pending.is_some() {
                self.strict_fault();
            }
            if let Some((v, p)) = self.last_fault.take() {
                let h = self.bus.peek32(self.vbr.wrapping_add(v as u32 * 4));
                if h == 0 || h == 0xFFFF_FFFF {
                    return Stop::Fault(v, p);
                }
            }
        }
    }

    /// Run block `i` of the cache, which starts at `pc`: its compiled code
    /// if it has some (compiling it once it runs hot), else its ops.
    #[inline(always)]
    fn run_block(&mut self, pc: u32, i: usize) {
        if let Some(p) = &mut self.profile {
            if self.bus.io.now >= self.next_sample {
                *p.entry(pc).or_insert(0) += 1;
                self.next_sample = self.bus.io.now + 64;
            }
        }
        if (self.jit.is_some() || self.jitw.is_some()) && self.bus.watch.is_none() {
            let blk = &mut self.bus.blk_arena[i];
            if let Some(f) = blk.code {
                self.run_compiled(f);
                return;
            }
            blk.hits += 1;
            if blk.hits >= crate::jit::HOT && !blk.no_jit {
                let f = if self.jitw.is_some() { self.jit_async(pc, i) } else { self.compile_block(pc, i) };
                if let Some(f) = f {
                    self.run_compiled(f);
                    return;
                }
            }
        }
        let blk: *const [crate::fast::Op] = &*self.bus.blk_arena[i].ops;
        if self.bus.timing.is_some() {
            return self.run_ops_timed(pc, i, blk);
        }
        self.run_ops(pc, blk);
    }

    /// Unit timing: switch between measuring and charging learned costs.
    #[cold]
    #[inline(never)]
    fn timing_phase(&mut self) {
        let now = self.bus.io.now;
        if let Some(live) = self.bus.timing.as_mut().unwrap().phase(now) {
            self.bus.timing_live = live;
        }
    }

    /// Unit timing on a block: measured (and learned from) in a window,
    /// else run plain and charged its learned cost.
    #[cold]
    #[inline(never)]
    fn run_ops_timed(&mut self, pc: u32, i: usize, blk: *const [crate::fast::Op]) {
        let t0 = self.bus.io.now;
        let n = self.run_ops(pc, blk) as u64;
        let now = self.bus.io.now;
        let (cost, tn) = (self.bus.blk_arena[i].tcost, self.bus.blk_arena[i].tn);
        let t = self.bus.timing.as_mut().unwrap();
        if self.bus.timing_live {
            let extra = (now - t0).saturating_sub(n);
            let (c, k) = t.learn(now, extra, n, cost, tn);
            // The block may be gone if it wrote code (flush at the next
            // boundary), but its slot is still there.
            let b = &mut self.bus.blk_arena[i];
            b.tcost = c;
            b.tn = k;
        } else {
            let x = if tn > 0 { cost as f64 } else { t.stats.cold_runs += n; t.g_cold * n as f64 };
            t.stats.gap_ops += n;
            self.bus.io.now += t.charge_learned(x);
        }
    }

    /// Run compiled code, then the compiled blocks that follow, for as long
    /// as `run` would go straight from one to the next: no interrupt
    /// pending, the clock below the limit and the next event, not stopped,
    /// no fault, no cache flush pending, no profile being taken. (A block
    /// never starts at a hooked PC, so there is no hook to check.)
    #[inline(always)]
    fn run_compiled(&mut self, mut f: crate::jit::BlockFn) {
        loop {
            self.jit_runs += 1;
            // SAFETY: compiled for a block that is still cached (the cache
            // only drops blocks in icache_settle, between blocks).
            unsafe { f(self) };
            let io = &self.bus.io;
            if io.irq_level != 0
                || io.now >= self.limit
                || io.now >= io.deadline
                || self.stopped
                || self.last_fault.is_some()
                || self.bus.flush_pending
                || self.profile.is_some()
            {
                return;
            }
            match self.bus.block_at(self.pc).and_then(|j| self.bus.blk_arena[j].code) {
                Some(g) => f = g,
                None => return,
            }
        }
    }

    /// A hot block without code, with the compiler on its thread: send it
    /// the first time, then take in whatever has been compiled. -> block
    /// `i`'s code, if it has arrived.
    #[cold]
    fn jit_async(&mut self, pc: u32, i: usize) -> Option<crate::jit::BlockFn> {
        let epoch = self.bus.icache_flushes;
        if !self.bus.blk_arena[i].queued {
            self.bus.blk_arena[i].queued = true;
            let mut mem = crate::jit::Mem::of(&mut self.bus);
            mem.macsr = self.macsr;
            let w = self.jitw.as_ref()?;
            w.queue(i, epoch, pc, &self.bus.blk_arena[i].ops, mem);
            return None;
        }
        let w = self.jitw.as_ref()?;
        for d in w.poll() {
            if d.epoch == epoch && d.idx < self.bus.blk_arena.len() {
                let b = &mut self.bus.blk_arena[d.idx];
                b.code = d.f;
                b.no_jit = d.f.is_none();
            }
        }
        self.bus.blk_arena[i].code
    }

    /// Blocks compiled, declined, and seconds spent compiling.
    pub fn jit_stats(&self) -> (u64, u64, f64) {
        use std::sync::atomic::Ordering::Relaxed;
        if let Some(w) = &self.jitw {
            let s = &w.stats;
            (s.compiled.load(Relaxed), s.failed.load(Relaxed), s.nanos.load(Relaxed) as f64 * 1e-9)
        } else if let Some(j) = &self.jit {
            (j.compiled, j.failed, j.compile_secs)
        } else {
            (0, 0, 0.0)
        }
    }

    #[cold]
    fn compile_block(&mut self, pc: u32, i: usize) -> Option<crate::jit::BlockFn> {
        let jit = self.jit.as_mut()?;
        if jit.epoch != self.bus.icache_flushes {
            jit.reset(self.bus.icache_flushes);
        }
        let mut mem = crate::jit::Mem::of(&mut self.bus);
        mem.macsr = self.macsr;
        let blk = &mut self.bus.blk_arena[i];
        let f = jit.compile(pc, &blk.ops, mem);
        blk.code = f;
        blk.no_jit = f.is_none();
        f
    }

    /// `run_ops` for a block given as a slice (compiled code falling back).
    pub(crate) fn run_ops_slice(&mut self, pc: u32, ops: &[crate::fast::Op]) {
        self.run_ops(pc, ops as *const [crate::fast::Op]);
    }

    /// Run the predecoded block `blk`, which starts at `pc`. Stops early
    /// when control leaves the straight line: a taken branch, an
    /// exception, STOP.
    #[inline(always)]
    fn run_ops(&mut self, pc: u32, blk: *const [crate::fast::Op]) -> usize {
        // SAFETY: blocks are boxed and only dropped by `icache_settle`,
        // which runs between blocks, never while one executes.
        let ops: &[crate::fast::Op] = unsafe { &*blk };
        // `bus.pc` only labels debug logs: exact under a watchpoint, the
        // block's start otherwise. STOP ends its block, so `stopped` needs
        // no check here.
        self.bus.pc = pc;
        if self.bus.watch.is_some() || self.bus.strict {
            self.run_ops_loop::<true>(pc, ops)
        } else {
            self.run_ops_loop::<false>(pc, ops)
        }
    }

    #[inline(always)]
    fn run_ops_loop<const EXACT: bool>(&mut self, mut pc: u32, ops: &[crate::fast::Op]) -> usize {
        let mut n = 0;
        for op in ops {
            n += 1;
            self.op_pc = pc;
            if EXACT {
                self.bus.pc = pc;
            }
            self.bus.io.now += 1;
            let next = pc.wrapping_add(op.len as u32);
            self.pc = next;
            if self.bus.timing_live {
                let flow = self.timing_pre(op.op, op.len);
                (op.h)(self, op);
                self.timing_post(flow);
            } else {
                (op.h)(self, op);
            }
            if self.pc != next || (EXACT && self.bus.strict_pending.is_some()) {
                break;
            }
            pc = next;
        }
        n
    }

    /// Execute exactly one instruction.
    pub fn step(&mut self) {
        let pc = self.pc;
        self.op_pc = pc;
        self.bus.pc = pc;
        self.bus.io.now += 1;
        if self.bus.cache.is_some() {
            // The cache model fetches through the I-cache: no predecoding.
            return self.execute_timed();
        }
        let op = crate::fast::decode(self, pc);
        self.pc = pc.wrapping_add(op.len as u32);
        if self.bus.timing_live {
            let flow = self.timing_pre(op.op, op.len);
            (op.h)(self, &op);
            return self.timing_post(flow);
        }
        (op.h)(self, &op);
        if let Some(t) = self.bus.timing.as_mut() {
            let x = t.g_extra;
            self.bus.io.now += t.charge_learned(x);
        }
    }

    pub(crate) fn execute(&mut self) {
        let op = self.fetch16();
        self.dispatch(op);
    }

    /// `execute` with unit timing's costs around it (timing.rs). The
    /// predecoded path charges in `run_ops_loop` instead, so `h_slow`'s
    /// `execute` is not charged twice.
    fn execute_timed(&mut self) {
        if self.bus.timing.is_none() {
            return self.execute();
        }
        if !self.bus.timing_live {
            self.execute();
            let t = self.bus.timing.as_mut().unwrap();
            let x = t.g_extra;
            self.bus.io.now += t.charge_learned(x);
            return;
        }
        let op = self.bus.peek16(self.op_pc);
        let flow = self.timing_pre(op, 0);
        self.execute();
        self.timing_post(flow);
    }

    /// Unit timing: the cycles of `op` at `op_pc` (`len` bytes, 0 if not
    /// known), beyond the one the run loop counted.
    #[cold]
    #[inline(never)]
    fn timing_pre(&mut self, op: u16, len: u8) -> crate::timing::Flow {
        let pc = self.op_pc;
        let w1 = self.bus.peek16(pc.wrapping_add(2));
        let mut t = self.bus.timing.take().unwrap();
        if let Some(p) = t.probe.as_mut() {
            p.step(pc, self.bus.io.now, self.a[7]);
        }
        let mut extra = t.ifetch(pc, if len == 0 { 2 } else { len as u32 });
        // The other words are read only when the PC's memo entry misses.
        let bus = &self.bus;
        let words = || {
            let mut w = [op, w1, 0, 0, 0, 0];
            for (i, x) in w.iter_mut().enumerate().skip(2) {
                *x = bus.peek16(pc.wrapping_add(2 * i as u32));
            }
            w
        };
        let (x, flow, folded) = t.pre_memo(pc, op, w1, words);
        extra += x;
        self.bus.timing = Some(t);
        self.bus.io.now += extra;
        if folded {
            self.bus.io.now -= 1;
        }
        flow
    }

    #[cold]
    #[inline(never)]
    fn timing_post(&mut self, flow: crate::timing::Flow) {
        if flow == crate::timing::Flow::None {
            return;
        }
        let sp_top = self.bus.peek32(self.a[7]);
        let t = self.bus.timing.as_mut().unwrap();
        let c = t.post(flow, self.pc, sp_top);
        self.bus.io.now = self.bus.io.now.wrapping_add_signed(c);
    }

    /// Execute `op`, already fetched, with PC past it.
    pub(crate) fn dispatch(&mut self, op: u16) {
        match op >> 12 {
            0x0 => self.line0(op),
            0x1 => self.op_move(op, 1),
            0x2 => self.op_move(op, 4),
            0x3 => self.op_move(op, 2),
            0x4 => self.line4(op),
            0x5 => self.line5(op),
            0x6 => self.line6(op),
            0x7 => self.line7(op),
            0x8 => self.line8(op),
            0x9 => self.line9d(op, false),
            0xA => self.linea(op),
            0xB => self.lineb(op),
            0xC => self.linec(op),
            0xD => self.line9d(op, true),
            0xE => self.linee(op),
            _ => self.linef(op),
        }
    }

    // -- arithmetic helpers -----------------------------------------------

    pub(crate) fn add_flags(&mut self, s: u32, d: u32, sz: u32, x: bool) -> u32 {
        let m = sz_mask(sz);
        let (s, d) = (s & m, d & m);
        let wide = s as u64 + d as u64 + x as u64;
        let r = (wide as u32) & m;
        let msb = sz_msb(sz);
        let carry = if sz == 4 { wide >> 32 != 0 } else { wide as u32 & (m + 1) != 0 };
        let over = (s ^ r) & (d ^ r) & msb != 0;
        let mut c = self.sr & !0x1F;
        if carry {
            c |= CF_C | CF_X;
        }
        if over {
            c |= CF_V;
        }
        if r & msb != 0 {
            c |= CF_N;
        }
        if r == 0 {
            c |= CF_Z;
        }
        self.sr = c;
        r
    }

    /// d - s - x, all condition codes including X.
    pub(crate) fn sub_flags(&mut self, s: u32, d: u32, sz: u32, x: bool) -> u32 {
        let m = sz_mask(sz);
        let (s, d) = (s & m, d & m);
        let r = d.wrapping_sub(s).wrapping_sub(x as u32) & m;
        let msb = sz_msb(sz);
        let borrow = (s as u64 + x as u64) > d as u64;
        let over = (s ^ d) & (r ^ d) & msb != 0;
        let mut c = self.sr & !0x1F;
        if borrow {
            c |= CF_C | CF_X;
        }
        if over {
            c |= CF_V;
        }
        if r & msb != 0 {
            c |= CF_N;
        }
        if r == 0 {
            c |= CF_Z;
        }
        self.sr = c;
        r
    }

    pub(crate) fn cmp_flags(&mut self, s: u32, d: u32, sz: u32) {
        let x = self.sr & CF_X;
        self.sub_flags(s, d, sz, false);
        self.sr = (self.sr & !CF_X) | x;
    }

    // -- line 0: bit operations, immediates, ISA_C extras ------------------

    pub(crate) fn line0(&mut self, op: u16) {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        if op & 0x0100 != 0 {
            // Dynamic bit ops: 0000 ddd1 tt mmmrrr
            let bit = self.d[((op >> 9) & 7) as usize];
            self.bitop((op >> 6) & 3, bit, mode, reg);
            return;
        }
        match op & 0xFFC0 {
            0x0800 | 0x0840 | 0x0880 | 0x08C0 => {
                let bit = self.fetch16() as u32 & 0xFF;
                self.bitop((op >> 6) & 3, bit, mode, reg);
                return;
            }
            _ => {}
        }
        match op & 0xFFF8 {
            0x00C0 => {
                // BITREV
                let r = reg as usize;
                self.d[r] = self.d[r].reverse_bits();
            }
            0x02C0 => {
                // BYTEREV
                let r = reg as usize;
                self.d[r] = self.d[r].swap_bytes();
            }
            0x04C0 => {
                // FF1: the offset of the first set bit from the msb.
                let r = reg as usize;
                let v = self.d[r];
                self.set_nz(v, 4);
                self.d[r] = v.leading_zeros();
            }
            0x0080 | 0x0280 | 0x0480 | 0x0680 | 0x0A80 => {
                let imm = self.fetch32();
                let r = reg as usize;
                let dv = self.d[r];
                let res = match op & 0xFFF8 {
                    0x0080 => {
                        let v = dv | imm;
                        self.set_nz(v, 4);
                        v
                    }
                    0x0280 => {
                        let v = dv & imm;
                        self.set_nz(v, 4);
                        v
                    }
                    0x0480 => self.sub_flags(imm, dv, 4, false),
                    0x0680 => self.add_flags(imm, dv, 4, false),
                    _ => {
                        let v = dv ^ imm;
                        self.set_nz(v, 4);
                        v
                    }
                };
                self.d[r] = res;
            }
            0x0C00 | 0x0C40 | 0x0C80 => {
                // CMPI.sz #imm, Dn
                let sz = [1, 2, 4][((op >> 6) & 3) as usize];
                let imm = match sz {
                    4 => self.fetch32(),
                    _ => self.fetch16() as u32 & sz_mask(sz),
                };
                let dv = self.d[reg as usize];
                self.cmp_flags(imm, dv, sz);
            }
            _ => self.illegal(VEC_ILLEGAL),
        }
    }

    pub(crate) fn bitop(&mut self, kind: u16, bit: u32, mode: u16, reg: u16) {
        if mode == 0 {
            let r = reg as usize;
            let m = 1u32 << (bit & 31);
            let v = self.d[r];
            self.sr = (self.sr & !CF_Z) | if v & m == 0 { CF_Z } else { 0 };
            match kind {
                1 => self.d[r] = v ^ m,
                2 => self.d[r] = v & !m,
                3 => self.d[r] = v | m,
                _ => {}
            }
            return;
        }
        let l = self.ea(mode, reg, 1);
        let m = 1u32 << (bit & 7);
        let v = self.read_loc(l, 1);
        self.sr = (self.sr & !CF_Z) | if v & m == 0 { CF_Z } else { 0 };
        let nv = match kind {
            1 => v ^ m,
            2 => v & !m,
            3 => v | m,
            _ => return,
        };
        self.write_loc(l, 1, nv);
    }

    // -- MOVE ------------------------------------------------------------

    pub(crate) fn op_move(&mut self, op: u16, sz: u32) {
        let src = self.ea((op >> 3) & 7, op & 7, sz);
        let v = self.read_loc(src, sz);
        let dmode = (op >> 6) & 7;
        let dreg = (op >> 9) & 7;
        if dmode == 1 {
            // MOVEA: word sources sign-extend; no flags.
            self.a[dreg as usize] = sext(v, sz);
            return;
        }
        let dst = self.ea(dmode, dreg, sz);
        self.write_loc(dst, sz, v);
        self.set_nz(v, sz);
    }

    // -- line 4: miscellaneous --------------------------------------------

    pub(crate) fn line4(&mut self, op: u16) {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        let r = reg as usize;
        // LEA: 0100 aaa1 11 ea (0x49C0 with mode 0 is EXTB.L)
        if op & 0x01C0 == 0x01C0 {
            if op & 0xFFF8 == 0x49C0 {
                let v = self.d[r] as u8 as i8 as i32 as u32;
                self.d[r] = v;
                self.set_nz(v, 4);
                return;
            }
            match self.ea_addr(mode, reg) {
                Some(a) => self.a[((op >> 9) & 7) as usize] = a,
                None => self.illegal(VEC_ILLEGAL),
            }
            return;
        }
        match op {
            0x4E71 => return, // NOP
            0x4E73 => {
                if !self.supervisor() {
                    return self.illegal(VEC_PRIV);
                }
                self.rte();
                return;
            }
            0x4E75 => {
                self.pc = self.pop32();
                return;
            }
            0x4E72 => {
                let imm = self.fetch16();
                if !self.supervisor() {
                    return self.illegal(VEC_PRIV);
                }
                self.set_sr(imm);
                self.stopped = true;
                return;
            }
            0x4E7A | 0x4E7B => {
                let ext = self.fetch16();
                if !self.supervisor() {
                    return self.illegal(VEC_PRIV);
                }
                let rn = ((ext >> 12) & 7) as usize;
                let is_a = ext & 0x8000 != 0;
                let rc = ext & 0x0FFF;
                if op == 0x4E7B {
                    let v = if is_a { self.a[rn] } else { self.d[rn] };
                    self.movec_write(rc, v);
                } else {
                    let v = self.movec_read(rc);
                    if is_a {
                        self.a[rn] = v;
                    } else {
                        self.d[rn] = v;
                    }
                }
                return;
            }
            0x4AC8 => {
                // HALT: a debug halt. Treat it as a stop.
                self.stopped = true;
                return;
            }
            0x4ACC => return, // PULSE
            0x4AFC => return self.illegal(VEC_ILLEGAL),
            0x40E7 => {
                // STRLDSR #imm: 0x40E7 0x46FC imm
                if self.bus.ifetch16(self.pc) == 0x46FC {
                    self.pc = self.pc.wrapping_add(2);
                    let imm = self.fetch16();
                    if !self.supervisor() {
                        return self.illegal(VEC_PRIV);
                    }
                    let sr = self.sr as u32;
                    self.a[7] = self.a[7].wrapping_sub(4);
                    let sp = self.a[7];
                    self.bus.write32(sp, sr);
                    self.set_sr(imm);
                    return;
                }
                return self.illegal(VEC_ILLEGAL);
            }
            _ => {}
        }
        match op & 0xFFF0 {
            0x4E40 => {
                let vec = VEC_TRAP0 + (op & 15) as u8;
                let pc = self.pc;
                self.exception(vec, pc, None);
                return;
            }
            0x4E50 => {
                if op & 8 == 0 {
                    // LINK An,#d16
                    let d = self.fetch16() as i16 as i32 as u32;
                    let v = self.a[r];
                    self.push32(v);
                    self.a[r] = self.a[7];
                    self.a[7] = self.a[7].wrapping_add(d);
                } else {
                    // UNLK An
                    self.a[7] = self.a[r];
                    let v = self.pop32();
                    self.a[r] = v;
                }
                return;
            }
            0x4E60 => {
                if !self.supervisor() {
                    return self.illegal(VEC_PRIV);
                }
                if op & 8 == 0 {
                    self.other_sp = self.a[r];
                } else {
                    self.a[r] = self.other_sp;
                }
                return;
            }
            _ => {}
        }
        match op & 0xFFC0 {
            0x4E80 => {
                // JSR
                match self.ea_addr(mode, reg) {
                    Some(t) => {
                        let ret = self.pc;
                        self.push32(ret);
                        self.pc = t;
                    }
                    None => self.illegal(VEC_ILLEGAL),
                }
                return;
            }
            0x4EC0 => {
                match self.ea_addr(mode, reg) {
                    Some(t) => self.pc = t,
                    None => self.illegal(VEC_ILLEGAL),
                }
                return;
            }
            0x40C0 => {
                // MOVE SR,Dn
                if !self.supervisor() {
                    return self.illegal(VEC_PRIV);
                }
                if mode == 0 {
                    self.d[r] = (self.d[r] & 0xFFFF_0000) | self.sr as u32;
                } else {
                    self.illegal(VEC_ILLEGAL);
                }
                return;
            }
            0x42C0 => {
                // MOVE CCR,Dn
                if mode == 0 {
                    self.d[r] = (self.d[r] & 0xFFFF_0000) | (self.sr & 0x1F) as u32;
                } else {
                    self.illegal(VEC_ILLEGAL);
                }
                return;
            }
            0x44C0 => {
                // MOVE <ea>,CCR
                let l = self.ea(mode, reg, 2);
                let v = self.read_loc(l, 2);
                self.set_ccr(v as u16);
                return;
            }
            0x46C0 => {
                // MOVE <ea>,SR
                let l = self.ea(mode, reg, 2);
                let v = self.read_loc(l, 2);
                if !self.supervisor() {
                    return self.illegal(VEC_PRIV);
                }
                self.set_sr(v as u16);
                return;
            }
            0x4840 => {
                if mode == 0 {
                    // SWAP
                    let v = self.d[r].rotate_left(16);
                    self.d[r] = v;
                    self.set_nz(v, 4);
                } else {
                    // PEA
                    match self.ea_addr(mode, reg) {
                        Some(a) => self.push32(a),
                        None => self.illegal(VEC_ILLEGAL),
                    }
                }
                return;
            }
            0x4880 => {
                if mode == 0 {
                    // EXT.W
                    let v = self.d[r] as u8 as i8 as i16 as u16 as u32;
                    self.d[r] = (self.d[r] & 0xFFFF_0000) | v;
                    self.set_nz(v, 2);
                } else {
                    self.illegal(VEC_ILLEGAL);
                }
                return;
            }
            0x48C0 => {
                if mode == 0 {
                    // EXT.L
                    let v = self.d[r] as u16 as i16 as i32 as u32;
                    self.d[r] = v;
                    self.set_nz(v, 4);
                } else {
                    self.movem(mode, reg, true);
                }
                return;
            }
            0x4CC0 => {
                self.movem(mode, reg, false);
                return;
            }
            0x4C00 => return self.mul_l(mode, reg),
            0x4C40 => return self.div_l(mode, reg),
            0x4C80 => {
                if mode == 0 {
                    // SATS.L
                    if self.flag(CF_V) {
                        self.d[r] = if self.d[r] & 0x8000_0000 != 0 { 0x7FFF_FFFF } else { 0x8000_0000 };
                    }
                    let v = self.d[r];
                    self.set_nz(v, 4);
                } else {
                    self.illegal(VEC_ILLEGAL);
                }
                return;
            }
            0x4AC0 => {
                // TAS.B
                let l = self.ea(mode, reg, 1);
                let v = self.read_loc(l, 1);
                self.set_nz(v, 1);
                self.write_loc(l, 1, v | 0x80);
                return;
            }
            _ => {}
        }
        // Size-encoded single-operand group: 0100 xxx0 ss mmmrrr
        let ss = (op >> 6) & 3;
        match op & 0xFF00 {
            0x4000 if ss == 2 && mode == 0 => {
                // NEGX.L Dn
                let x = self.flag(CF_X);
                let z = self.sr & CF_Z;
                let v = self.sub_flags(self.d[r], 0, 4, x);
                if v != 0 {
                    self.sr &= !CF_Z;
                } else {
                    self.sr = (self.sr & !CF_Z) | z;
                }
                self.d[r] = v;
            }
            0x4200 if ss != 3 => {
                // CLR
                let sz = [1, 2, 4][ss as usize];
                let l = self.ea(mode, reg, sz);
                self.write_loc(l, sz, 0);
                self.sr = (self.sr & !0x0F) | CF_Z;
            }
            0x4400 if ss == 2 && mode == 0 => {
                let v = self.sub_flags(self.d[r], 0, 4, false);
                self.d[r] = v;
            }
            0x4600 if ss == 2 && mode == 0 => {
                let v = !self.d[r];
                self.d[r] = v;
                self.set_nz(v, 4);
            }
            0x4A00 if ss != 3 => {
                // TST
                let sz = [1, 2, 4][ss as usize];
                let l = self.ea(mode, reg, sz);
                let v = self.read_loc(l, sz);
                self.set_nz(v, sz);
            }
            _ => self.illegal(VEC_ILLEGAL),
        }
    }

    pub(crate) fn movem(&mut self, mode: u16, reg: u16, to_mem: bool) {
        let mask = self.fetch16();
        let base = match mode {
            2 => self.a[reg as usize],
            5 => {
                let d = self.fetch16() as i16 as i32 as u32;
                self.a[reg as usize].wrapping_add(d)
            }
            _ => return self.illegal(VEC_ILLEGAL),
        };
        let mut a = base;
        for i in 0..16 {
            if mask & (1 << i) == 0 {
                continue;
            }
            if to_mem {
                let v = if i < 8 { self.d[i] } else { self.a[i - 8] };
                self.bus.write32(a, v);
            } else {
                let v = self.bus.read32(a);
                if i < 8 {
                    self.d[i] = v;
                } else {
                    self.a[i - 8] = v;
                }
            }
            a = a.wrapping_add(4);
        }
    }

    pub(crate) fn mul_l(&mut self, mode: u16, reg: u16) {
        let ext = self.fetch16();
        let l = self.ea(mode, reg, 4);
        let s = self.read_loc(l, 4);
        let dl = ((ext >> 12) & 7) as usize;
        let res = if ext & 0x0800 != 0 {
            (self.d[dl] as i32).wrapping_mul(s as i32) as u32
        } else {
            self.d[dl].wrapping_mul(s)
        };
        self.d[dl] = res;
        self.set_nz(res, 4);
    }

    pub(crate) fn div_l(&mut self, mode: u16, reg: u16) {
        let ext = self.fetch16();
        let l = self.ea(mode, reg, 4);
        let s = self.read_loc(l, 4);
        let dq = ((ext >> 12) & 7) as usize;
        let dr = (ext & 7) as usize;
        let signed = ext & 0x0800 != 0;
        if s == 0 {
            let pc = self.pc;
            self.exception(VEC_DIVZERO, pc, None);
            return;
        }
        let dividend = self.d[dq];
        let (q, rem, over) = if signed {
            let (a, b) = (dividend as i32, s as i32);
            if a == i32::MIN && b == -1 {
                (0, 0, true)
            } else {
                ((a / b) as u32, (a % b) as u32, false)
            }
        } else {
            (dividend / s, dividend % s, false)
        };
        if over {
            self.div_overflow();
            return;
        }
        // The condition codes describe the quotient, for REM as for DIV.
        self.set_nz(q, 4);
        if dq == dr {
            self.d[dq] = q;
        } else {
            // REMS/REMU: the remainder goes to Dw, the dividend stays.
            self.d[dr] = rem;
        }
    }

    /// Division overflow: the destination is untouched, V set, C and Z
    /// clear, N kept (as the 68040 and QEMU's ColdFire model do).
    fn div_overflow(&mut self) {
        self.sr = (self.sr & !(CF_V | CF_C | CF_Z)) | CF_V;
    }

    fn movec_write(&mut self, rc: u16, v: u32) {
        match rc {
            0x002 => {
                let old_eusp = self.cacr & 0x20 != 0;
                self.cacr = v;
                let _ = old_eusp;
                if let Some(c) = self.bus.cache.as_mut() {
                    c.set_cacr(v);
                }
                if let Some(t) = self.bus.timing.as_mut() {
                    t.tags.set_cacr(v);
                }
            }
            0x004..=0x007 => {
                self.ctl.insert(rc, v);
                if let Some(c) = self.bus.cache.as_mut() {
                    c.set_acr((rc - 4) as usize, v);
                }
                if let Some(t) = self.bus.timing.as_mut() {
                    t.tags.acr[(rc - 4) as usize] = v;
                }
            }
            0x800 => self.other_sp = v,
            0x801 => self.vbr = v & 0xFFF0_0000,
            _ => {
                self.ctl.insert(rc, v);
            }
        }
    }

    /// Hand CACR and ACR0-3 to the cache model (after a snapshot load).
    pub fn cache_sync(&mut self) {
        let acr = [4u16, 5, 6, 7].map(|r| self.ctl.get(&r).copied().unwrap_or(0));
        let cacr = self.cacr;
        if let Some(c) = self.bus.cache.as_mut() {
            c.configure(cacr, acr);
        }
        if let Some(t) = self.bus.timing.as_mut() {
            t.tags.configure(cacr, acr);
        }
    }

    fn movec_read(&mut self, rc: u16) -> u32 {
        match rc {
            0x002 => self.cacr,
            0x800 => self.other_sp,
            0x801 => self.vbr,
            _ => self.ctl.get(&rc).copied().unwrap_or(0),
        }
    }

    // -- line 5: ADDQ, SUBQ, Scc, TPF --------------------------------------

    pub(crate) fn line5(&mut self, op: u16) {
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        if (op >> 6) & 3 == 3 {
            match op & 0x3F {
                0x3A => {
                    self.pc = self.pc.wrapping_add(2);
                    return;
                }
                0x3B => {
                    self.pc = self.pc.wrapping_add(4);
                    return;
                }
                0x3C => return,
                _ => {}
            }
            if mode == 0 {
                let v = if self.cond((op >> 8) & 15) { 0xFF } else { 0 };
                let r = reg as usize;
                self.d[r] = (self.d[r] & !0xFF) | v;
            } else {
                self.illegal(VEC_ILLEGAL);
            }
            return;
        }
        let mut q = ((op >> 9) & 7) as u32;
        if q == 0 {
            q = 8;
        }
        let sz = [1, 2, 4][((op >> 6) & 3) as usize];
        if mode == 1 {
            let r = reg as usize;
            self.a[r] = if op & 0x100 != 0 { self.a[r].wrapping_sub(q) } else { self.a[r].wrapping_add(q) };
            return;
        }
        let l = self.ea(mode, reg, sz);
        let dv = self.read_loc(l, sz);
        let res = if op & 0x100 != 0 {
            self.sub_flags(q, dv, sz, false)
        } else {
            self.add_flags(q, dv, sz, false)
        };
        self.write_loc(l, sz, res);
    }

    // -- line 6: branches -------------------------------------------------

    pub(crate) fn line6(&mut self, op: u16) {
        let base = self.pc;
        let d8 = op & 0xFF;
        let disp = match d8 {
            0 => self.fetch16() as i16 as i32 as u32,
            0xFF => self.fetch32(),
            _ => d8 as u8 as i8 as i32 as u32,
        };
        let cc = (op >> 8) & 15;
        let target = base.wrapping_add(disp);
        match cc {
            0 => self.pc = target,
            1 => {
                let ret = self.pc;
                self.push32(ret);
                self.pc = target;
            }
            _ => {
                if self.cond(cc) {
                    self.pc = target;
                }
            }
        }
    }

    // -- line 7: MOVEQ, MVS, MVZ --------------------------------------------

    pub(crate) fn line7(&mut self, op: u16) {
        let dn = ((op >> 9) & 7) as usize;
        if op & 0x100 == 0 {
            let v = op as u8 as i8 as i32 as u32;
            self.d[dn] = v;
            self.set_nz(v, 4);
            return;
        }
        let ss = (op >> 6) & 3;
        let sz = if ss & 1 == 0 { 1 } else { 2 };
        let l = self.ea((op >> 3) & 7, op & 7, sz);
        let v = self.read_loc(l, sz);
        let v = if ss < 2 { sext(v, sz) } else { v };
        self.d[dn] = v;
        self.set_nz(v, 4);
    }

    // -- line 8: OR, DIVU.W, DIVS.W -----------------------------------------

    pub(crate) fn line8(&mut self, op: u16) {
        let dn = ((op >> 9) & 7) as usize;
        let opmode = (op >> 6) & 7;
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        match opmode {
            3 | 7 => {
                let l = self.ea(mode, reg, 2);
                let s = self.read_loc(l, 2);
                self.div_w(dn, s, opmode == 7);
            }
            0..=2 => {
                let sz = [1, 2, 4][opmode as usize];
                let l = self.ea(mode, reg, sz);
                let v = self.read_loc(l, sz) | self.d[dn];
                self.write_loc(Loc::D(dn), sz, v);
                self.set_nz(v, sz);
            }
            _ => {
                let sz = [1, 2, 4][(opmode - 4) as usize];
                let l = self.ea(mode, reg, sz);
                let v = self.read_loc(l, sz) | self.d[dn];
                self.write_loc(l, sz, v);
                self.set_nz(v, sz);
            }
        }
    }

    pub(crate) fn div_w(&mut self, dn: usize, s: u32, signed: bool) {
        let s16 = s & 0xFFFF;
        if s16 == 0 {
            let pc = self.pc;
            self.exception(VEC_DIVZERO, pc, None);
            return;
        }
        let dv = self.d[dn];
        let (q, r, over) = if signed {
            let (a, b) = (dv as i32 as i64, s16 as u16 as i16 as i64);
            let q = a / b;
            let r = a % b;
            (q as u32, r as u32, !(-32768..=32767).contains(&q))
        } else {
            let q = dv / s16;
            (q, dv % s16, q > 0xFFFF)
        };
        if over {
            self.div_overflow();
            return;
        }
        self.d[dn] = (r << 16) | (q & 0xFFFF);
        self.set_nz(q, 2);
    }

    // -- lines 9 and D: SUB/ADD family ---------------------------------------

    pub(crate) fn line9d(&mut self, op: u16, add: bool) {
        let dn = ((op >> 9) & 7) as usize;
        let opmode = (op >> 6) & 7;
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        match opmode {
            3 | 7 => {
                // ADDA/SUBA
                let sz = if opmode == 3 { 2 } else { 4 };
                let l = self.ea(mode, reg, sz);
                let s = sext(self.read_loc(l, sz), sz);
                self.a[dn] = if add { self.a[dn].wrapping_add(s) } else { self.a[dn].wrapping_sub(s) };
            }
            0..=2 => {
                let sz = [1, 2, 4][opmode as usize];
                let l = self.ea(mode, reg, sz);
                let s = self.read_loc(l, sz);
                let d = self.d[dn];
                let r = if add { self.add_flags(s, d, sz, false) } else { self.sub_flags(s, d, sz, false) };
                self.write_loc(Loc::D(dn), sz, r);
            }
            _ => {
                let sz = [1, 2, 4][(opmode - 4) as usize];
                if mode == 0 {
                    // ADDX/SUBX Dy,Dx
                    let ry = reg as usize;
                    let x = self.flag(CF_X);
                    let z = self.sr & CF_Z;
                    let (s, d) = (self.d[ry], self.d[dn]);
                    let r = if add { self.add_flags(s, d, sz, x) } else { self.sub_flags(s, d, sz, x) };
                    if r & sz_mask(sz) == 0 {
                        self.sr = (self.sr & !CF_Z) | z;
                    }
                    self.write_loc(Loc::D(dn), sz, r);
                    return;
                }
                if mode == 1 {
                    return self.illegal(VEC_ILLEGAL);
                }
                let l = self.ea(mode, reg, sz);
                let d = self.read_loc(l, sz);
                let s = self.d[dn];
                let r = if add { self.add_flags(s, d, sz, false) } else { self.sub_flags(s, d, sz, false) };
                self.write_loc(l, sz, r);
            }
        }
    }

    // -- line B: CMP, CMPA, EOR ----------------------------------------------

    pub(crate) fn lineb(&mut self, op: u16) {
        let dn = ((op >> 9) & 7) as usize;
        let opmode = (op >> 6) & 7;
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        match opmode {
            0..=2 => {
                let sz = [1, 2, 4][opmode as usize];
                let l = self.ea(mode, reg, sz);
                let s = self.read_loc(l, sz);
                let d = self.d[dn];
                self.cmp_flags(s, d, sz);
            }
            3 | 7 => {
                let sz = if opmode == 3 { 2 } else { 4 };
                let l = self.ea(mode, reg, sz);
                let s = sext(self.read_loc(l, sz), sz);
                let d = self.a[dn];
                self.cmp_flags(s, d, 4);
            }
            _ => {
                let sz = [1, 2, 4][(opmode - 4) as usize];
                let l = self.ea(mode, reg, sz);
                let v = self.read_loc(l, sz) ^ self.d[dn];
                self.write_loc(l, sz, v);
                self.set_nz(v, sz);
            }
        }
    }

    // -- line C: AND, MULU.W, MULS.W -----------------------------------------

    pub(crate) fn linec(&mut self, op: u16) {
        let dn = ((op >> 9) & 7) as usize;
        let opmode = (op >> 6) & 7;
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        match opmode {
            3 | 7 => {
                let l = self.ea(mode, reg, 2);
                let s = self.read_loc(l, 2);
                let r = if opmode == 7 {
                    (self.d[dn] as u16 as i16 as i32).wrapping_mul(s as u16 as i16 as i32) as u32
                } else {
                    (self.d[dn] & 0xFFFF).wrapping_mul(s & 0xFFFF)
                };
                self.d[dn] = r;
                self.set_nz(r, 4);
            }
            0..=2 => {
                let sz = [1, 2, 4][opmode as usize];
                let l = self.ea(mode, reg, sz);
                let v = self.read_loc(l, sz) & self.d[dn];
                self.write_loc(Loc::D(dn), sz, v);
                self.set_nz(v, sz);
            }
            _ => {
                let sz = [1, 2, 4][(opmode - 4) as usize];
                let l = self.ea(mode, reg, sz);
                let v = self.read_loc(l, sz) & self.d[dn];
                self.write_loc(l, sz, v);
                self.set_nz(v, sz);
            }
        }
    }

    // -- line E: shifts (register, long only on ColdFire) --------------------

    pub(crate) fn linee(&mut self, op: u16) {
        let r = (op & 7) as usize;
        let cnt_field = ((op >> 9) & 7) as u32;
        let count = if op & 0x20 != 0 {
            self.d[cnt_field as usize] & 63
        } else if cnt_field == 0 {
            8
        } else {
            cnt_field
        };
        let left = op & 0x100 != 0;
        let kind = (op >> 3) & 3;
        if (op >> 6) & 3 != 2 || kind > 1 {
            return self.illegal(VEC_ILLEGAL);
        }
        let v = self.d[r];
        let mut ccr = self.sr & !0x1F;
        let res;
        if count == 0 {
            res = v;
            ccr |= self.sr & CF_X;
        } else if left {
            res = if count >= 32 { 0 } else { v << count };
            let c = if count > 32 { false } else { (v >> (32 - count)) & 1 != 0 };
            if c {
                ccr |= CF_C | CF_X;
            }
        } else if kind == 1 {
            // LSR
            res = if count >= 32 { 0 } else { v >> count };
            let c = if count > 32 { false } else { (v >> (count - 1)) & 1 != 0 };
            if c {
                ccr |= CF_C | CF_X;
            }
        } else {
            // ASR
            let sv = v as i32;
            res = if count >= 32 { (sv >> 31) as u32 } else { (sv >> count) as u32 };
            let c = if count >= 32 { sv < 0 } else { (v >> (count - 1)) & 1 != 0 };
            if c {
                ccr |= CF_C | CF_X;
            }
        }
        if res == 0 {
            ccr |= CF_Z;
        }
        if res & 0x8000_0000 != 0 {
            ccr |= CF_N;
        }
        self.sr = ccr;
        self.d[r] = res;
    }

    // -- line F: cache maintenance, debug, and the missing FPU ---------------

    pub(crate) fn linef(&mut self, op: u16) {
        match op & 0xFF00 {
            0xF400 => {
                // CPUSHL dc/ic/bc (An) and INTOUCH (An): the cache model's
                // (cache.rs), a no-op without it.
                if !self.supervisor() {
                    return self.illegal(VEC_PRIV);
                }
                if self.bus.cache.is_some() && op & 0x38 == 0x28 {
                    let an = self.a[(op & 7) as usize];
                    self.bus.cache_op(((op >> 6) & 3) as u8, an);
                }
            }
            0xFB00 => {
                // WDDATA / WDEBUG: compute the EA for its length only.
                let mode = (op >> 3) & 7;
                let reg = op & 7;
                if op & 0x00C0 == 0x00C0 {
                    let _ = self.ea_addr(mode, reg);
                    self.pc = self.pc.wrapping_add(2);
                } else {
                    let sz = [1, 2, 4, 4][((op >> 6) & 3) as usize];
                    let _ = self.ea(mode, reg, sz);
                }
            }
            _ => self.illegal(VEC_LINEF),
        }
    }

    // -- line A: EMAC and MOV3Q ----------------------------------------------

    /// Physical (ACCn, ACCextn) of a 48-bit value under MACSR `m`.
    fn phys_of(v: i64, m: u32) -> (u32, u16) {
        let raw = v as u64;
        if m & MACSR_FI != 0 {
            ((raw >> 8) as u32, ((((raw >> 40) & 0xFF) << 8) | (raw & 0xFF)) as u16)
        } else {
            (raw as u32, ((((raw >> 40) & 0xFF) << 8) | ((raw >> 32) & 0xFF)) as u16)
        }
    }

    /// The 48-bit value of physical (ACCn, ACCextn) under MACSR `m`.
    fn value_of(acc: u32, ext: u16, m: u32) -> i64 {
        let acc = acc as u64;
        let hi = (ext >> 8) as u64 & 0xFF;
        let lo = ext as u64 & 0xFF;
        if m & MACSR_FI != 0 {
            let raw = (hi << 40) | (acc << 8) | lo;
            ((raw << 16) as i64) >> 16
        } else {
            let raw = (hi << 40) | (lo << 32) | acc;
            if m & MACSR_SU == 0 {
                ((raw << 16) as i64) >> 16
            } else {
                raw as i64
            }
        }
    }

    /// The physical ACCn and ACCextn.
    pub fn acc_phys(&self, i: usize) -> (u32, u16) {
        Self::phys_of(self.accv[i], self.macsr)
    }

    pub fn set_acc_phys(&mut self, i: usize, acc: u32, ext: u16) {
        self.accv[i] = Self::value_of(acc, ext, self.macsr);
    }

    /// Write MACSR: the physical registers stay, their meaning changes.
    pub fn set_macsr(&mut self, v: u32) {
        let old = self.macsr;
        let new = v & 0xFFF;
        if (old ^ new) & (MACSR_FI | MACSR_SU) != 0 {
            for i in 0..4 {
                let (a, e) = Self::phys_of(self.accv[i], old);
                self.accv[i] = Self::value_of(a, e, new);
            }
        }
        self.macsr = new;
    }

    #[inline(always)]
    pub fn acc_get(&self, i: usize) -> i64 {
        self.accv[i]
    }

    /// Store the low 48 bits of `v`, extended as the mode reads them.
    #[inline(always)]
    pub fn acc_set(&mut self, i: usize, v: i64) {
        self.accv[i] = if self.macsr & (MACSR_FI | MACSR_SU) == MACSR_SU {
            v & ((1i64 << 48) - 1)
        } else {
            (v << 16) >> 16
        };
    }

    fn mac_signed_int(&self) -> bool {
        self.macsr & (MACSR_FI | MACSR_SU) == 0
    }

    fn mac_word(&self, v: u32, upper: bool) -> u32 {
        if self.macsr & MACSR_FI != 0 {
            if upper { v & 0xFFFF_0000 } else { v << 16 }
        } else if self.mac_signed_int() {
            if upper { ((v as i32) >> 16) as u32 } else { v as u16 as i16 as i32 as u32 }
        } else if upper {
            v >> 16
        } else {
            v & 0xFFFF
        }
    }

    fn mac_product(&mut self, x: u32, y: u32) -> i64 {
        if self.macsr & MACSR_FI != 0 {
            let p = (((x as i32 as i64) * (y as i32 as i64)) as u64) << 1;
            let rem = p & 0xFF_FFFF;
            let mut f = p >> 24;
            if self.macsr & MACSR_RT != 0 && (rem > 0x80_0000 || (rem == 0x80_0000 && f & 1 != 0)) {
                f = f.wrapping_add(1);
            }
            f &= (1u64 << 40) - 1;
            if x == 0x8000_0000 && y == 0x8000_0000 {
                return f as i64;
            }
            ((f << 24) as i64) >> 24
        } else if self.mac_signed_int() {
            let p = (x as i32 as i64) * (y as i32 as i64);
            let res = (p << 24) >> 24;
            if res != p {
                self.macsr |= MACSR_V;
                if self.macsr & MACSR_OMC != 0 {
                    return if p < 0 { !(1i64 << 50) } else { 1i64 << 50 };
                }
            }
            res
        } else {
            let mut p = (x as u64) * (y as u64);
            if p & (0xFF_FFFFu64 << 40) != 0 {
                self.macsr |= MACSR_V;
                if self.macsr & MACSR_OMC != 0 {
                    p = 1u64 << 50;
                } else {
                    p &= (1u64 << 40) - 1;
                }
            }
            p as i64
        }
    }

    fn mac_saturate(&mut self, i: usize, sum: i64) -> i64 {
        let pav = MACSR_PAV0 << i;
        if self.macsr & MACSR_FI != 0 {
            let mut r = (sum << 16) >> 16;
            if r != sum {
                self.macsr |= MACSR_V;
            }
            if self.macsr & MACSR_V != 0 {
                self.macsr |= pav;
                if self.macsr & MACSR_OMC != 0 {
                    r = if sum < 0 { 0xFFFF_FF80_0000_0000u64 as i64 } else { 0x007F_FFFF_FF00 };
                }
            }
            r
        } else if self.mac_signed_int() {
            let mut r = (sum << 16) >> 16;
            if r != sum {
                self.macsr |= MACSR_V;
            }
            if self.macsr & MACSR_V != 0 {
                self.macsr |= pav;
                if self.macsr & MACSR_OMC != 0 {
                    r = if sum < 0 { -0x8000_0000 } else { 0x7FFF_FFFF };
                }
            }
            r
        } else {
            let mut v = sum as u64;
            if v & (0xFFFFu64 << 48) != 0 {
                self.macsr |= MACSR_V;
            }
            if self.macsr & MACSR_V != 0 {
                self.macsr |= pav;
                if self.macsr & MACSR_OMC != 0 {
                    v = if v > (1u64 << 53) { 0 } else { (1u64 << 48) - 1 };
                } else {
                    v &= (1u64 << 48) - 1;
                }
            }
            v as i64
        }
    }

    fn mac_set_flags(&mut self, i: usize) {
        let v = self.acc_get(i);
        if v == 0 {
            self.macsr |= MACSR_Z;
        } else if (v as u64) & (1u64 << 47) != 0 {
            self.macsr |= MACSR_N;
        }
        if self.macsr & (MACSR_PAV0 << i) != 0 {
            self.macsr |= MACSR_V;
        }
        let ev = if self.macsr & MACSR_FI != 0 {
            let t = v >> 39;
            t != 0 && t != -1
        } else if self.mac_signed_int() {
            let t = v >> 31;
            t != 0 && t != -1
        } else {
            (v as u64) >> 32 != 0
        };
        if ev {
            self.macsr |= MACSR_EV;
        }
    }

    fn mac_clear_flags(&mut self) {
        self.macsr &= !(MACSR_V | MACSR_Z | MACSR_N | MACSR_EV);
    }

    /// The value MOVE ACCn,Rx reads.
    pub(crate) fn mac_read(&self, i: usize) -> u32 {
        let v = self.acc_get(i);
        if self.macsr & MACSR_FI != 0 {
            if self.macsr & MACSR_SU != 0 {
                let rem = (v & 0xFF_FFFF) as u32;
                let mut q = v >> 24;
                if rem > 0x80_0000 || (rem == 0x80_0000 && q & 1 != 0) {
                    q += 1;
                }
                if self.macsr & MACSR_OMC != 0 && q != q as i16 as i64 {
                    q = if v < 0 { -0x8000 } else { 0x7FFF };
                }
                return (q as u32) & 0xFFFF;
            }
            let rem = (v & 0xFF) as u32;
            let mut q = v >> 8;
            if self.macsr & MACSR_RT != 0 && (rem > 0x80 || (rem == 0x80 && q & 1 != 0)) {
                q += 1;
            }
            if self.macsr & MACSR_OMC != 0 && q != q as i32 as i64 {
                q = if v < 0 { i32::MIN as i64 } else { i32::MAX as i64 };
            }
            return q as u32;
        }
        if self.macsr & MACSR_OMC == 0 {
            return v as u32;
        }
        if self.mac_signed_int() {
            if v == v as i32 as i64 {
                v as u32
            } else if v < 0 {
                0x8000_0000
            } else {
                0x7FFF_FFFF
            }
        } else if (v as u64) >> 32 == 0 {
            v as u32
        } else {
            0xFFFF_FFFF
        }
    }

    pub(crate) fn linea(&mut self, op: u16) {
        if op & 0x0100 == 0 {
            return self.mac(op);
        }
        let mode = (op >> 3) & 7;
        let reg = op & 7;
        if op & 0xF1C0 == 0xA140 {
            // MOV3Q #imm,<ea>
            let mut v = ((op >> 9) & 7) as u32;
            if v == 0 {
                v = 0xFFFF_FFFF;
            }
            let l = self.ea(mode, reg, 4);
            self.write_loc(l, 4, v);
            self.set_nz(v, 4);
            return;
        }
        if op == 0xA9C0 {
            // MOVE MACSR,CCR
            let m = self.macsr;
            let mut c = 0;
            if m & MACSR_N != 0 {
                c |= CF_N;
            }
            if m & MACSR_Z != 0 {
                c |= CF_Z;
            }
            if m & MACSR_V != 0 {
                c |= CF_V;
            }
            self.set_ccr(c);
            return;
        }
        let rx = |s: &Cpu, n: u16| -> u32 {
            let r = (n & 7) as usize;
            if n & 8 != 0 { s.a[r] } else { s.d[r] }
        };
        let set_rx = |s: &mut Cpu, n: u16, v: u32| {
            let r = (n & 7) as usize;
            if n & 8 != 0 {
                s.a[r] = v
            } else {
                s.d[r] = v
            }
        };
        if op & 0xF9B0 == 0xA180 {
            // MOVE.L ACCy,Rx / MOVCLR.L
            let i = ((op >> 9) & 3) as usize;
            let v = self.mac_read(i);
            set_rx(self, op & 0xF, v);
            if op & 0x40 != 0 {
                self.accv[i] = 0;
                self.macsr &= !(MACSR_PAV0 << i);
            }
            return;
        }
        if op & 0xF9FC == 0xA110 {
            // MOVE.L ACCy,ACCx
            let src = (op & 3) as usize;
            let dst = ((op >> 9) & 3) as usize;
            self.accv[dst] = self.accv[src];
            self.mac_clear_flags();
            let pav_src = self.macsr & (MACSR_PAV0 << src) != 0;
            self.macsr &= !(MACSR_PAV0 << dst);
            if pav_src {
                self.macsr |= MACSR_PAV0 << dst;
            }
            self.mac_set_flags(dst);
            return;
        }
        if op & 0xFFF0 == 0xA980 {
            let v = self.macsr;
            set_rx(self, op & 0xF, v);
            return;
        }
        if op & 0xFFF0 == 0xAD80 {
            let v = self.mask;
            set_rx(self, op & 0xF, v);
            return;
        }
        if op & 0xFBF0 == 0xAB80 {
            // MOVE.L ACCext01/23,Rx
            let i = if op & 0x400 != 0 { 2 } else { 0 };
            let v = ((self.acc_phys(i).1 as u32) << 16) | self.acc_phys(i + 1).1 as u32;
            set_rx(self, op & 0xF, v);
            return;
        }
        // The rest take a source EA: Dn, An or #imm.
        let l = self.ea(mode, reg, 4);
        let val = self.read_loc(l, 4);
        let _ = rx;
        if op & 0xF9C0 == 0xA100 {
            // MOVE.L <ea>,ACCx
            let i = ((op >> 9) & 3) as usize;
            let v = if self.macsr & MACSR_FI != 0 {
                (val as i32 as i64) << 8
            } else if self.mac_signed_int() {
                val as i32 as i64
            } else {
                val as i64
            };
            self.acc_set(i, v);
            self.macsr &= !(MACSR_PAV0 << i);
            self.mac_clear_flags();
            self.mac_set_flags(i);
        } else if op & 0xFFC0 == 0xA900 {
            self.set_macsr(val);
        } else if op & 0xFBC0 == 0xAB00 {
            let i = if op & 0x400 != 0 { 2 } else { 0 };
            let (a0, _) = self.acc_phys(i);
            let (a1, _) = self.acc_phys(i + 1);
            self.set_acc_phys(i, a0, (val >> 16) as u16);
            self.set_acc_phys(i + 1, a1, val as u16);
        } else if op & 0xFFC0 == 0xAD00 {
            self.mask = val | 0xFFFF_0000;
        } else {
            self.illegal(VEC_LINEA);
        }
    }

    /// MAC / MSAC, with or without a parallel load.
    fn mac(&mut self, op: u16) {
        let ext = self.fetch16();
        let disp = if op & 0x30 != 0 && (op >> 3) & 7 == 5 { self.fetch16() as i16 as i32 as u32 } else { 0 };
        self.mac_core(op, ext, disp);
    }

    /// The body of MAC/MSAC with its extension word (and, for a (d16,An)
    /// load, the displacement) already fetched.
    pub fn mac_core(&mut self, op: u16, ext: u16, disp: u32) {
        let mut acc = (((op >> 7) & 1) | ((ext >> 3) & 2)) as usize;
        let load = op & 0x30 != 0;
        let (rx, ry, loadval, addr) = if load {
            let mode = (op >> 3) & 7;
            let reg = (op & 7) as usize;
            // The EA without (An)+/-(An) side effects; applied after the MAC.
            let base = match mode {
                2 | 3 => self.a[reg],
                4 => self.a[reg].wrapping_sub(4),
                5 => self.a[reg].wrapping_add(disp),
                _ => return self.illegal(VEC_LINEA),
            };
            let a = if ext & 0x20 != 0 { base & self.mask } else { base };
            let v = self.bus.read32(a);
            acc ^= 1;
            let rxn = ((ext >> 12) & 7) as usize;
            let rx = if ext & 0x8000 != 0 { self.a[rxn] } else { self.d[rxn] };
            let ryn = (ext & 7) as usize;
            let ry = if ext & 8 != 0 { self.a[ryn] } else { self.d[ryn] };
            (rx, ry, v, a)
        } else {
            let rxn = ((op >> 9) & 7) as usize;
            let rx = if op & 0x40 != 0 { self.a[rxn] } else { self.d[rxn] };
            let ryn = (op & 7) as usize;
            let ry = if op & 8 != 0 { self.a[ryn] } else { self.d[ryn] };
            (rx, ry, 0, 0)
        };
        let skip = self.macsr & MACSR_OMC != 0 && self.macsr & (MACSR_PAV0 << acc) != 0;
        if !skip {
            let (x, y) = if ext & 0x0800 == 0 {
                (self.mac_word(rx, ext & 0x80 != 0), self.mac_word(ry, ext & 0x40 != 0))
            } else {
                (rx, ry)
            };
            self.mac_clear_flags();
            let mut p = self.mac_product(x, y);
            // The scale factor applies in integer modes only (RM 5.3.5).
            let scale = if self.macsr & MACSR_FI != 0 { 0 } else { (ext >> 9) & 3 };
            match scale {
                1 => p = p.wrapping_shl(1),
                3 => {
                    p = if self.macsr & MACSR_SU != 0 && self.macsr & MACSR_FI == 0 {
                        ((p as u64) >> 1) as i64
                    } else {
                        p >> 1
                    }
                }
                _ => {}
            }
            let cur = self.acc_get(acc);
            let sum = if ext & 0x100 != 0 { cur.wrapping_sub(p) } else { cur.wrapping_add(p) };
            let r = self.mac_saturate(acc, sum);
            self.acc_set(acc, r);
            self.mac_set_flags(acc);
        }
        if load {
            let rw = ((op >> 9) & 7) as usize;
            if op & 0x40 != 0 {
                self.a[rw] = loadval;
            } else {
                self.d[rw] = loadval;
            }
            let reg = (op & 7) as usize;
            match (op >> 3) & 7 {
                3 => self.a[reg] = addr.wrapping_add(4),
                4 => self.a[reg] = addr,
                _ => {}
            }
        }
    }
}
