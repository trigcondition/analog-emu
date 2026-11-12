//! Unit timing (`DTEMU_TIMING=unit`): the clock counts core cycles, not
//! instructions, so the CPU load the firmware sees (analoghealth's CPU %,
//! DSP %, the UI tick rate) follows the unit's instead of an ideal one
//! instruction per tick. docs/TIMING.md has the sources and the fit.
//!
//! The default mode is untouched: one tick per instruction, 200 M a second.
//! In unit mode:
//! - `Io::ips` is the core clock, so every peripheral still converts through
//!   it and keeps its real rate. The default is 2 x the 132 MHz bus clock the
//!   PIT rates establish (io.rs), as the MCF5441x runs its bus at fsys/2.
//! - Instruction costs come from the MCF54418 reference manual (Rev. 8,
//!   section 3.3.5, tables 3-13 to 3-20): one cycle for most instructions,
//!   including a memory operand and a read-modify-write; +1 for the
//!   (d8,An,Xi*SF) mode and for a memory-to-memory MOVE; MUL 4, DIV.W 20,
//!   DIV.L and REM 35, MOVEM n, MOVEC 20, NOP 6, RTE 15, JSR/JMP (An) 5, ...
//! - The load-use stall of section 3.3.5.1: an address generated from a
//!   register the previous instruction changed in the execute stage waits
//!   2 cycles (base, or index scaled by 1) or 3 (index scaled by 2 or 4).
//! - Branches: the 8-entry branch cache and the 128-entry prediction table
//!   with 2-bit counters (section 3.1.1.1): a taken branch the branch cache
//!   predicts costs 0, other correct predictions 1, a misprediction 8. RTS
//!   uses the 4-entry return stack: 2 predicted, 9 mispredicted, 8 empty.
//! - EMAC: an accumulator store right after a MAC, MSAC or accumulator load
//!   takes 4 cycles; MOVE to MACSR 8, to MASK 7.
//! - Memory: tag-only 8 KB 4-way I- and D-caches (section 6.2; `Tags`,
//!   following CACR and the ACRs; data stays in the bus). The manual's times assume zero-wait
//!   memory, so each D-cache line fill, dirty push and I-cache line fill
//!   costs a DDR transfer (`dmiss`, `wb`, `imiss`); a data access past the
//!   caches costs `unc_rd`/`unc_wr` (DDR through the uncached alias, which
//!   the stock CACR makes inhibited-precise: a write holds the pipeline),
//!   `sram` or `io` (peripheral registers on the fsys/2 bus). These memory
//!   numbers are not in the manual; they are fitted to readings from a unit.
//! - An interrupt or exception entry costs `exc` (TRAP's 18 in table 3-17).
//!
//! Not modelled: MOVE folding (two instructions issued together, "fold"
//! is a fitted fraction instead), the IFP/OEP decoupling that makes BRA,
//! BSR and absolute JSR take 1-3 cycles (taken as 2), stalls from other
//! register dependencies the manual doesn't list, DDR banks and refresh,
//! and bus contention with eDMA. The aim is the load within a few points.
//!
//! Sampled mode (`DTEMU_TIMING=unit`, the default) measures all of the
//! above for `window_us` of every `period_us` (gaps jittered so windows do
//! not lock onto the firmware's own periods), learns each predecoded
//! block's cost, and charges the learned cost between windows: about real
//! time. `DTEMU_TIMING=exact` measures throughout, about 3x slower.

use serde::{Deserialize, Serialize};

/// Cost parameters, in core cycles.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Params {
    /// Core clock, Hz.
    pub hz: f64,
    /// Fraction of MOVEs issued for free with the instruction before them.
    pub fold: f64,
    /// Exception or interrupt entry.
    pub exc: u32,
    /// Branch misprediction (manual: 8).
    pub mispredict: u32,
    /// D-cache line fill from DDR.
    pub dmiss: u32,
    /// Dirty D-cache line pushed to DDR (mostly hidden by the push buffer).
    pub wb: u32,
    /// I-cache line fill from DDR after a change of flow.
    pub imiss: u32,
    /// I-cache line fill of the line after the last one fetched: the
    /// fetch pipeline (IFP) has started it ahead, so less of it shows.
    pub imiss_seq: u32,
    /// Data read from DDR past the D-cache (uncached alias).
    pub unc_rd: u32,
    /// Data write to DDR past the D-cache (precise: holds the pipeline).
    pub unc_wr: u32,
    /// An access to internal SRAM (single-cycle: 0 extra).
    pub sram: u32,
    /// An access to a peripheral register.
    pub io: u32,
    /// Sampled mode: measure every instruction and access for `window_us`
    /// of each `period_us` of emulated time, and charge each block's
    /// learned cost the rest of the time.
    pub window_us: f64,
    pub period_us: f64,
    /// The share of each window spent warming the cache tags (their state
    /// is stale after a gap) before learning starts.
    pub warm: f64,
}

impl Default for Params {
    fn default() -> Params {
        Params {
            hz: 2.0 * crate::io::F_BUS,
            fold: 0.0,
            exc: 17,
            mispredict: 8,
            dmiss: 42,
            wb: 4,
            imiss: 42,
            imiss_seq: 42,
            unc_rd: 38,
            unc_wr: 8,
            sram: 0,
            io: 10,
            window_us: 2000.0,
            period_us: 20000.0,
            warm: 0.5,
        }
    }
}

