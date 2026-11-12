//! The eSDHC controller (0xFC0CC000, RM chapter 25) and the eMMC behind it.
//!
//! The firmware drives the controller in PIO mode for commands and moves
//! bulk data through SoC eDMA channel 59, reading or writing DATPORT; the
//! controller's own DMA (XFERTYP[DMAEN]) is never used. Completion reaches
//! the firmware through its own interrupt handlers: the controller's
//! (vector 223, INTC2 source 31) on IRQSTAT & IRQSIGEN, and channel 59's
//! major-loop interrupt (vector 192). Nothing here writes driver state.
//!
//! Timing: by default the card takes no time at all. With DTEMU_TIMING=unit
//! (`CardTiming`, `emmc_*` keys of DTEMU_TIMING_PARAMS) it holds the bus as an eMMC does: a read command's data
//! arrives after an access time plus the transfer at the bus clock and width
//! the firmware set; after a write's data the card stays busy (DAT0 low,
//! TC withheld) for a per-command overhead plus a program time per block;
//! CMD38 erase and the other R1b commands (CMD6, CMD12) busy it too, and the
//! end of every busy is an edge on EPORT IRQ3, the pin wired to DAT0.
//! DTEMU_ESDHC_LOG prints each command and busy with its time.
//!
//! The card must be one the firmware recognises, or it will not mount the
//! +Drive: `FUN_400e253e` matches the CID's manufacturer and product name
//! against a seven-row table at 0x4020c2c4, and `FUN_400e26f2` checks three
//! EXT_CSD identity bytes and the sector count against the same row. The
//! values below are that table's own (reference project, esdhc.py `PART`).

use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Seek, SeekFrom, Write};

pub const BASE: u32 = 0xFC0C_C000;
pub const DMA_CHAN: usize = 59;

const CC: u32 = 1 << 0;
const TC: u32 = 1 << 1;
const BWR: u32 = 1 << 4;
const BRR: u32 = 1 << 5;

const CIHB: u32 = 1 << 0;
const CDIHB: u32 = 1 << 1;
const DLA: u32 = 1 << 2;
const BWEN: u32 = 1 << 10;
const BREN: u32 = 1 << 11;
const CINS: u32 = 1 << 16;
/// PRSSTAT DLSL[0]: the DAT0 line's level (low while the card is busy).
const DAT0: u32 = 1 << 24;

const SYSCTL_SELFCLEAR: u32 = 0x0F00_0000;

pub const MANUFACTURER: u32 = 0x11;
pub const PRODUCT: &[u8; 6] = b"004GE0";
pub const SECTORS_SLC: u32 = 0x003B_0000;
const ID_MULT: u32 = 0x0001D8;
const ID_B: u8 = 0x01;
const ID_C: u8 = 0x08;

/// How long the card holds the bus (DTEMU_TIMING=unit). Times in seconds.
///
/// Fitted to recorded unit results, not to a datasheet (`UNIT`):
/// - The firmware writes the +Drive in 16 KB reliable writes (CMD23 with
///   bit 31, then CMD25 of 32 blocks). The drive loader's boot inits (T169:
///   90.2 and 84.7 ms on the unit, 6.0 ms in fast mode) are about 14 of
///   those plus 20 reads; 0.9 ms + 32 x 26 us a write lands them at 85.0
///   and 85.0 ms with DTEMU_TIMING=exact's CPU costs (unit-timing 8dd6c30). How that splits between the
///   per-command and per-block parts is a guess: every write seen is 32 or
///   128 blocks.
/// - The base clock is the 132 MHz bus clock [inferred]: SYSCTL 0x0E010C
///   divides it by 2, so 66 MHz on a 4-bit bus. Reading the 13 mods of T178
///   (6.3 MB) then costs 0.5 s, which the unit's 5.32 s link (4.62 s in
///   fast mode) leaves room for; a slower clock would not.
/// - Read access, erase and R1b times are small guesses: no recorded result
///   separates them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CardTiming {
    /// Busy after a write command's data (CMD24/25), on top of the blocks.
    pub write_s: f64,
    /// Program time per 512-byte block written.
    pub block_s: f64,
    /// Access time before a read command's first block.
    pub read_s: f64,
    /// Busy after CMD38 (erase), whatever the range.
    pub erase_s: f64,
    /// Busy after the other R1b commands (CMD6 SWITCH, CMD12 STOP, ...).
    pub r1b_s: f64,
    /// The controller's base clock; SDCLK is this over SYSCTL's prescaler
    /// and divisor.
    pub base_hz: f64,
}

impl CardTiming {
    /// The unit-mode defaults.
    pub const UNIT: CardTiming = CardTiming {
        write_s: 0.9e-3,
        block_s: 26e-6,
        read_s: 20e-6,
        erase_s: 1e-3,
        r1b_s: 250e-6,
        base_hz: 132e6,
    };

    /// Apply the `emmc_*` keys of `key=value,...` (DTEMU_TIMING_PARAMS,
    /// which timing.rs shares: its own keys are left to it). Times in us,
    /// `emmc_base_mhz` in MHz.
    pub fn apply(&mut self, s: &str) -> Result<(), String> {
        for kv in s.split(',').map(str::trim).filter(|kv| !kv.is_empty()) {
            let Some((k, v)) = kv.split_once('=') else { continue };
            let k = k.trim();
            if !k.starts_with("emmc_") {
                continue;
            }
            let f: f64 = v.trim().parse().map_err(|_| format!("timing param `{kv}`: not a number"))?;
            if f < 0.0 || (k == "emmc_base_mhz" && f <= 0.0) {
                return Err(format!("timing param `{kv}`: out of range"));
            }
            match k {
                "emmc_write_us" => self.write_s = f * 1e-6,
                "emmc_block_us" => self.block_s = f * 1e-6,
                "emmc_read_us" => self.read_s = f * 1e-6,
                "emmc_erase_us" => self.erase_s = f * 1e-6,
                "emmc_r1b_us" => self.r1b_s = f * 1e-6,
                "emmc_base_mhz" => self.base_hz = f * 1e6,
                other => return Err(format!("timing param `{other}`: unknown")),
            }
        }
        Ok(())
    }

    /// `UNIT` with DTEMU_TIMING_PARAMS's `emmc_*` keys applied (unit mode).
    pub fn from_env() -> Result<CardTiming, String> {
        let mut t = CardTiming::UNIT;
        if let Ok(s) = std::env::var("DTEMU_TIMING_PARAMS") {
            t.apply(&s)?;
        }
        Ok(t)
    }
}

/// What the card is busy with (unit timing).
#[derive(PartialEq, Eq, Clone, Copy, Debug, Default)]
enum Busy {
    #[default]
    None,
    /// A read's data is on its way: BREN, BRR and TC when it lands.
    Read,
    /// Programming after a write's data: DAT0 low, TC when it ends.
    Prog,
    /// After an R1b command (CMD38, CMD6, CMD12): DAT0 low, no TC at the
    /// end. The firmware (1.74 0x4009234e, erase) reads PRSSTAT DLSL[0]
    /// after CC and, while DAT0 is low, waits for EPORT IRQ3, which the
    /// board wires to DAT0: `dat0_rose` raises it.
    R1b,
}

/// Sparse sector storage with an optional backing file.
#[derive(Serialize, Deserialize)]
pub struct Card {
    #[serde(with = "sector_map")]
    pub sectors: BTreeMap<u32, Box<[u8; 512]>>,
    pub capacity: u32,
    #[serde(with = "BigArray")]
    pub ext_csd: [u8; 512],
    pub cid: [u32; 4],
    pub rca: u32,
    pub path: Option<std::path::PathBuf>,
    pub dirty: bool,
    erase_lo: Option<u32>,
    erase_hi: Option<u32>,
    pub reads: u64,
    pub writes: u64,
}

impl Card {
    pub fn new() -> Card {
        let mut ext = [0u8; 512];
        ext[0x98] = 1; // SLC mode: what slc_status_predicate wants
        ext[0xD4..0xD8].copy_from_slice(&SECTORS_SLC.to_be_bytes()); // read big-endian
        ext[0xAF] = 1;
        ext[0xB7] = 1;
        ext[0xB9] = 1;
        ext[0x9C..0x9F].copy_from_slice(&ID_MULT.to_be_bytes()[1..]);
        ext[0xDE] = ID_B;
        ext[0xE3] = ID_C;
        let n = PRODUCT;
        // R2 order {RSP3[23:0], RSP2, RSP1, RSP0} over CID[127:8].
        let cid = [
            0,
            (n[4] as u32) << 24 | (n[5] as u32) << 16,
            (n[0] as u32) << 24 | (n[1] as u32) << 16 | (n[2] as u32) << 8 | n[3] as u32,
            MANUFACTURER << 16,
        ];
        Card {
            sectors: BTreeMap::new(),
            capacity: SECTORS_SLC,
            ext_csd: ext,
            cid,
            rca: 0,
            path: None,
            dirty: false,
            erase_lo: None,
            erase_hi: None,
            reads: 0,
            writes: 0,
        }
    }