impl Params {
    /// Apply `key=value,...` over these values. Keys this model doesn't know
    /// are refused, except those of other models sharing the variable
    /// (`emmc_*`), which they parse themselves.
    pub fn apply(&mut self, s: &str) -> Result<(), String> {
        for kv in s.split(',').map(str::trim).filter(|kv| !kv.is_empty()) {
            let (k, v) = kv.split_once('=').ok_or_else(|| format!("timing param `{kv}`: want key=value"))?;
            let k = k.trim();
            if k.starts_with("emmc_") {
                continue;
            }
            let f: f64 = v.trim().parse().map_err(|_| format!("timing param `{kv}`: not a number"))?;
            let u = || -> Result<u32, String> {
                if f < 0.0 || f.fract() != 0.0 {
                    Err(format!("timing param `{kv}`: want a whole number of cycles"))
                } else {
                    Ok(f as u32)
                }
            };
            match k {
                "hz" => self.hz = f,
                "fold" => self.fold = f,
                "exc" => self.exc = u()?,
                "mispredict" => self.mispredict = u()?,
                "dmiss" => self.dmiss = u()?,
                "wb" => self.wb = u()?,
                "imiss" => self.imiss = u()?,
                "imiss_seq" => self.imiss_seq = u()?,
                "unc_rd" => self.unc_rd = u()?,
                "unc_wr" => self.unc_wr = u()?,
                "sram" => self.sram = u()?,
                "io" => self.io = u()?,
                "window_us" => self.window_us = f,
                "period_us" => self.period_us = f,
                "warm" => self.warm = f,
                other => return Err(format!("timing param `{other}`: unknown")),
            }
        }
        if self.hz <= 0.0 || !(0.0..=1.0).contains(&self.fold) {
            return Err("timing params: hz must be > 0 and fold within 0..1".into());
        }
        if !(self.window_us > 0.0 && self.window_us <= self.period_us) {
            return Err("timing params: want 0 < window_us <= period_us".into());
        }
        Ok(())
    }
}

/// Where the cycles went, for the end-of-run summary and the fit.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stats {
    /// Peripheral accesses per 16 KB page (for `summary`).
    #[serde(skip)]
    pub io_pages: std::collections::BTreeMap<u32, u64>,
    pub instructions: u64,
    /// Cycles charged from learned block costs (sampled mode).
    pub learned: u64,
    /// Sampled mode: block runs between windows with no learned cost, and all ops run there.
    pub cold_runs: u64,
    pub gap_ops: u64,
    /// Table cycles beyond one per instruction.
    pub table: u64,
    pub stall: u64,
    /// Branch and return-stack cycles beyond one (negative: folded branches).
    pub branch: i64,
    pub mispredicts: u64,
    pub folded: u64,
    pub exc: u64,
    pub dmiss: u64,
    pub wb: u64,
    pub imiss: u64,
    pub unc: u64,
    pub sram: u64,
    pub io: u64,
}

/// What `post` needs to know about the instruction `pre` just costed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    None,
    /// A conditional branch at this PC, falling through to `next`.
    Bcc { pc: u32, next: u32 },
    /// BSR or JSR: the return address is on the stack after it runs.
    Call,
    Rts,
}

const NONE: u8 = 0xFF;
const MEMO: usize = 1 << 15;

/// An instruction's static timing (`Timing::decode`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decoded {
    /// Table cycles beyond one.
    pub x: u16,
    /// The register changed in the execute stage (0-7 D, 8-15 A), or NONE.
    pub dst: u8,
    /// 0 none, 1 call (BSR/JSR), 2 RTS, 3 Bcc.
    pub flow: u8,
    /// Bcc length in bytes.
    pub len: u8,
    pub is_move: bool,
    pub mac: bool,
    /// An EMAC register store: 3 more cycles right after a MAC.
    pub mac_rd: bool,
    /// Address registers read: (base, index, the index's stall), NONE if unused.
    pub uses: [(u8, u8, u8); 2],
}