    /// Back the card with an image file (read now, written by `flush`).
    pub fn open(path: &std::path::Path) -> std::io::Result<Card> {
        let mut c = Card::new();
        c.path = Some(path.to_path_buf());
        if let Ok(mut f) = std::fs::File::open(path) {
            let len = f.metadata()?.len();
            let mut buf = [0u8; 512];
            let mut s = 0u32;
            let mut off = 0u64;
            // Only keep sectors that are not all zero.
            let mut chunk = vec![0u8; 1 << 20];
            while off < len {
                let n = f.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                for part in chunk[..n].chunks(512) {
                    if part.iter().any(|&b| b != 0) {
                        buf[..part.len()].copy_from_slice(part);
                        buf[part.len()..].fill(0);
                        c.sectors.insert(s, Box::new(buf));
                    }
                    s += 1;
                }
                off += n as u64;
            }
        }
        Ok(c)
    }

    pub fn flush(&mut self) -> std::io::Result<()> {
        let Some(p) = self.path.clone() else { return Ok(()) };
        if !self.dirty {
            return Ok(());
        }
        // Written beside the image and renamed over it, so a failed write
        // never leaves a half-written card. All-zero sectors stay holes.
        let tmp = p.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            let mut last = 0u32;
            for (&s, data) in &self.sectors {
                f.seek(SeekFrom::Start(s as u64 * 512))?;
                f.write_all(&data[..])?;
                last = last.max(s + 1);
            }
            f.set_len(last as u64 * 512)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &p)?;
        self.dirty = false;
        Ok(())
    }

    pub fn read(&mut self, sector: u32, out: &mut Vec<u8>) {
        self.reads += 1;
        match self.sectors.get(&sector) {
            Some(d) => out.extend_from_slice(&d[..]),
            None => out.extend_from_slice(&[0u8; 512]),
        }
    }

    pub fn write(&mut self, sector: u32, data: &[u8]) {
        self.writes += 1;
        self.dirty = true;
        if data.iter().all(|&b| b == 0) {
            self.sectors.remove(&sector);
            return;
        }
        let mut b = [0u8; 512];
        b[..data.len().min(512)].copy_from_slice(&data[..data.len().min(512)]);
        self.sectors.insert(sector, Box::new(b));
    }

    fn erase(&mut self, lo: u32, hi_incl: u32) {
        let keys: Vec<u32> = self.sectors.range(lo..=hi_incl).map(|(k, _)| *k).collect();
        for k in keys {
            self.sectors.remove(&k);
        }
        self.dirty = true;
    }
}

impl Default for Card {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug, Serialize, Deserialize)]
enum Phase {
    Idle,
    /// Card -> host: `rx` holds what is left to read.
    Read,
    /// Host -> card: `wx` collects until `want` bytes arrive.
    Write,
}

#[derive(Serialize, Deserialize)]
pub struct Esdhc {
    #[serde(with = "BigArray")]
    pub regs: [u32; 0x40],
    pub card: Card,
    rx: VecDeque<u8>,
    wx: Vec<u8>,
    want: usize,
    phase: Phase,
    cmd: u32,
    arg: u32,
    pattern: u32,
    pub log: Vec<(u32, u32)>,
    /// CMD0 with argument 0xF0F0F0F0 (GO_PRE_IDLE_STATE) seen: the last thing
    /// the power-fail handler does with the card (OS 1.73 0x401226e6).
    #[serde(skip)]
    pub pre_idle: u64,
    /// Unit timing (DTEMU_TIMING=unit); None: everything at once.
    #[serde(skip)]
    pub timing: Option<CardTiming>,
    /// The clock and its rate, set by `Io` before each call.
    #[serde(skip)]
    pub now: u64,
    #[serde(skip)]
    pub ips: f64,
    #[serde(skip)]
    busy: Busy,
    /// When the busy ends (clock units).
    #[serde(skip)]
    pub ready_at: Option<u64>,
    /// DTEMU_ESDHC_LOG: each command and completion on stderr, with times.
    #[serde(skip)]
    trace: bool,
    /// Unit timing: total time the card held the bus, in clock units.
    #[serde(skip)]
    pub busy_total: u64,
    /// DAT0 went high at the end of a busy: an edge on EPORT IRQ3, the pin
    /// the board ties to DAT0 (`Io` takes it).
    #[serde(skip)]
    pub dat0_rose: bool,
}

impl Esdhc {
    // Registers are indexed as byte offset / 4, as the manual lists them.
    #[allow(clippy::eq_op)]
    pub fn new(card: Card) -> Esdhc {
        let mut regs = [0u32; 0x40];
        regs[0x04 / 4] = 0x0001_0000; // BLKATTR
        regs[0x24 / 4] = 0xFF88_00F8 | CINS; // PRSSTAT, a card inserted
        regs[0x28 / 4] = 0x0000_0020; // PROCTL
        regs[0x2C / 4] = 0x0000_8008; // SYSCTL
        regs[0x34 / 4] = 0x117F_013F; // IRQSTATEN
        regs[0x40 / 4] = 0x07F3_0000; // HOSTCAPBLT
        regs[0x44 / 4] = 0x0810_0810; // WML
        regs[0xC0 / 4] = 1; // VENDOR
        regs[0xFC / 4] = 0x0000_1201; // HOSTVER
        Esdhc {
            regs,
            card,
            rx: VecDeque::new(),
            wx: Vec::new(),
            want: 0,
            phase: Phase::Idle,
            cmd: 0,
            arg: 0,
            pattern: 0,
            log: Vec::new(),
            pre_idle: 0,
            timing: None,
            now: 0,
            ips: 200e6,
            busy: Busy::None,
            ready_at: None,
            trace: std::env::var_os("DTEMU_ESDHC_LOG").is_some(),
            busy_total: 0,
            dat0_rose: false,
        }
    }

    /// Seconds to clock units.
    fn ticks(&self, s: f64) -> u64 {
        (s * self.ips).round() as u64
    }

    /// Seconds to move `bytes` over the bus at the clock and width the
    /// firmware set (SYSCTL SDCLKFS/DVS, PROCTL DTW).
    fn wire_s(&self, t: &CardTiming, bytes: usize) -> f64 {
        let sys = self.r(0x2C);
        let fs = (sys >> 8) & 0xFF;
        let pre = if fs == 0 { 1.0 } else { 2.0 * (1u32 << (31 - fs.leading_zeros())) as f64 };
        let div = ((sys >> 4) & 0xF) as f64 + 1.0;
        let width = match (self.r(0x28) >> 1) & 3 {
            0 => 1.0,
            1 => 4.0,
            _ => 8.0,
        };
        bytes as f64 * 8.0 / width / (t.base_hz / pre / div)
    }

    fn start_busy(&mut self, kind: Busy, s: f64) {
        let at = self.now + self.ticks(s).max(1);
        self.busy = kind;
        if kind != Busy::Read {
            let prs = self.r(0x24) & !DAT0;
            self.set(0x24, prs);
        }
        self.ready_at = Some(at);
        self.busy_total += at - self.now;
        if self.trace {
            eprintln!("esdhc {:>12.1} us busy {:?} {:.1} us", self.us(self.now), kind, s * 1e6);
        }
    }

    fn us(&self, t: u64) -> f64 {
        t as f64 / self.ips * 1e6
    }

    /// The card is busy (unit timing): DAT0 held low.
    pub fn busy(&self) -> bool {
        self.busy != Busy::None
    }

    /// End a busy that is due at `now`. -> true when something completed.
    pub fn service(&mut self, now: u64) -> bool {
        self.now = now;
        match self.ready_at {
            Some(t) if t <= now => {
                self.complete();
                true
            }
            _ => false,
        }
    }