/// Record the registers <ea> (mode, reg) reads to form its address.
fn use_ea(u: &mut (u8, u8, u8), mode: u16, reg: u16, ext: u16) {
    if (2..=6).contains(&mode) {
        u.0 = 8 + reg as u8;
    }
    if is_indexed(mode, reg) {
        u.1 = (ext >> 12) as u8 & 15;
        u.2 = if (ext >> 9) & 3 == 0 { 2 } else { 3 };
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Timing {
    pub p: Params,
    pub stats: Stats,
    /// The register (0-7 D, 8-15 A) the previous instruction changed in
    /// the execute stage, or NONE.
    last_dst: u8,
    /// The previous instruction was a MAC, MSAC or accumulator load.
    last_mac: bool,
    /// The previous instruction was folded (a pair issues once).
    last_folded: bool,
    fold_acc: f64,
    /// Branch cache: 8 direct-mapped entries (tag PC, 2-bit counter).
    bc: [(u32, u8); 8],
    /// Prediction table: 128 direct-mapped 2-bit counters.
    pt: Vec<u8>,
    /// Return stack, 4 entries (LIFO, the oldest dropped).
    rs: Vec<u32>,
    /// Tag-only I- and D-caches: hits and misses, no data (memory stays
    /// the emulator's plain memory, as in the default mode).
    pub tags: Tags,
    #[serde(skip, default = "memo_keys")]
    memo_key: Vec<(u32, u16, u16)>,
    #[serde(skip, default = "memo_vals")]
    memo: Vec<Decoded>,
    /// The I-cache line of the last fetch (a fetch in the same line hits).
    #[serde(skip)]
    last_iline: u32,
    /// Sampled mode: measuring now, and the clock of the next switch.
    pub measuring: bool,
    pub next_switch: u64,
    /// Learning starts this far into a window (the tags were not kept up
    /// to date outside it).
    pub learn_from: u64,
    /// Average cycles beyond one per instruction in the last window, for
    /// code without a learned block cost; and this window's sums.
    pub g_extra: f64,
    win_extra: u64,
    win_ops: u64,
    /// First measured runs of blocks, over all windows: the cost for a
    /// block not learned yet (rare code runs cold, above the window average).
    cold_extra: u64,
    cold_ops: u64,
    pub g_cold: f64,
    /// Fractional cycles carried between learned charges.
    frac: f64,
    rng: u64,
    /// `DTEMU_TIMING_FN=ADDR`: time every call of the function at ADDR,
    /// callees included (entry clock and A7, then calls and total cycles).
    #[serde(skip)]
    pub probe: Option<Probe>,
}

#[derive(Clone, Debug, Default)]
pub struct Probe {
    pub addr: u32,
    pub open: Option<(u64, u32)>,
    pub calls: u64,
    pub cycles: u64,
}

impl Probe {
    /// Called before each instruction with its PC, the clock and A7.
    #[inline]
    pub fn step(&mut self, pc: u32, now: u64, sp: u32) {
        if let Some((t0, sp0)) = self.open {
            // Returned: A7 is above the return address the call pushed.
            if sp > sp0 {
                self.calls += 1;
                self.cycles += now - t0;
                self.open = None;
            }
        } else if pc == self.addr {
            self.open = Some((now, sp));
        }
    }
}

fn memo_keys() -> Vec<(u32, u16, u16)> {
    vec![(u32::MAX, 0, 0); MEMO]
}

fn memo_vals() -> Vec<Decoded> {
    vec![Decoded::default(); MEMO]
}

const LINE: u32 = 16;
const WAYS: usize = 4;
const CACR_DEC: u32 = 0x8000_0000;
const CACR_DPI: u32 = 0x1000_0000;
const CACR_DDCM: u32 = 0x0600_0000;
const CACR_DCINVA: u32 = 0x0100_0000;
const CACR_IEC: u32 = 0x0000_8000;
const CACR_IDPI: u32 = 0x0000_1000;
const CACR_IDCM: u32 = 0x0000_0400;
const CACR_ICINVA: u32 = 0x0000_0100;

fn acr_match(acr: u32, a: u32) -> bool {
    if acr & 0x8000 == 0 {
        return false;
    }
    let base = acr >> 24;
    let mask = (acr >> 16) & 0xFF;
    ((a >> 24) & !mask & 0xFF) == (base & !mask)
}

/// The caches' tags (cache.rs's geometry and modes, without the data).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tags {
    sets: usize,
    pub cacr: u32,
    pub acr: [u32; 4],
    /// (line address | 1 valid | 2 dirty) per way; line addresses are
    /// 16-aligned, so the low bits are free.
    d: Vec<u32>,
    i: Vec<u32>,
    d_rr: Vec<u8>,
    i_rr: Vec<u8>,
    pub d_hits: u64,
    pub d_misses: u64,
    pub d_pushes: u64,
    pub i_hits: u64,
    pub i_misses: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DMode {
    WriteThrough,
    CopyBack,
    Inhibited,
}

impl Tags {
    pub fn new(kb: usize) -> Tags {
        let sets = kb * 1024 / (LINE as usize * WAYS);
        Tags {
            sets,
            cacr: 0,
            acr: [0; 4],
            d: vec![0; sets * WAYS],
            i: vec![0; sets * WAYS],
            d_rr: vec![0; sets],
            i_rr: vec![0; sets],
            d_hits: 0,
            d_misses: 0,
            d_pushes: 0,
            i_hits: 0,
            i_misses: 0,
        }
    }

    pub fn configure(&mut self, cacr: u32, acr: [u32; 4]) {
        self.cacr = cacr;
        self.acr = acr;
        self.d.iter_mut().chain(self.i.iter_mut()).for_each(|l| *l = 0);
    }

    pub fn set_cacr(&mut self, v: u32) {
        self.cacr = v;
        if v & CACR_DCINVA != 0 {
            self.d.iter_mut().for_each(|l| *l = 0);
        }
        if v & CACR_ICINVA != 0 {
            self.i.iter_mut().for_each(|l| *l = 0);
        }
    }

    fn data_mode(&self, a: u32) -> DMode {
        if self.cacr & CACR_DEC == 0 {
            return DMode::Inhibited;
        }
        let cm = if acr_match(self.acr[0], a) {
            (self.acr[0] >> 5) & 3
        } else if acr_match(self.acr[1], a) {
            (self.acr[1] >> 5) & 3
        } else {
            (self.cacr & CACR_DDCM) >> 25
        };
        match cm {
            0 => DMode::WriteThrough,
            1 => DMode::CopyBack,
            _ => DMode::Inhibited,
        }
    }

    fn insn_cached(&self, a: u32) -> bool {
        if self.cacr & CACR_IEC == 0 {
            return false;
        }
        let cm = if acr_match(self.acr[2], a) {
            (self.acr[2] >> 5) & 3
        } else if acr_match(self.acr[3], a) {
            (self.acr[3] >> 5) & 3
        } else if self.cacr & CACR_IDCM != 0 {
            2
        } else {
            0
        };
        cm < 2
    }

    #[inline(always)]
    fn set_of(&self, la: u32) -> usize {
        ((la / LINE) as usize) & (self.sets - 1)
    }

    /// -> (hit way index, or the victim's index and whether it was dirty).
    #[inline(always)]
    fn lookup(lines: &mut [u32], rr: &mut [u8], set: usize, la: u32) -> (bool, usize, bool) {
        let base = set * WAYS;
        for w in 0..WAYS {
            let l = lines[base + w];
            if l & 1 != 0 && l & !0xF == la {
                return (true, base + w, false);
            }
        }
        let w = match (0..WAYS).find(|&w| lines[base + w] & 1 == 0) {
            Some(w) => w,
            None => {
                let w = rr[set] as usize;
                rr[set] = ((w + 1) % WAYS) as u8;
                w
            }
        };
        let dirty = lines[base + w] & 3 == 3;
        lines[base + w] = la | 1;
        (false, base + w, dirty)
    }

    /// A data access to DDR at `a`. -> (missed, pushed a dirty line,
    /// inhibited: the access went to memory).
    pub fn data(&mut self, a: u32, write: bool) -> (bool, bool, bool) {
        let mode = self.data_mode(a);
        if mode == DMode::Inhibited {
            return (false, false, true);
        }
        let la = a & !(LINE - 1);
        let set = self.set_of(la);
        if mode == DMode::WriteThrough && write {
            // Writes go through the store buffer; a hit updates the line,
            // a miss doesn't allocate.
            let base = set * WAYS;
            if (0..WAYS).any(|w| self.d[base + w] & 1 != 0 && self.d[base + w] & !0xF == la) {
                self.d_hits += 1;
            }
            return (false, false, false);
        }
        let (hit, k, dirty) = Self::lookup(&mut self.d, &mut self.d_rr, set, la);
        if hit {
            self.d_hits += 1;
        } else {
            self.d_misses += 1;
            if dirty {
                self.d_pushes += 1;
            }
        }
        if write && mode == DMode::CopyBack {
            self.d[k] |= 2;
        }
        (!hit, !hit && dirty, false)
    }

    /// An instruction fetch of the line holding `a`. -> missed.
    pub fn ifetch(&mut self, a: u32) -> bool {
        if !self.insn_cached(a) {
            return false;
        }
        let la = a & !(LINE - 1);
        let set = self.set_of(la);
        let (hit, _, _) = Self::lookup(&mut self.i, &mut self.i_rr, set, la);
        if hit {
            self.i_hits += 1;
        } else {
            self.i_misses += 1;
        }
        !hit
    }

    /// CPUSHL (1 dc, 2 ic, 3 bc) by set and way; INTOUCH is `which` 0.
    /// -> a dirty line was pushed.
    pub fn cpushl(&mut self, which: u8, an: u32) -> bool {
        if which == 0 {
            self.ifetch(an);
            return false;
        }
        let k = self.set_of(an) * WAYS + (an & 3) as usize;
        let mut pushed = false;
        if which & 1 != 0 {
            pushed = self.d[k] & 3 == 3;
            self.d[k] &= !2;
            if self.cacr & CACR_DPI == 0 {
                self.d[k] = 0;
            }
        }
        if which & 2 != 0 && self.cacr & CACR_IDPI == 0 {
            self.i[k] = 0;
        }
        pushed
    }
}

/// Effective-address mode and register of a 6-bit field.
#[inline]
fn ea(field: u16) -> (u16, u16) {
    ((field >> 3) & 7, field & 7)
}

/// Extension words an <ea> takes (`size`: 1, 2 or 4 bytes for #imm).
fn ea_words(mode: u16, reg: u16, size: u32) -> usize {
    match mode {
        5 | 6 => 1,
        7 => match reg {
            0 | 2 | 3 => 1,
            1 => 2,
            4 => {
                if size == 4 {
                    2
                } else {
                    1
                }
            }
            _ => 0,
        },
        _ => 0,
    }
}

fn is_mem(mode: u16, reg: u16) -> bool {
    mode >= 2 && !(mode == 7 && reg == 4)
}

fn is_indexed(mode: u16, reg: u16) -> bool {
    mode == 6 || (mode == 7 && reg == 3)
}

/// Cache size per cache: the MCF5441x's 8 KB (MCF54418RM 6.2), or
/// `DTEMU_CACHE_KB`.
fn p_kb() -> usize {
    std::env::var("DTEMU_CACHE_KB").ok().and_then(|s| s.parse().ok()).unwrap_or(8)
}

impl Timing {
    pub fn new(p: Params) -> Timing {
        Timing {
            p,
            stats: Stats::default(),
            last_dst: NONE,
            last_mac: false,
            last_folded: false,
            fold_acc: 0.0,
            bc: [(u32::MAX, 0); 8],
            pt: vec![1; 128],
            rs: Vec::with_capacity(4),
            tags: Tags::new(p_kb()),
            memo_key: memo_keys(),
            memo: memo_vals(),
            last_iline: u32::MAX,
            measuring: true,
            next_switch: 0,
            learn_from: 0,
            g_extra: 1.0,
            win_extra: 0,
            win_ops: 0,
            cold_extra: 0,
            cold_ops: 0,
            g_cold: 1.0,
            frac: 0.0,
            rng: 0x9E37_79B9_7F4A_7C15,
            probe: std::env::var("DTEMU_TIMING_FN")
                .ok()
                .and_then(|a| u32::from_str_radix(a.trim_start_matches("0x"), 16).ok())
                .map(|addr| Probe { addr, ..Probe::default() }),
        }
    }

    /// From the environment: None unless `DTEMU_TIMING=unit`. A bad
    /// `DTEMU_TIMING` or `DTEMU_TIMING_PARAMS` is an error, not a silent
    /// default, so a run never claims unit timing it doesn't have.
    pub fn from_env() -> Result<Option<Timing>, String> {
        let v = std::env::var("DTEMU_TIMING").unwrap_or_default();
        let exact = match v.as_str() {
            "" | "0" | "off" => return Ok(None),
            "unit" | "1" => false,
            "exact" => true,
            other => return Err(format!("DTEMU_TIMING={other}: want `unit`, `exact` (or unset)")),
        };
        let mut p = Params::default();
        if let Ok(s) = std::env::var("DTEMU_TIMING_PARAMS") {
            p.apply(&s)?;
        }
        if exact {
            p.window_us = p.period_us;
        }
        Ok(Some(Timing::new(p)))
    }

    /// The part of an instruction's cost that depends only on its words:
    /// table cycles, the registers it changes and reads for addresses, and
    /// what `post` must check. Memoised per PC (`memo`).
    pub fn decode(w: &[u16; 6]) -> Decoded {
        let op = w[0];
        let mut d = Decoded { uses: [(NONE, NONE, 0); 2], ..Decoded::default() };
        d.dst = NONE;
        let (sm, sr) = ea(op & 0x3F);
        match op >> 12 {
            // MOVE.B/.L/.W
            0x1..=0x3 => {
                d.is_move = true;
                let size = match op >> 12 {
                    1 => 1,
                    2 => 4,
                    _ => 2,
                };
                let (dm, dr) = ((op >> 6) & 7, (op >> 9) & 7);
                let sw = ea_words(sm, sr, size);
                if is_indexed(sm, sr) {
                    d.x += 1;
                }
                if dm == 6 {
                    d.x += 1;
                }
                if is_mem(sm, sr) && dm >= 2 {
                    d.x += 1;
                }
                use_ea(&mut d.uses[0], sm, sr, w[1]);
                if dm >= 2 {
                    use_ea(&mut d.uses[1], dm, dr, w[1 + sw]);
                }
                match dm {
                    0 => d.dst = dr as u8,
                    1 => d.dst = 8 + dr as u8,
                    _ => {}
                }
            }
            0x0 => {
                // Bit ops: BTST/BCHG/BCLR/BSET, dynamic (bit 8) or static (0x08xx).
                let bitop = op & 0x0100 != 0 || op & 0x0F00 == 0x0800;
                if bitop {
                    let kind = (op >> 6) & 3; // 0 BTST, 1 BCHG, 2 BCLR, 3 BSET
                    let ext = if op & 0x0100 != 0 { 0 } else { 1 };
                    if kind != 0 || (sm == 0 && op & 0x0100 != 0) {
                        d.x += 1;
                    }
                    if is_indexed(sm, sr) {
                        d.x += 1;
                    }
                    use_ea(&mut d.uses[0], sm, sr, w[1 + ext]);
                    if sm == 0 && kind != 0 {
                        d.dst = sr as u8;
                    }
                } else {
                    // ORI/ANDI/SUBI/ADDI/EORI/CMPI #imm,Dx
                    if sm == 0 && (op >> 9) & 7 != 6 {
                        d.dst = sr as u8;
                    }
                }
            }
            0x4 => {
                match op {
                    0x4E71 => d.x += 5,                               // NOP: 6, syncs
                    0x4E73 => d.x += 14,                              // RTE: 15
                    0x4E72 => d.x += 5,                               // STOP: 6
                    0x4E75 => d.flow = 2,                     // RTS: post
                    0x4E7B => d.x += 19,                              // MOVEC: 20
                    0x4E60..=0x4E6F => d.x += 2,                      // MOVE USP: 3
                    _ if op & 0xFFF8 == 0x4E50 => d.x += 1,           // LINK.W: 2
                    _ if op & 0xFFF0 == 0x4E40 => {}                // TRAP: exc
                    _ if op & 0xFF80 == 0x4E80 => {
                        // JSR (0x4E80) / JMP (0x4EC0)
                        d.x += match sm {
                            2 | 5 => 4,
                            6 => 5,
                            7 if sr == 3 => 5,
                            _ => 1, // abs, (d16,PC): 1-3 cycles
                        };
                        use_ea(&mut d.uses[0], sm, sr, w[1]);
                        if op & 0x0040 == 0 {
                            d.flow = 1;
                        }
                    }
                    _ if op & 0xFFC0 == 0x4C00 => {
                        // MULx.L: 4
                        d.x += 3;
                        use_ea(&mut d.uses[0], sm, sr, w[2]);
                        d.dst = ((w[1] >> 12) & 7) as u8;
                    }
                    _ if op & 0xFFC0 == 0x4C40 => {
                        // DIVx.L / REMx.L: 35
                        d.x += 34;
                        use_ea(&mut d.uses[0], sm, sr, w[2]);
                        d.dst = ((w[1] >> 12) & 7) as u8;
                    }
                    _ if (op & 0xFFC0 == 0x48C0 || op & 0xFFC0 == 0x4CC0) && (sm == 2 || sm == 5) => {
                        // MOVEM.L: n
                        d.x += (w[1].count_ones() as u16).saturating_sub(1);
                        use_ea(&mut d.uses[0], sm, sr, 0);
                    }
                    _ if op & 0xFFC0 == 0x46C0 => {
                        // MOVE to SR: 4 (1 with #imm and bit 13 set)
                        if !(sm == 7 && sr == 4 && w[1] & 0x2000 != 0) {
                            d.x += 3;
                        }
                    }
                    _ if op & 0xF1C0 == 0x41C0 => {
                        // LEA: the address ALU, forwarded without stalls.
                        if is_indexed(sm, sr) {
                            d.x += 1;
                        }
                        use_ea(&mut d.uses[0], sm, sr, w[1]);
                    }
                    _ if op & 0xFFC0 == 0x4840 => {
                        // PEA (0x4840) / SWAP (mode 0)
                        if sm == 0 {
                            d.dst = sr as u8;
                        } else {
                            if is_indexed(sm, sr) {
                                d.x += 1;
                            }
                            use_ea(&mut d.uses[0], sm, sr, w[1]);
                        }
                    }
                    _ => {
                        // CLR, NEG, NEGX, NOT, EXT, TST, TAS, MOVE from SR...
                        if is_indexed(sm, sr) {
                            d.x += 1;
                        }
                        use_ea(&mut d.uses[0], sm, sr, w[1]);
                        if sm == 0 && op & 0x0F00 != 0x0A00 {
                            d.dst = sr as u8;
                        }
                    }
                }
            }
            0x5 => {
                // ADDQ/SUBQ/Scc/TPF
                if is_indexed(sm, sr) {
                    d.x += 1;
                }
                use_ea(&mut d.uses[0], sm, sr, w[1]);
                match sm {
                    0 => d.dst = sr as u8,
                    1 => d.dst = 8 + sr as u8,
                    _ => {}
                }
            }
            0x6 => {
                let cond = (op >> 8) & 15;
                let d8 = op & 0xFF;
                let len = match d8 {
                    0 => 4,
                    0xFF => 6,
                    _ => 2,
                };
                match cond {
                    0 => d.x += 1, // BRA: 1-3
                    1 => {
                        d.x += 1; // BSR: 1-3
                        d.flow = 1;
                    }
                    _ => {
                        d.flow = 3;
                        d.len = len;
                    }
                }
            }
            0x7 => d.dst = ((op >> 9) & 7) as u8, // MOVEQ / MVS / MVZ
            0x8 | 0xC => {
                let opmode = (op >> 6) & 7;
                let dx = ((op >> 9) & 7) as u8;
                if opmode == 3 || opmode == 7 {
                    // DIVx.W 20 / MULx.W 4
                    d.x += if op >> 12 == 8 { 19 } else { 3 };
                    d.dst = dx;
                } else if opmode < 3 {
                    d.dst = dx;
                }
                if is_indexed(sm, sr) {
                    d.x += 1;
                }
                use_ea(&mut d.uses[0], sm, sr, w[1]);
            }
            0x9 | 0xB | 0xD => {
                let opmode = (op >> 6) & 7;
                let rx = ((op >> 9) & 7) as u8;
                if is_indexed(sm, sr) {
                    d.x += 1;
                }
                use_ea(&mut d.uses[0], sm, sr, w[1]);
                let cmp = op >> 12 == 0xB && (opmode < 3 || opmode == 7 || opmode == 3);
                if !cmp {
                    match opmode {
                        0..=2 => d.dst = rx,
                        3 | 7 => d.dst = 8 + rx,
                        _ if sm == 0 && op >> 12 == 0xB => d.dst = sr as u8, // EOR Dy,Dx
                        _ if sm == 0 => d.dst = rx,                          // ADDX / SUBX Dy,Dx
                        _ => {}
                    }
                }
            }
            0xA => {
                // EMAC.
                let movacc_to = op & 0xF9C0 == 0xA100; // MOVE.L <ea>,ACCx
                if op & 0xFFC0 == 0xA900 {
                    d.x += 7; // MOVE.L <ea>,MACSR: 8
                } else if op & 0xFFC0 == 0xAD00 {
                    d.x += 6; // MOVE.L <ea>,MASK: 7
                } else if op & 0xF9F0 == 0xA180 || op & 0xFFF0 == 0xA980 || op & 0xFFF0 == 0xAD80 || op & 0xFFF0 == 0xAB80 || op & 0xFFF0 == 0xAF80 {
                    // MOVE.L ACCx/MACSR/MASK/ACCEXT,Rx: 4 right after a MAC.
                    d.mac_rd = true;
                    d.dst = (op & 15) as u8;
                } else if movacc_to {
                    d.mac = true;
                } else {
                    // MAC/MSAC, maybe with a load (bit 7 of the opword set
                    // means register form for some encodings; the load forms
                    // write Rw).
                    d.mac = true;
                    use_ea(&mut d.uses[0], sm, sr, 0);
                }
            }
            0xE => {
                if op & 0x00C0 != 0x00C0 {
                    d.dst = (op & 7) as u8;
                }
            }
            0xF if op & 0xFF00 == 0xF400 => {
                d.x += match (op >> 6) & 3 {
                    1 => 11, // dc: 12
                    _ => 17, // ic, bc: 18
                };
                if op & 0xFFF8 == 0xF428 {
                    d.x = 18; // INTOUCH: 19
                }
            }
            _ => {}
        }
        d
    }

    /// Cycles beyond one for the instruction at `pc` whose words start
    /// with `w` (`w[0]` the opword; 6 words, zero past the end are fine).
    /// -> (extra cycles, what `post` must check, whether a cycle is
    /// refunded for folding).
    pub fn pre(&mut self, pc: u32, w: &[u16; 6]) -> (u64, Flow, bool) {
        let d = Self::decode(w);
        self.apply(pc, &d)
    }

    /// `pre` with the decoding memoised by (PC, opword, next word).
    #[inline]
    pub fn pre_memo(&mut self, pc: u32, op: u16, w1: u16, words: impl FnOnce() -> [u16; 6]) -> (u64, Flow, bool) {
        let k = ((pc >> 1) as usize) & (MEMO - 1);
        let key = (pc, op, w1);
        if self.memo_key[k] != key {
            self.memo_key[k] = key;
            self.memo[k] = Self::decode(&words());
        }
        let d = self.memo[k];
        self.apply(pc, &d)
    }

    fn apply(&mut self, pc: u32, d: &Decoded) -> (u64, Flow, bool) {
        let mut x = d.x as u64;
        if d.mac_rd && self.last_mac {
            x += 3;
        }
        let mut stall = 0u64;
        if self.last_dst != NONE {
            for &(base, idx, st) in &d.uses {
                if base == self.last_dst {
                    stall = stall.max(2);
                }
                if idx == self.last_dst {
                    stall = stall.max(st as u64);
                }
            }
        }
        let flow = match d.flow {
            1 => Flow::Call,
            2 => Flow::Rts,
            3 => Flow::Bcc { pc, next: pc.wrapping_add(d.len as u32) },
            _ => Flow::None,
        };
        // MOVE folding: a MOVE after an unfolded instruction issues with it.
        let mut folded = false;
        if d.is_move && !self.last_folded && self.p.fold > 0.0 && stall == 0 && x == 0 {
            self.fold_acc += self.p.fold;
            if self.fold_acc >= 1.0 {
                self.fold_acc -= 1.0;
                folded = true;
                self.stats.folded += 1;
            }
        }
        self.last_folded = folded;
        self.last_dst = d.dst;
        self.last_mac = d.mac;
        self.stats.instructions += 1;
        self.stats.table += x;
        self.stats.stall += stall;
        (x + stall, flow, folded)
    }

    /// After the instruction ran: branch prediction and the return stack.
    /// `pc_after` is the PC it left; `sp_top` the long at (A7).
    /// -> cycles to add, beyond the one the instruction already took; -1
    /// for a taken branch the branch cache folds away (0 cycles).
    pub fn post(&mut self, flow: Flow, pc_after: u32, sp_top: u32) -> i64 {
        let c = match flow {
            Flow::None => 0,
            Flow::Call => {
                if self.rs.len() == 4 {
                    self.rs.remove(0);
                }
                self.rs.push(sp_top);
                0
            }
            Flow::Rts => match self.rs.pop() {
                Some(a) if a == pc_after => 1,
                Some(_) => 8,
                None => 7,
            },
            Flow::Bcc { pc, next } => {
                let taken = pc_after != next;
                let bi = ((pc >> 1) & 7) as usize;
                let pi = ((pc >> 1) & 127) as usize;
                let (hit, ctr) = if self.bc[bi].0 == pc { (true, self.bc[bi].1) } else { (false, self.pt[pi]) };
                let pred = ctr >= 2;
                let cost: i64 = if pred != taken {
                    self.stats.mispredicts += 1;
                    self.p.mispredict as i64 - 1
                } else if taken && hit {
                    -1
                } else {
                    0
                };
                let n = if taken { (ctr + 1).min(3) } else { ctr.saturating_sub(1) };
                if hit {
                    self.bc[bi].1 = n;
                } else if taken {
                    // Taken branches go into the branch cache.
                    self.bc[bi] = (pc, n);
                }
                self.pt[pi] = n;
                cost
            }
        };
        self.stats.branch += c;
        c
    }

    #[inline]
    pub fn exception(&mut self) -> u64 {
        self.last_dst = NONE;
        self.last_mac = false;
        self.stats.exc += self.p.exc as u64;
        self.p.exc as u64
    }

    /// Sampled mode: the phase at clock `now`. -> Some(measuring) when it
    /// changes. Exact mode (window = period) measures throughout.
    #[inline]
    pub fn phase(&mut self, now: u64) -> Option<bool> {
        if now < self.next_switch {
            return None;
        }
        let us = self.p.hz / 1e6;
        if self.p.window_us >= self.p.period_us {
            self.next_switch = u64::MAX;
            self.measuring = true;
            return Some(true);
        }
        if self.measuring && self.next_switch != 0 {
            // A window ends: its average for code without a learned cost.
            if self.win_ops > 0 {
                self.g_extra = self.win_extra as f64 / self.win_ops as f64;
            }
            self.measuring = false;
            // The gap is jittered (0.5x..1.5x, a fixed xorshift sequence, so
            // runs repeat) so that windows do not lock onto the firmware's
            // own periods (UI ticks, render blocks) and sample one phase of them.
            self.rng ^= self.rng << 13;
            self.rng ^= self.rng >> 7;
            self.rng ^= self.rng << 17;
            let j = 0.5 + (self.rng >> 11) as f64 / (1u64 << 53) as f64;
            self.next_switch = now + ((self.p.period_us - self.p.window_us) * us * j) as u64;
        } else {
            self.measuring = true;
            self.win_extra = 0;
            self.win_ops = 0;
            self.last_iline = u32::MAX;
            self.last_dst = NONE;
            self.learn_from = now + (self.p.window_us * self.p.warm * us) as u64;
            self.next_switch = now + (self.p.window_us * us) as u64;
        }
        Some(self.measuring)
    }

    /// A measured run of a block: `extra` cycles beyond one per
    /// instruction over `ops` instructions, ending at clock `now`. -> the
    /// block's updated (cost, runs).
    #[inline]
    pub fn learn(&mut self, now: u64, extra: u64, ops: u64, cost: f32, n: u32) -> (f32, u32) {
        if now < self.learn_from {
            return (cost, n);
        }
        self.win_extra += extra;
        self.win_ops += ops;
        let e = extra as f32;
        if n == 0 {
            self.cold_extra += extra;
            self.cold_ops += ops;
            self.g_cold = self.cold_extra as f64 / self.cold_ops as f64;
            (e, 1)
        } else {
            {
            // A running mean, not a short moving average: a block's costly
            // runs come in bursts (the cold first pass of a loop, then many
            // warm ones), and a short average forgets them by the time the
            // burst ends. Capped so the mean can still follow a change.
            let k = n.saturating_add(1).min(1 << 16);
            (cost + (e - cost) / k as f32, n.saturating_add(1))
        }
        }
    }

    /// Outside a window: `x` learned cycles -> whole cycles to add now.
    #[inline]
    pub fn charge_learned(&mut self, x: f64) -> u64 {
        self.frac += x;
        let c = self.frac.floor();
        self.frac -= c;
        self.stats.learned += c as u64;
        c as u64
    }

    /// Instruction fetch of `len` bytes at `pc`: I-cache line fills.
    #[inline]
    pub fn ifetch(&mut self, pc: u32, len: u32) -> u64 {
        let end = pc.wrapping_add(len.max(1) - 1);
        let (l0, l1) = (pc & !(LINE - 1), end & !(LINE - 1));
        let mut c = 0u64;
        if l0 != self.last_iline {
            if self.tags.ifetch(pc) {
                c += if l0 == self.last_iline.wrapping_add(LINE) { self.p.imiss_seq } else { self.p.imiss } as u64;
            }
        } else {
            self.tags.i_hits += 1;
        }
        if l1 != l0 && self.tags.ifetch(end) {
            c += self.p.imiss_seq as u64;
        }
        self.last_iline = l1;
        self.stats.imiss += c;
        c
    }

    /// A CPU data access at `a` in region `r` -> cycles.
    #[inline]
    pub fn data(&mut self, a: u32, write: bool, r: Region) -> u64 {
        match r {
            Region::Ddr => {
                let (miss, push, inhibited) = self.tags.data(a, write);
                if inhibited {
                    let c = if write { self.p.unc_wr } else { self.p.unc_rd } as u64;
                    self.stats.unc += c;
                    return c;
                }
                let (a, b) = (if miss { self.p.dmiss as u64 } else { 0 }, if push { self.p.wb as u64 } else { 0 });
                self.stats.dmiss += a;
                self.stats.wb += b;
                a + b
            }
            Region::Sram => {
                self.stats.sram += self.p.sram as u64;
                self.p.sram as u64
            }
            Region::Io => {
                *self.stats.io_pages.entry(a & !0x3FFF).or_insert(0) += 1;
                self.stats.io += self.p.io as u64;
                self.p.io as u64
            }
        }
    }

    /// CPUSHL / INTOUCH on the tags -> cycles beyond the table's.
    pub fn cache_op(&mut self, which: u8, an: u32, in_ddr: bool) -> u64 {
        if which == 0 && !in_ddr {
            return 0;
        }
        let pushed = self.tags.cpushl(which, an);
        let c = if pushed { self.p.wb as u64 } else { 0 };
        self.stats.wb += c;
        c
    }

    /// One line for the end of a run.
    pub fn summary(&self, now: u64) -> String {
        let probe = self.probe.as_ref().map(|p| {
            let avg = if p.calls == 0 { 0.0 } else { p.cycles as f64 / p.calls as f64 };
            format!(
                "\ntiming: fn {:08x}: {} calls, {:.0} cycles = {:.2} us a call",
                p.addr,
                p.calls,
                avg,
                avg / self.p.hz * 1e6
            )
        });
        let s = &self.stats;
        let pct = |v: u64| if now == 0 { 0.0 } else { v as f64 * 100.0 / now as f64 };
        let parts = [
            ("table", s.table),
            ("stall", s.stall),
            ("exc", s.exc),
            ("dmiss", s.dmiss),
            ("wb", s.wb),
            ("imiss", s.imiss),
            ("unc", s.unc),
            ("sram", s.sram),
            ("io", s.io),
            ("learned", s.learned),
            ("cold_runs", s.cold_runs),
            ("gap_ops", s.gap_ops),
        ];
        let tg = &self.tags;
        let rate = |h: u64, m: u64| if h + m == 0 { 0.0 } else { 100.0 * h as f64 / (h + m) as f64 };
        let mut out = format!(
            "timing: unit ({}), {:.0} MHz, {} cycles, {} instructions ({} folded, {} mispredicted branches); \
             D-cache {:.2}% hits, I-cache {:.2}% hits; extra:",
            if self.p.window_us >= self.p.period_us { "exact".to_string() } else { format!("sampled {:.0}/{:.0} us", self.p.window_us, self.p.period_us) },
            self.p.hz / 1e6,
            now,
            s.instructions,
            s.folded,
            s.mispredicts,
            rate(tg.d_hits, tg.d_misses),
            rate(tg.i_hits, tg.i_misses)
        );
        for (k, v) in parts {
            out += &format!(" {k} {v} ({:.1}%)", pct(v));
        }
        out += &format!(" branch {}", s.branch);
        let mut pages: Vec<_> = s.io_pages.iter().collect();
        pages.sort_by_key(|p| std::cmp::Reverse(*p.1));
        out += "\ntiming: io pages:";
        for (pg, n) in pages.iter().take(8) {
            out += &format!(" {pg:08x}:{n}");
        }
        out + &probe.unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Ddr,
    Sram,
    Io,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cost(t: &mut Timing, w: &[u16]) -> u64 {
        let mut a = [0u16; 6];
        a[..w.len()].copy_from_slice(w);
        t.pre(0x4000_0000, &a).0
    }

    #[test]
    fn params_parse() {
        let mut p = Params::default();
        p.apply("dmiss=30, fold=0.25,io=5, emmc_write_us=200").unwrap();
        assert_eq!((p.dmiss, p.fold, p.io), (30, 0.25, 5));
        assert!(p.apply("nope=1").is_err());
        assert!(p.apply("dmiss=1.5").is_err());
        assert!(p.apply("dmiss").is_err());
        assert!(p.apply("fold=2").is_err());
    }

    /// Table values from MCF54418RM section 3.3.5 (extra = table - 1).
    #[test]
    fn manual_tables() {
        let mut t = Timing::new(Params::default());
        assert_eq!(cost(&mut t, &[0x2001]), 0); // MOVE.L d1,d0: 1
        assert_eq!(cost(&mut t, &[0x2290]), 1); // MOVE.L (a0),(a1): 2
        assert_eq!(cost(&mut t, &[0x2030, 0x1800]), 1); // MOVE.L (0,a0,d1.l),d0: 2
        assert_eq!(cost(&mut t, &[0x4E71]), 5); // NOP: 6
        assert_eq!(cost(&mut t, &[0x4E73]), 14); // RTE: 15
        assert_eq!(cost(&mut t, &[0x4E7B, 0x0002]), 19); // MOVEC: 20
        assert_eq!(cost(&mut t, &[0x4C00, 0x0800]), 3); // MULS.L d0,d0: 4
        assert_eq!(cost(&mut t, &[0x4C41, 0x0800]), 34); // DIVS.L d1,d0: 35
        assert_eq!(cost(&mut t, &[0xC1C1]), 3); // MULS.W d1,d0: 4
        assert_eq!(cost(&mut t, &[0x80C1]), 19); // DIVU.W d1,d0: 20
        assert_eq!(cost(&mut t, &[0x48D7, 0x00FF]), 7); // MOVEM.L d0-d7,(sp): 8
        assert_eq!(cost(&mut t, &[0x4E90]), 4); // JSR (a0): 5
        assert_eq!(cost(&mut t, &[0x4EB0, 0x0800]), 5); // JSR (0,a0,d0): 6
        assert_eq!(cost(&mut t, &[0x08D0, 0x0003]), 1); // BSET #3,(a0): 2
        assert_eq!(cost(&mut t, &[0xF4E8]), 17); // CPUSHL bc,(a0): 18
        assert_eq!(cost(&mut t, &[0x46FC, 0x2700]), 0); // MOVE #$2700,SR: 1
        assert_eq!(cost(&mut t, &[0x46C0]), 3); // MOVE d0,SR: 4
    }

    #[test]
    fn load_use_stall() {
        let mut t = Timing::new(Params::default());
        // MULS.L #..,d0 then MOVE.L (0,a0,d0.l*4),d1: 3-cycle stall.
        cost(&mut t, &[0x4C3C, 0x0800, 0, 4]);
        assert_eq!(cost(&mut t, &[0x2230, 0x0C00]), 1 + 3);
        // ADDQ.L #4,a0 then MOVE.L (a0),d1: base register, 2 cycles.
        cost(&mut t, &[0x5888]);
        assert_eq!(cost(&mut t, &[0x2210]), 2);
        // ...but not when an unrelated instruction sits between.
        cost(&mut t, &[0x5888]);
        cost(&mut t, &[0x7001]);
        assert_eq!(cost(&mut t, &[0x2210]), 0);
        // LEA forwards: no stall.
        cost(&mut t, &[0x41E8, 0x0010]);
        assert_eq!(cost(&mut t, &[0x2210]), 0);
    }

    #[test]
    fn branch_prediction() {
        let mut t = Timing::new(Params::default());
        let pc = 0x4000_1000;
        let mut costs = vec![];
        for _ in 0..4 {
            let (_, f, _) = t.pre(pc, &[0x66FE, 0, 0, 0, 0, 0]); // BNE.S *
            costs.push(t.post(f, pc, 0)); // taken (back to itself)
        }
        // Weakly not-taken at first: a mispredict (8 cycles), then in the
        // branch cache and predicted taken: 0 cycles (one refunded).
        assert_eq!(costs, vec![7, -1, -1, -1]);
        // Falls through: mispredicted.
        let (_, f, _) = t.pre(pc, &[0x66FE, 0, 0, 0, 0, 0]);
        assert_eq!(t.post(f, pc + 2, 0), 7);
    }

    #[test]
    fn return_stack() {
        let mut t = Timing::new(Params::default());
        let (_, f, _) = t.pre(0x4000_0000, &[0x4E90, 0, 0, 0, 0, 0]);
        t.post(f, 0x4000_2000, 0x4000_0002);
        let (_, f, _) = t.pre(0x4000_2000, &[0x4E75, 0, 0, 0, 0, 0]);
        assert_eq!(t.post(f, 0x4000_0002, 0), 1); // RTS predicted: 2
        let (_, f, _) = t.pre(0x4000_2000, &[0x4E75, 0, 0, 0, 0, 0]);
        assert_eq!(t.post(f, 0x4000_0010, 0), 7); // stack empty: 8
    }

    #[test]
    fn folding() {
        let mut t = Timing::new(Params { fold: 0.5, ..Params::default() });
        let n = (0..8).filter(|_| t.pre(0, &[0x2001, 0, 0, 0, 0, 0]).2).count();
        // A pair issues once, so a folded MOVE never folds the next one.
        assert_eq!(n, 3);
    }
}