    fn complete(&mut self) {
        let kind = std::mem::take(&mut self.busy);
        self.ready_at = None;
        if self.trace {
            eprintln!("esdhc {:>12.1} us done {:?}", self.us(self.now), kind);
        }
        match kind {
            Busy::Read => {
                let prs = self.r(0x24) | BREN;
                self.set(0x24, prs);
                self.irq_status(BRR | TC);
            }
            Busy::Prog | Busy::R1b => {
                let prs = (self.r(0x24) & !(CIHB | CDIHB | DLA)) | DAT0;
                self.set(0x24, prs);
                if kind == Busy::Prog {
                    self.irq_status(TC);
                }
                self.dat0_rose = true;
            }
            Busy::None => {}
        }
    }

    /// After a snapshot restore (the busy state is not saved): finish what
    /// the registers show in flight, so the firmware is not left waiting.
    pub fn settle_after_restore(&mut self) {
        let prs = self.r(0x24);
        if self.phase == Phase::Read && !self.rx.is_empty() && prs & BREN == 0 {
            self.busy = Busy::Read;
            self.complete();
        } else if self.phase == Phase::Idle && prs & (CDIHB | DLA) != 0 {
            self.busy = if matches!(self.cmd, 24 | 25) { Busy::Prog } else { Busy::R1b };
            self.complete();
        }
    }

    fn r(&self, off: u32) -> u32 {
        self.regs[(off / 4) as usize]
    }

    fn set(&mut self, off: u32, v: u32) {
        self.regs[(off / 4) as usize] = v;
    }

    fn irq_status(&mut self, bits: u32) {
        let en = self.r(0x34);
        let v = self.r(0x30) | (bits & en);
        self.set(0x30, v);
    }

    /// The controller's interrupt line.
    pub fn line(&self) -> bool {
        self.r(0x30) & self.r(0x38) != 0
    }

    /// Asking eDMA channel 59 for service.
    pub fn dma_request(&self) -> bool {
        match self.phase {
            Phase::Read => !self.rx.is_empty() && self.busy != Busy::Read,
            Phase::Write => self.wx.len() < self.want,
            Phase::Idle => false,
        }
    }

    fn block_bytes(&self) -> usize {
        let blkattr = self.r(0x04);
        let size = (blkattr & 0x1FFF) as usize;
        let size = if size == 0 { 512 } else { size };
        let xfer = self.r(0x0C);
        let multi = xfer & (1 << 5) != 0; // MSBSEL
        let count = if multi && xfer & (1 << 1) != 0 { (blkattr >> 16) as usize } else { 1 };
        size * count.max(1)
    }

    fn issue(&mut self, xfer: u32) {
        if self.busy() && (xfer & (1 << 21) != 0 || (xfer >> 16) & 3 == 3) {
            // A data or busy command before the card let go of DAT0: the
            // firmware did not wait. Finish the busy first.
            self.complete();
        }
        self.set(0x0C, xfer);
        let idx = (xfer >> 24) & 0x3F;
        let arg = self.r(0x08);
        if self.log.len() < 100_000 {
            self.log.push((idx, arg));
        }
        self.cmd = idx;
        self.arg = arg;
        if idx == 0 && arg == 0xF0F0_F0F0 {
            self.pre_idle += 1;
        }
        if self.trace {
            eprintln!(
                "esdhc {:>12.1} us CMD{idx} arg {arg:08x} xfer {xfer:08x} blkattr {:08x} sysctl {:08x} proctl {:08x}",
                self.us(self.now),
                self.r(0x04),
                self.r(0x2C),
                self.r(0x28)
            );
        }
        let r1 = 0x0000_0900; // transfer state, READY_FOR_DATA
        let resp: [u32; 4] = match idx {
            0 => [0; 4],
            1 => [0xC0FF_8080, 0, 0, 0],
            2 | 10 => self.card.cid,
            9 => [0; 4],
            3 => {
                self.card.rca = arg >> 16;
                [r1, 0, 0, 0]
            }
            35 => {
                self.card.erase_lo = Some(arg);
                [r1, 0, 0, 0]
            }
            36 => {
                self.card.erase_hi = Some(arg);
                [r1, 0, 0, 0]
            }
            13 if self.busy() => [0x0000_0E00, 0, 0, 0], // prg, not READY_FOR_DATA
            38 => {
                if let (Some(lo), Some(hi)) = (self.card.erase_lo, self.card.erase_hi) {
                    self.card.erase(lo, hi);
                }
                self.card.erase_lo = None;
                self.card.erase_hi = None;
                [r1, 0, 0, 0]
            }
            _ => [r1, 0, 0, 0],
        };
        for (i, v) in resp.iter().enumerate() {
            self.set(0x10 + 4 * i as u32, *v);
        }
        let mut prs = self.r(0x24) & !(CIHB | CDIHB | DLA | BREN | BWEN);
        self.rx.clear();
        self.wx.clear();
        self.phase = Phase::Idle;
        let mut status = CC;
        if xfer & (1 << 21) != 0 {
            // A data command.
            if xfer & (1 << 4) != 0 {
                // Card -> host.
                let n = self.block_bytes();
                let mut data = Vec::with_capacity(n);
                match idx {
                    8 => data.extend_from_slice(&self.card.ext_csd),
                    14 => data.extend_from_slice(&(!self.pattern).to_be_bytes()),
                    17 | 18 => {
                        let blocks = n.div_ceil(512);
                        for s in 0..blocks as u32 {
                            self.card.read(arg + s, &mut data);
                        }
                    }
                    _ => {}
                }
                data.resize(n.max(4), 0);
                self.rx.extend(data);
                self.phase = Phase::Read;
                if let Some(t) = self.timing.filter(|_| matches!(idx, 17 | 18)) {
                    // The data lands after the access time and the wire.
                    let s = t.read_s + self.wire_s(&t, n);
                    prs |= CDIHB | DLA;
                    self.set(0x24, prs);
                    self.irq_status(status);
                    self.start_busy(Busy::Read, s);
                    return;
                }
                prs |= BREN;
                // The card has put the whole transfer into the buffer: the
                // transaction is complete from the card's side.
                status |= BRR | TC;
            } else {
                self.want = self.block_bytes();
                self.phase = Phase::Write;
                prs |= BWEN | DLA | CDIHB;
                status |= BWR;
                if idx == 19 {
                    self.want = 4;
                }
            }
        } else if let Some(t) = self.timing {
            // R1b (RSPTYP 11): the card holds DAT0 low after the response.
            if (xfer >> 16) & 3 == 3 {
                let s = if idx == 38 { t.erase_s } else { t.r1b_s };
                if s > 0.0 {
                    self.set(0x24, prs);
                    self.irq_status(status);
                    self.start_busy(Busy::R1b, s);
                    return;
                }
            }
        }
        self.set(0x24, prs);
        self.irq_status(status);
    }

    fn finish_data(&mut self) {
        let mut prs = self.r(0x24) & !(CDIHB | DLA | BREN | BWEN);
        let mut prog = None;
        if self.phase == Phase::Write {
            match self.cmd {
                24 | 25 => {
                    let data = std::mem::take(&mut self.wx);
                    if let Some(t) = self.timing {
                        let blocks = data.len().div_ceil(512) as f64;
                        prog = Some(self.wire_s(&t, data.len()) + t.write_s + blocks * t.block_s);
                    }
                    for (i, chunk) in data.chunks(512).enumerate() {
                        self.card.write(self.arg + i as u32, chunk);
                    }
                }
                19 => {
                    if self.wx.len() >= 4 {
                        self.pattern = u32::from_be_bytes(self.wx[..4].try_into().unwrap());
                    } else if let Some(&b) = self.wx.first() {
                        self.pattern = b as u32;
                    }
                }
                _ => {}
            }
        }
        self.phase = Phase::Idle;
        prs &= !(CIHB);
        if let Some(s) = prog {
            // Programming: DAT0 low, TC when the card lets go.
            self.set(0x24, prs | CDIHB | DLA);
            self.start_busy(Busy::Prog, s);
            return;
        }
        self.set(0x24, prs);
        self.irq_status(TC);
    }

    pub fn read(&mut self, off: u32, size: u32) -> Option<u32> {
        if off >= 0x100 || off & 3 != 0 || size != 4 {
            // Byte/word reads of a register: from the register image.
            if off < 0x100 {
                let w = self.r(off & !3);
                let sh = 8 * (4 - size - (off & 3));
                let m = if size == 4 { u32::MAX } else { (1u32 << (8 * size)) - 1 };
                return Some((w >> sh) & m);
            }
            return None;
        }
        if off == 0x20 {
            // DATPORT: the next word of the transfer.
            let mut v = 0u32;
            for _ in 0..4 {
                v = v << 8 | self.rx.pop_front().unwrap_or(0) as u32;
            }
            if self.phase == Phase::Read && self.rx.is_empty() {
                self.phase = Phase::Idle;
                let prs = self.r(0x24) & !BREN;
                self.set(0x24, prs);
            }
            return Some(v);
        }
        Some(self.r(off))
    }

    pub fn write(&mut self, off: u32, size: u32, v: u32) -> bool {
        if off >= 0x100 {
            return false;
        }
        let (off, v) = if size == 4 {
            (off, v)
        } else {
            // Merge a narrow write into the register.
            let reg = off & !3;
            let sh = 8 * (4 - size - (off & 3));
            let m = if size == 4 { u32::MAX } else { ((1u32 << (8 * size)) - 1) << sh };
            (reg, (self.r(reg) & !m) | ((v << sh) & m))
        };
        match off {
            0x0C => self.issue(v),
            0x20 => {
                if self.phase == Phase::Write {
                    self.wx.extend_from_slice(&v.to_be_bytes());
                    if self.wx.len() >= self.want {
                        self.finish_data();
                    }
                } else {
                    self.pattern = v;
                }
            }
            0x2C => {
                // SYSCTL: INITA and the resets complete at once.
                if v & (1 << 24) != 0 {
                    self.phase = Phase::Idle;
                    self.rx.clear();
                    self.wx.clear();
                    self.set(0x30, 0);
                }
                self.set(0x2C, v & !SYSCTL_SELFCLEAR);
            }
            0x30 => {
                let cur = self.r(0x30);
                self.set(0x30, cur & !v);
            }
            0x24 | 0x10 | 0x14 | 0x18 | 0x1C | 0x40 | 0xFC => {}
            _ => self.set(off, v),
        }
        true
    }
}

mod sector_map {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S: Serializer>(m: &BTreeMap<u32, Box<[u8; 512]>>, s: S) -> Result<S::Ok, S::Error> {
        let v: Vec<(u32, &[u8])> = m.iter().map(|(k, d)| (*k, &d[..])).collect();
        v.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<u32, Box<[u8; 512]>>, D::Error> {
        let v: Vec<(u32, Vec<u8>)> = Vec::deserialize(d)?;
        Ok(v.into_iter()
            .map(|(k, d)| {
                let mut b = Box::new([0u8; 512]);
                b[..d.len().min(512)].copy_from_slice(&d[..d.len().min(512)]);
                (k, b)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XFER_DATA: u32 = 1 << 21;
    const XFER_READ: u32 = 1 << 4;
    const R1: u32 = 2 << 16;
    const R1B: u32 = 3 << 16;

    /// A controller with unit timing on a 1 MHz clock (one tick a
    /// microsecond), 4-bit bus at SYSCTL 0x0E010C (base / 2).
    fn unit() -> Esdhc {
        let mut e = Esdhc::new(Card::new());
        e.timing = Some(CardTiming { write_s: 1e-3, block_s: 10e-6, read_s: 100e-6, erase_s: 2e-3, r1b_s: 50e-6, base_hz: 100e6 });
        e.ips = 1e6;
        e.write(0x2C, 4, 0x000E_010C);
        e.write(0x28, 4, 0x22);
        e
    }

    fn tc(e: &Esdhc) -> bool {
        e.r(0x30) & TC != 0
    }

    fn write_blocks(e: &mut Esdhc, sector: u32, blocks: u32) {
        e.write(0x30, 4, u32::MAX);
        e.write(0x04, 4, blocks << 16 | 0x200);
        e.write(0x08, 4, sector);
        let cmd = if blocks > 1 { 25 } else { 24 };
        e.write(0x0C, 4, cmd << 24 | XFER_DATA | R1 | if blocks > 1 { 1 << 5 | 1 << 1 } else { 0 });
        for i in 0..blocks * 128 {
            e.write(0x20, 4, 0x1234_0000 | i);
        }
    }

    #[test]
    fn params() {
        let mut t = CardTiming::UNIT;
        t.apply("dmiss=30, emmc_write_us=200,emmc_base_mhz=50").unwrap();
        assert!((t.write_s - 200e-6).abs() < 1e-12 && t.base_hz == 50e6);
        assert!(t.apply("emmc_nope=1").is_err());
        assert!(t.apply("emmc_block_us=-1").is_err());
        assert!(t.apply("emmc_base_mhz=0").is_err());
    }

    #[test]
    fn default_mode_is_instant() {
        let mut e = Esdhc::new(Card::new());
        assert!(e.timing.is_none());
        write_blocks(&mut e, 0x100, 1);
        assert!(tc(&e));
        assert!(e.ready_at.is_none());
        assert_eq!(e.card.writes, 1);
    }

    #[test]
    fn write_holds_dat0_until_programmed() {
        let mut e = unit();
        e.now = 1000;
        write_blocks(&mut e, 0x100, 32);
        // 16 KB over 4 bits at 50 MHz: 655 us; then 1 ms + 32 x 10 us.
        let at = e.ready_at.expect("busy");
        assert_eq!(at, 1000 + 655 + 1000 + 320);
        assert!(!tc(&e));
        assert_eq!(e.r(0x24) & DAT0, 0, "DAT0 low while programming");
        assert_eq!(e.card.writes, 32, "the data is on the card at once");
        // CMD13 meanwhile: state prg, not ready for data; the busy goes on.
        e.write(0x0C, 4, 13 << 24 | R1);
        assert_eq!(e.r(0x10), 0x0E00);
        assert_eq!(e.ready_at, Some(at));
        assert!(!e.service(at - 1));
        assert!(e.service(at));
        assert!(tc(&e));
        assert_ne!(e.r(0x24) & DAT0, 0);
        assert_eq!(e.r(0x24) & (CDIHB | DLA), 0);
        e.write(0x0C, 4, 13 << 24 | R1);
        assert_eq!(e.r(0x10), 0x0900);
    }

    #[test]
    fn read_lands_after_access_and_wire() {
        let mut e = unit();
        e.card.write(7, &[0xAB; 512]);
        e.write(0x04, 4, 1 << 16 | 0x200);
        e.write(0x08, 4, 7);
        e.write(0x0C, 4, 17 << 24 | XFER_DATA | XFER_READ | R1);
        assert!(!e.dma_request(), "no data before the card sends it");
        assert_eq!(e.r(0x30) & (BRR | TC), 0);
        // 100 us access + 512 bytes at 25 MB/s (20.48 us).
        assert_eq!(e.ready_at, Some(120));
        assert!(e.service(120));
        assert!(e.dma_request());
        assert_eq!(e.r(0x30) & (BRR | TC), BRR | TC);
        assert_eq!(e.read(0x20, 4), Some(0xABAB_ABAB));
    }

    #[test]
    fn r1b_commands_busy() {
        let mut e = unit();
        e.write(0x08, 4, 0);
        e.write(0x0C, 4, 38 << 24 | R1B);
        assert_eq!(e.ready_at, Some(2000));
        assert_eq!(e.r(0x30) & CC, CC, "the response comes at once");
        assert_eq!(e.r(0x24) & DAT0, 0);
        assert!(!e.dat0_rose);
        assert!(e.service(2000));
        assert!(e.dat0_rose, "DAT0's edge for EPORT IRQ3");
        assert_eq!(e.r(0x30) & TC, 0, "no TC after an R1b busy");
        assert_ne!(e.r(0x24) & DAT0, 0);
        e.write(0x0C, 4, 12 << 24 | R1B);
        assert_eq!(e.ready_at, Some(2000 + 50));
    }

    #[test]
    fn a_new_data_command_ends_a_busy_the_firmware_skipped() {
        let mut e = unit();
        write_blocks(&mut e, 0x100, 1);
        assert!(e.busy());
        write_blocks(&mut e, 0x101, 1);
        assert!(e.busy(), "the second write's own busy");
        assert_eq!(e.card.writes, 2);
    }

    #[test]
    fn settle_after_restore_finishes_a_busy() {
        let mut e = unit();
        write_blocks(&mut e, 0x100, 1);
        // What a snapshot keeps: the registers, not the busy.
        e.busy = Busy::None;
        e.ready_at = None;
        e.settle_after_restore();
        assert!(tc(&e));
        assert_eq!(e.r(0x24) & (CDIHB | DLA), 0);
    }
}
