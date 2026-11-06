//! The address space: DDR, internal SRAM, sparse backing for everything else,
//! and the MMIO dispatch into `crate::io::Io`.
//!
//! MCF54418RM Table 1-2 gives the DDR controller 0x4000_0000-0x7FFF_FFFF.
//! The controller decodes only the address bits the fitted part needs, so the
//! 128 MB repeats through the whole window, and the firmware relies on it:
//! it keeps its stack at 0x47FFxxxx and reads DMA buffers through 0x48000000
//! and up, which ACR0 leaves cache-inhibited -- an uncached view of the same
//! memory. The 64 KB internal SRAM likewise repeats through 0x80000000-
//! 0x8BFFFFFF.
//!
//! Every other address that no peripheral model claims reads back what was
//! last written to it (zero at first), a page at a time. That matches how the
//! reference emulator ran: an unmodelled register page is plain memory. The
//! pages touched that way are recorded, because an unmodelled register the
//! firmware waits on looks exactly like a firmware bug.

use crate::fast::Op;
use crate::io::Io;
use std::collections::{BTreeMap, HashMap};

pub const DDR_BASE: u32 = 0x4000_0000;
pub const SRAM_BASE: u32 = 0x8000_0000;
pub const SRAM_SIZE: usize = 0x1_0000;
const SPARSE_PAGE: u32 = 0x1000;

/// Strict mode's unmodelled pages that stock firmware touches as a matter of
/// course: peripheral registers with no model here, [lo, hi) page ranges.
/// Measured on stock OS 1.73 (cold boot, play, machine lists, SAMPLES browsing,
/// project save and load; 2026-10-09, docs/RYTM-PORT.md).
const STRICT_ALLOWED: &[(u32, u32)] = &[
    // First stock PC seen; which module each page is, is not checked here.
    (0xEC07_0000, 0xEC07_5000), // 0x40001ca6, 0x400020ac (boot)
    (0xEC09_0000, 0xEC09_1000), // 0x40000870 (boot)
    (0xEC09_4000, 0xEC09_5000), // 0x40000690; 0x4013579a in the audio path
    (0xFC03_C000, 0xFC03_D000), // 0x4007a482, polled
    (0xFC04_0000, 0xFC04_1000), // 0x40000888, writes to 0xFC04002D/2F
    (0xFC05_C000, 0xFC05_D000), // 0x401368c6 (flash reads)
    (0xFC09_0000, 0xFC09_1000), // 0x40000690 (boot)
    (0xFC09_4000, 0xFC09_5000), // 0x40000946; 0x4013f208, polled
    (0xFC0B_0000, 0xFC0B_4000), // the USB controller (usbdev.rs models part)
    (0xFC0C_8000, 0xFC0C_9000), // SSI1's page, 0x4000561c (setup)
];

/// Strict mode: the stock instructions that read or write page 0 (address
/// 0..0xFFF), allowed from these PCs only, so that a NULL access anywhere else
/// still faults. Measured in stock runs: 0x4012119a (`clr.w (a0)`, a0 = 0,
/// half a million times a run), 0x4012185c (`move.b 0x0,d1`), 0x400afe1e
/// (`tst.b 0x8`). The others are the stock image's remaining absolute
/// references below 0x1000 in code (objdump of stock 1.73, inferred).
const STRICT_PAGE0_PCS: &[u32] = &[
    0x4012_119A, 0x4012_185C, 0x400A_FE1E, 0x4009_8AD2, 0x4009_8AEE, 0x400A_FFD6, 0x4010_53F0, 0x4010_56F0,
    0x4010_5D2E, 0x4010_B07A, 0x4010_CCBC, 0x4012_19D8, 0x4013_B882, 0x4015_4322, 0x4015_43A8,
];

pub struct Bus {
    pub ddr: Vec<u8>,
    ddr_mask: u32,
    pub sram: Vec<u8>,
    pub sparse: HashMap<u32, Box<[u8; SPARSE_PAGE as usize]>>,
    /// Sparse pages first touched, page base -> (first pc, access count).
    pub unmapped_touch: BTreeMap<u32, (u32, u64)>,
    /// Strict mode (`DTEMU_STRICT=1`): an access to an unmapped page (one no
    /// model claims and `STRICT_ALLOWED` does not list) or to the SRAM mirror
    /// beyond the real 64 KB is an access error. The access completes as
    /// before (reads from the backing page or mirror); the CPU then takes
    /// vector 2 with the faulting instruction's PC (`Cpu::strict_fault`).
    pub strict: bool,
    /// Strict mode raises the access error (`DTEMU_STRICT=1`), or only logs
    /// (`DTEMU_STRICT=log`).
    pub strict_raise: bool,
    /// The span of the SRAM window that is plain SRAM: 0x0C00_0000 (the
    /// mirror) normally, 64 KB in strict mode.
    sram_span: u32,
    /// Strict violations not yet raised: the first one's (pc, address).
    pub strict_pending: Option<(u32, u32)>,
    /// Every strict violation, (pc, page, write) -> (first address, count).
    pub strict_log: BTreeMap<(u32, u32, bool), (u32, u64)>,
    pub io: Io,
    /// The Rytm's analog voices, modelled from the control frame (`analog.rs`).
    /// None on the Digitakt. Not part of a snapshot: envelopes restart silent.
    pub analog: Option<crate::analog::Analog>,
    /// The control frame as the voice boards got it last (machine.rs, the profile's `voice_frame_sent`): the analog
    /// model reads it instead of the live frame when set.
    pub analog_sent: Option<Vec<u8>>,
    /// The Analog Four MKII's voices, modelled from its voice frame (a4voice.rs).
    pub a4: Option<crate::a4voice::A4Voices>,
    /// The Syntakt's analog voices and analog FX block (stanalog.rs).
    pub st: Option<crate::stanalog::StAnalog>,
    /// The MKI units' parallel NOR flash at 0 and persistent memory at
    /// 0x20000000 (nor.rs); None on the MKII units and the Digitakt, whose
    /// flash is SPI.
    pub nor: Option<crate::nor::Nor>,
    /// The MKI codec's control port on DSPI1 (mk1.rs); None elsewhere (the
    /// MKII units drive their voices on DSPI1).
    pub codec: Option<crate::mk1::CodecSpi>,
    /// The serial flash on DSPI0 (spiflash.rs); None on the MKI units.
    pub spi: Option<crate::spiflash::SpiFlash>,
    /// The PC of the instruction being executed, for diagnostics.
    pub pc: u32,
    /// A DDR write watchpoint: [lo, hi) as physical offsets. Hits are logged
    /// as (pc, address, size, value).
    pub watch: Option<(u32, u32)>,
    pub watch_hits: u64,
    pub watch_log: Vec<(u32, u32, u32, u32)>,
    /// The watchpoint also logs DDR reads (the CPU's; DMA is not seen).
    pub watch_reads: bool,
    /// An SRAM write watchpoint: [lo, hi) as SRAM offsets, logged into
    /// `watch_log` as the DDR one. Seen by the interpreter only (compiled
    /// code stores to SRAM pages directly): run with DTEMU_NO_JIT=1.
    pub sram_watch: Option<(u32, u32)>,
    /// Set while the eDMA engine runs, so a register write it makes cannot
    /// start the engine again underneath itself.
    pub in_dma: bool,
    /// The block cache over [CODE_BASE, CODE_BASE + span): halfword ->
    /// block id (0 = none), the blocks, and which halfwords hold code.
    blk_map: Vec<u32>,
    pub(crate) blk_arena: Vec<Block>,
    code_bits: Vec<u64>,
    pub(crate) flush_pending: bool,
    /// Compiled code's page tables (see `jit_pages`): per 64 KB of the
    /// address space, the host address of plain memory there or 0, for
    /// reads and for writes; and what they were built for.
    jit_rd_pages: Vec<usize>,
    jit_wr_pages: Vec<usize>,
    jit_pages_for: (usize, usize, u32),
    pub icache_flushes: u64,
    icache_span: u32,
    pub icache_on: bool,
    pub icache_decodes: u64,
    pub icache_invalidations: u64,
    /// The cache model (`DTEMU_CACHE`, cache.rs): None normally.
    pub cache: Option<Box<crate::cache::Caches>>,
}

/// A predecoded block, and its compiled code once it ran hot.
pub struct Block {
    pub ops: Box<[Op]>,
    pub hits: u32,
    pub code: Option<crate::jit::BlockFn>,
    /// The compiler declined it; it stays interpreted.
    pub no_jit: bool,
    /// Sent to the background compiler.
    pub queued: bool,
}

/// Where MAIN OS's text starts; the cache covers the image from here.
pub const CODE_BASE: u32 = 0x4000_0400;

impl Bus {
    pub fn new(ddr_mb: usize) -> Bus {
        let size = ddr_mb << 20;
        assert!(size.is_power_of_two());
        Bus {
            ddr: vec![0; size],
            ddr_mask: (size - 1) as u32,
            sram: vec![0; SRAM_SIZE],
            sparse: HashMap::new(),
            unmapped_touch: BTreeMap::new(),
            strict: false,
            strict_raise: true,
            sram_span: 0x0C00_0000,
            strict_pending: None,
            strict_log: BTreeMap::new(),
            io: Io::new(),
            analog: None,
            analog_sent: None,
            a4: None,
            st: None,
            nor: None,
            codec: None,
            spi: None,
            pc: 0,
            watch: None,
            watch_hits: 0,
            watch_log: Vec::new(),
            watch_reads: false,
            sram_watch: None,
            in_dma: false,
            blk_map: Vec::new(),
            jit_rd_pages: Vec::new(),
            jit_wr_pages: Vec::new(),
            jit_pages_for: (0, 0, 0),
            blk_arena: Vec::new(),
            code_bits: Vec::new(),
            flush_pending: false,
            icache_flushes: 0,
            icache_span: 0,
            icache_on: false,
            icache_decodes: 0,
            icache_invalidations: 0,
            cache: None,
        }
    }

    /// Turn strict mode on or off (`strict`). Rebuilds compiled code's page
    /// tables, which map the SRAM mirror otherwise.
    pub fn set_strict(&mut self, on: bool) {
        self.strict = on;
        self.sram_span = if on { SRAM_SIZE as u32 } else { 0x0C00_0000 };
        self.jit_rd_pages.clear();
    }

    /// A strict-mode violation at `a`: log it, and have the CPU raise an
    /// access error after the instruction (not for eDMA's own accesses).
    #[cold]
    fn strict_hit(&mut self, a: u32, write: bool) {
        let e = self.strict_log.entry((self.pc, a & !(SPARSE_PAGE - 1), write)).or_insert((a, 0));
        e.1 += 1;
        if self.strict_raise && !self.in_dma && self.strict_pending.is_none() {
            self.strict_pending = Some((self.pc, a));
        }
    }

    #[inline(always)]
    pub fn ddr_off(&self, addr: u32) -> Option<usize> {
        if addr & 0xC000_0000 == 0x4000_0000 {
            Some((addr & self.ddr_mask) as usize)
        } else {
            None
        }
    }

    /// Cover `span` bytes of code from CODE_BASE with the block cache.
    pub fn icache_enable(&mut self, span: u32) {
        let n = (span / 2) as usize + 1;
        self.blk_map = vec![0; n];
        self.code_bits = vec![0; n / 64 + 1];
        self.blk_arena.clear();
        self.icache_span = span;
        self.icache_on = true;
    }

    /// Drop every block (at the next block boundary, never under one).
    pub fn icache_flush(&mut self) {
        self.flush_pending = true;
    }

    /// Apply a pending flush. Only between blocks.
    #[inline(always)]
    pub fn icache_settle(&mut self) {
        if self.flush_pending {
            self.flush_pending = false;
            self.blk_map.iter_mut().for_each(|x| *x = 0);
            self.code_bits.iter_mut().for_each(|x| *x = 0);
            self.blk_arena.clear();
            self.icache_flushes += 1;
        }
    }

    /// -> the index of the block starting at `pc`, if one is built.
    #[inline(always)]
    pub fn block_at(&self, pc: u32) -> Option<usize> {
        let o = pc.wrapping_sub(CODE_BASE);
        if o >= self.icache_span || o & 1 != 0 {
            return None;
        }
        let id = self.blk_map[(o >> 1) as usize];
        if id == 0 {
            return None;
        }
        Some((id - 1) as usize)
    }

    /// DDR offset of `[a, a + len)` when all of it is plain DDR that no
    /// watchpoint or cached code covers, so it can be moved directly.
    #[inline(always)]
    pub fn ddr_plain(&self, a: u32, len: u32) -> Option<usize> {
        let o = self.ddr_off(a)?;
        let end = a.wrapping_add(len - 1);
        if self.ddr_off(end)? != o + len as usize - 1 || self.watch.is_some() || self.cache.is_some() {
            return None;
        }
        // Wholly below or above the cached code, or a store could hit it.
        let (lo, hi) = (CODE_BASE - 0x4000_0000, CODE_BASE - 0x4000_0000 + self.icache_span);
        let o32 = o as u32;
        if o32 + len > lo && o32 < hi {
            return None;
        }
        Some(o)
    }

    /// Page tables for compiled code: entry `a >> 16` is the host address
    /// of the 64 KB page holding `a` when it is plain memory -- DDR (any
    /// alias) or SRAM -- and 0 otherwise. The write table leaves out DDR
    /// pages that overlap cached code, whose stores `check_code` must see.
    /// An access through a non-zero entry that stays inside its page is
    /// one `read*` / `write*` would make directly. Rebuilt when DDR or SRAM
    /// move (a snapshot load) or the code window changes. -> (reads, writes)
    pub fn jit_pages(&mut self) -> (*const usize, *const usize) {
        let key = (self.ddr.as_ptr() as usize, self.sram.as_ptr() as usize, self.icache_span);
        if self.jit_rd_pages.is_empty() || self.jit_pages_for != key {
            let (lo, hi) = (CODE_BASE - 0x4000_0000, CODE_BASE - 0x4000_0000 + self.icache_span);
            let mut rd = vec![0usize; 0x1_0000];
            let mut wr = vec![0usize; 0x1_0000];
            for page in 0..0x1_0000u32 {
                let a = page << 16;
                if let Some(o) = self.ddr_off(a) {
                    if o + 0x1_0000 <= self.ddr.len() {
                        let host = self.ddr.as_ptr() as usize + o;
                        rd[page as usize] = host;
                        let (s, e) = (o as u32, o as u32 + 0x1_0000);
                        if self.icache_span == 0 || e <= lo || s >= hi {
                            wr[page as usize] = host;
                        }
                    }
                } else if self.sram_off(a).is_some() {
                    rd[page as usize] = self.sram.as_ptr() as usize;
                    wr[page as usize] = self.sram.as_ptr() as usize;
                }
            }
            self.jit_rd_pages = rd;
            self.jit_wr_pages = wr;
            self.jit_pages_for = key;
        }
        (self.jit_rd_pages.as_ptr(), self.jit_wr_pages.as_ptr())
    }

    #[inline(always)]
    pub fn in_code_window(&self, pc: u32) -> bool {
        self.icache_on && pc.wrapping_sub(CODE_BASE) < self.icache_span && pc & 1 == 0
    }

    /// Record a block that starts at `pc` and covers [pc, end).
    pub fn block_store(&mut self, pc: u32, end: u32, ops: Box<[Op]>) -> usize {
        let o = pc.wrapping_sub(CODE_BASE);
        self.blk_arena.push(Block { ops, hits: 0, code: None, no_jit: false, queued: false });
        let id = self.blk_arena.len() as u32;
        self.blk_map[(o >> 1) as usize] = id;
        let e = end.wrapping_sub(CODE_BASE).min(self.icache_span);
        for h in (o >> 1)..e.div_ceil(2) {
            let h = h as usize;
            self.code_bits[h >> 6] |= 1 << (h & 63);
        }
        self.icache_decodes += 1;
        self.blk_arena.len() - 1
    }

    /// A store to physical DDR offset `o`: if it lands on code some block
    /// was built from, every block goes (at the next block boundary).
    #[inline(always)]
    fn check_code(&mut self, o: u32, n: u32) {
        let rel = o.wrapping_sub(CODE_BASE - 0x4000_0000);
        if rel < self.icache_span {
            let lo = rel >> 1;
            let hi = ((rel + n).min(self.icache_span) + 1) >> 1;
            for i in lo..hi {
                let i = i as usize;
                if self.code_bits[i >> 6] & (1 << (i & 63)) != 0 {
                    self.icache_invalidations += 1;
                    self.flush_pending = true;
                    return;
                }
            }
        }
    }

    /// Offset into SRAM for an address in its 0x80000000-0x8BFFFFFF window.
    /// Strict mode: only the real 64 KB.
    #[inline(always)]
    fn sram_off(&self, addr: u32) -> Option<usize> {
        if addr.wrapping_sub(SRAM_BASE) < self.sram_span {
            Some((addr as usize) & (SRAM_SIZE - 1))
        } else {
            None
        }
    }

    #[inline(always)]
    fn is_io(addr: u32) -> bool {
        addr >= 0xE000_0000
    }

    #[allow(clippy::nonminimal_bool)]
    fn sparse_page(&mut self, addr: u32, write: bool) -> &mut [u8; SPARSE_PAGE as usize] {
        let base = addr & !(SPARSE_PAGE - 1);
        if self.strict
            && !STRICT_ALLOWED.iter().any(|&(lo, hi)| (lo..hi).contains(&base))
            && !(base == 0 && STRICT_PAGE0_PCS.contains(&self.pc))
            && !(self.nor.is_some() && base.wrapping_sub(crate::nor::PRAM_BASE) < crate::nor::PRAM_SIZE)
        {
            self.strict_hit(addr, write);
        }
        if !self.sparse.contains_key(&base) {
            // First touch of a page nothing models: worth knowing about (the
            // MKI's CS1 memory is modelled as these pages).
            if !(self.nor.is_some() && base.wrapping_sub(crate::nor::PRAM_BASE) < crate::nor::PRAM_SIZE) {
                self.unmapped_touch.insert(base, (self.pc, 0));
            }
            self.sparse.insert(base, Box::new([0; SPARSE_PAGE as usize]));
        }
        // Count every access to unmodelled I/O (the first pc stays).
        if base >= 0xE000_0000 {
            self.unmapped_touch.entry(base).or_insert((self.pc, 0)).1 += 1;
        }
        self.sparse.get_mut(&base).unwrap()
    }

    /// Plain-memory byte read, bypassing peripherals.
    pub fn peek8(&self, addr: u32) -> u8 {
        if let Some(o) = self.ddr_off(addr) {
            if let Some(b) = self.cache.as_ref().and_then(|c| c.peek(addr)) {
                return b;
            }
            return self.ddr[o];
        }
        if let Some(d) = self.io.dsp.as_ref() {
            if addr.wrapping_sub(crate::syntakt::MBOX_BASE) < crate::syntakt::MBOX_SIZE {
                return d.read(addr, 1) as u8;
            }
        }
        if let Some(d) = self.io.dsp_side.as_ref() {
            if addr < crate::syntakt::MBOX_SIZE {
                return d.read(addr, 1) as u8;
            }
        }
        if self.nor.is_some() && addr < crate::nor::NOR_SIZE {
            return crate::nor::byte(&self.sparse, addr);
        }
        if (SRAM_BASE..0x8C00_0000).contains(&addr) {
            return self.sram[(addr as usize) & (SRAM_SIZE - 1)];
        }
        let base = addr & !(SPARSE_PAGE - 1);
        match self.sparse.get(&base) {
            Some(p) => p[(addr - base) as usize],
            None => 0,
        }
    }

    /// Plain-memory byte write, bypassing peripherals.
    pub fn poke8(&mut self, addr: u32, v: u8) {
        if let Some(o) = self.ddr_off(addr) {
            if let Some(c) = self.cache.as_mut() {
                c.poke(addr, v);
            }
            self.check_code(o as u32, 1);
            self.ddr[o] = v;
            return;
        }
        if let Some(d) = self.io.dsp.as_mut() {
            if addr.wrapping_sub(crate::syntakt::MBOX_BASE) < crate::syntakt::MBOX_SIZE {
                return d.write(addr, 1, v as u32);
            }
        }
        if let Some(d) = self.io.dsp_side.as_mut() {
            if addr < crate::syntakt::MBOX_SIZE {
                return d.write(addr, 1, v as u32);
            }
        }
        if (SRAM_BASE..0x8C00_0000).contains(&addr) {
            self.sram[(addr as usize) & (SRAM_SIZE - 1)] = v;
            return;
        }
        let base = addr & !(SPARSE_PAGE - 1);
        let p = self.sparse.entry(base).or_insert_with(|| Box::new([0; SPARSE_PAGE as usize]));
        p[(addr - base) as usize] = v;
    }

    pub fn peek16(&self, a: u32) -> u16 {
        (self.peek8(a) as u16) << 8 | self.peek8(a.wrapping_add(1)) as u16
    }

    pub fn peek32(&self, a: u32) -> u32 {
        if let (Some(o), None) = (self.ddr_off(a), &self.cache) {
            if o + 4 <= self.ddr.len() {
                return u32::from_be_bytes(self.ddr[o..o + 4].try_into().unwrap());
            }
        }
        (self.peek16(a) as u32) << 16 | self.peek16(a.wrapping_add(2)) as u32
    }

    pub fn poke16(&mut self, a: u32, v: u16) {
        self.poke8(a, (v >> 8) as u8);
        self.poke8(a.wrapping_add(1), v as u8);
    }

    pub fn poke32(&mut self, a: u32, v: u32) {
        self.poke16(a, (v >> 16) as u16);
        self.poke16(a.wrapping_add(2), v as u16);
    }

    pub fn peek_bytes(&self, a: u32, n: usize) -> Vec<u8> {
        (0..n as u32).map(|i| self.peek8(a.wrapping_add(i))).collect()
    }

    pub fn poke_bytes(&mut self, a: u32, data: &[u8]) {
        if let Some(c) = self.cache.as_mut() {
            for (i, b) in data.iter().enumerate() {
                c.poke(a.wrapping_add(i as u32), *b);
            }
        }
        if let Some(o) = self.ddr_off(a) {
            if o + data.len() <= self.ddr.len() {
                if self.icache_on {
                    for k in (0..data.len()).step_by(2) {
                        self.check_code((o + k) as u32, 2);
                    }
                }
                self.ddr[o..o + data.len()].copy_from_slice(data);
                return;
            }
        }
        for (i, &b) in data.iter().enumerate() {
            self.poke8(a.wrapping_add(i as u32), b);
        }
    }

    // -- CPU accesses, through the cache model when it is on ------------------

    #[inline(always)]
    pub fn read8(&mut self, a: u32) -> u8 {
        if self.cache.is_some() {
            return self.cached_read(a, 1) as u8;
        }
        self.read8_mem(a)
    }

    #[inline(always)]
    pub fn read16(&mut self, a: u32) -> u16 {
        if self.cache.is_some() {
            return self.cached_read(a, 2) as u16;
        }
        self.read16_mem(a)
    }

    #[inline(always)]
    pub fn read32(&mut self, a: u32) -> u32 {
        if self.cache.is_some() {
            return self.cached_read(a, 4);
        }
        self.read32_mem(a)
    }

    #[inline(always)]
    pub fn write8(&mut self, a: u32, v: u8) {
        if self.cache.is_some() {
            return self.cached_write(a, 1, v as u32);
        }
        self.write8_mem(a, v)
    }

    #[inline(always)]
    pub fn write16(&mut self, a: u32, v: u16) {
        if self.cache.is_some() {
            return self.cached_write(a, 2, v as u32);
        }
        self.write16_mem(a, v)
    }

    #[inline(always)]
    pub fn write32(&mut self, a: u32, v: u32) {
        if self.cache.is_some() {
            return self.cached_write(a, 4, v);
        }
        self.write32_mem(a, v)
    }

    /// An instruction fetch: through the I-cache in cache mode (filled from
    /// memory, never the D-cache), else a plain read.
    #[inline(always)]
    pub fn ifetch16(&mut self, a: u32) -> u16 {
        if self.cache.is_some() {
            return self.cached_ifetch(a, 2) as u16;
        }
        self.read16(a)
    }

    #[inline(always)]
    pub fn ifetch32(&mut self, a: u32) -> u32 {
        if self.cache.is_some() {
            return self.cached_ifetch(a, 4);
        }
        self.read32(a)
    }

    fn mem_read(&mut self, a: u32, n: u32) -> u32 {
        match n {
            1 => self.read8_mem(a) as u32,
            2 => self.read16_mem(a) as u32,
            _ => self.read32_mem(a),
        }
    }

    #[cold]
    #[inline(never)]
    fn cached_read(&mut self, a: u32, n: u32) -> u32 {
        // eDMA (in_dma) reads memory, past the caches.
        if !self.in_dma && self.ddr_off(a).is_some() && self.ddr_off(a.wrapping_add(n - 1)).is_some() {
            let c = self.cache.as_mut().unwrap();
            if let Some(v) = c.read(&mut self.ddr, self.ddr_mask, a, n) {
                if self.watch_reads {
                    self.check_watch(a, n, v);
                }
                return v;
            }
        }
        self.mem_read(a, n)
    }

    #[cold]
    #[inline(never)]
    fn cached_write(&mut self, a: u32, n: u32, v: u32) {
        if !self.in_dma && self.ddr_off(a).is_some() && self.ddr_off(a.wrapping_add(n - 1)).is_some() {
            let c = self.cache.as_mut().unwrap();
            if c.write(&mut self.ddr, self.ddr_mask, a, n, v) {
                self.check_watch(a, n, v);
                return;
            }
        }
        match n {
            1 => self.write8_mem(a, v as u8),
            2 => self.write16_mem(a, v as u16),
            _ => self.write32_mem(a, v),
        }
    }

    #[cold]
    #[inline(never)]
    fn cached_ifetch(&mut self, a: u32, n: u32) -> u32 {
        if self.ddr_off(a).is_some() && self.ddr_off(a.wrapping_add(n - 1)).is_some() {
            let pc = self.pc;
            let c = self.cache.as_mut().unwrap();
            if let Some(v) = c.ifetch(&self.ddr, self.ddr_mask, a, n, pc) {
                return v;
            }
        }
        // Not instruction-cacheable: memory itself, past the D-cache too.
        self.mem_read(a, n)
    }

    /// CPUSHL (`which`: 1 dc, 2 ic, 3 bc) with An = `an`; INTOUCH is `which` 0.
    pub fn cache_op(&mut self, which: u8, an: u32) {
        let in_ddr = self.ddr_off(an).is_some();
        if let Some(c) = self.cache.as_mut() {
            if which == 0 {
                if in_ddr {
                    c.intouch(&self.ddr, self.ddr_mask, an);
                }
            } else {
                c.cpushl(&mut self.ddr, self.ddr_mask, which, an);
            }
        }
    }

    // -- memory, past the caches ---------------------------------------------

    #[inline(always)]
    fn read8_mem(&mut self, a: u32) -> u8 {
        if let Some(o) = self.ddr_off(a) {
            if self.watch_reads {
                self.check_watch(a, 1, self.ddr[o] as u32);
            }
            return self.ddr[o];
        }
        if let Some(o) = self.sram_off(a) {
            return self.sram[o];
        }
        self.read_slow(a, 1) as u8
    }

    #[inline(always)]
    fn read16_mem(&mut self, a: u32) -> u16 {
        if let Some(o) = self.ddr_off(a) {
            if o + 2 <= self.ddr.len() {
                let v = u16::from_be_bytes([self.ddr[o], self.ddr[o + 1]]);
                if self.watch_reads {
                    self.check_watch(a, 2, v as u32);
                }
                return v;
            }
        }
        if let Some(o) = self.sram_off(a) {
            if o + 2 <= SRAM_SIZE {
                return u16::from_be_bytes([self.sram[o], self.sram[o + 1]]);
            }
        }
        self.read_slow(a, 2) as u16
    }

    #[inline(always)]
    fn read32_mem(&mut self, a: u32) -> u32 {
        if let Some(o) = self.ddr_off(a) {
            if o + 4 <= self.ddr.len() {
                let v = u32::from_be_bytes(self.ddr[o..o + 4].try_into().unwrap());
                if self.watch_reads {
                    self.check_watch(a, 4, v);
                }
                return v;
            }
        }
        if let Some(o) = self.sram_off(a) {
            if o + 4 <= SRAM_SIZE {
                return u32::from_be_bytes(self.sram[o..o + 4].try_into().unwrap());
            }
        }
        self.read_slow(a, 4)
    }

    #[inline(always)]
    fn write8_mem(&mut self, a: u32, v: u8) {
        if let Some(o) = self.ddr_off(a) {
            self.check_watch(a, 1, v as u32);
            self.check_code(o as u32, 1);
            self.ddr[o] = v;
            return;
        }
        if let Some(o) = self.sram_off(a) {
            self.check_sram_watch(o, a, 1, v as u32);
            self.sram[o] = v;
            return;
        }
        self.write_slow(a, 1, v as u32)
    }

    #[inline(always)]
    fn write16_mem(&mut self, a: u32, v: u16) {
        if let Some(o) = self.ddr_off(a) {
            if o + 2 <= self.ddr.len() {
                self.check_watch(a, 2, v as u32);
                self.check_code(o as u32, 2);
                self.ddr[o..o + 2].copy_from_slice(&v.to_be_bytes());
                return;
            }
        }
        if let Some(o) = self.sram_off(a) {
            if o + 2 <= SRAM_SIZE {
                self.check_sram_watch(o, a, 2, v as u32);
                self.sram[o..o + 2].copy_from_slice(&v.to_be_bytes());
                return;
            }
        }
        self.write_slow(a, 2, v as u32)
    }

    #[inline(always)]
    fn write32_mem(&mut self, a: u32, v: u32) {
        if let Some(o) = self.ddr_off(a) {
            if o + 4 <= self.ddr.len() {
                self.check_watch(a, 4, v);
                self.check_code(o as u32, 4);
                self.ddr[o..o + 4].copy_from_slice(&v.to_be_bytes());
                return;
            }
        }
        if let Some(o) = self.sram_off(a) {
            if o + 4 <= SRAM_SIZE {
                self.check_sram_watch(o, a, 4, v);
                self.sram[o..o + 4].copy_from_slice(&v.to_be_bytes());
                return;
            }
        }
        self.write_slow(a, 4, v)
    }

    #[inline(always)]
    fn check_sram_watch(&mut self, o: usize, a: u32, n: u32, v: u32) {
        if let Some((lo, hi)) = self.sram_watch {
            let o = o as u32;
            if o + n > lo && o < hi {
                self.watch_hits += 1;
                if self.watch_log.len() < 4096 {
                    self.watch_log.push((self.pc, a, n, v));
                }
            }
        }
    }

    #[inline(always)]
    fn check_watch(&mut self, a: u32, n: u32, v: u32) {
        if let Some((lo, hi)) = self.watch {
            let o = a & self.ddr_mask;
            if o + n > lo && o < hi {
                self.watch_hits += 1;
                if self.watch_log.len() < 4096 {
                    self.watch_log.push((self.pc, a, n, v));
                }
            }
        }
    }

    fn plain_read(&mut self, a: u32, size: u32) -> u32 {
        let mut v = 0u32;
        for i in 0..size {
            let x = a.wrapping_add(i);
            let b = if let Some(o) = self.ddr_off(x) {
                self.ddr[o]
            } else if (SRAM_BASE..0x8C00_0000).contains(&x) {
                if self.strict {
                    self.strict_hit(x, false);
                }
                self.sram[(x as usize) & (SRAM_SIZE - 1)]
            } else {
                let p = self.sparse_page(x, false);
                p[(x & (SPARSE_PAGE - 1)) as usize]
            };
            v = v << 8 | b as u32;
        }
        v
    }

    fn plain_write(&mut self, a: u32, size: u32, v: u32) {
        for i in 0..size {
            let x = a.wrapping_add(i);
            let b = (v >> (8 * (size - 1 - i))) as u8;
            if let Some(o) = self.ddr_off(x) {
                self.ddr[o] = b;
            } else if (SRAM_BASE..0x8C00_0000).contains(&x) {
                if self.strict {
                    self.strict_hit(x, true);
                }
                self.sram[(x as usize) & (SRAM_SIZE - 1)] = b;
            } else {
                let p = self.sparse_page(x, true);
                p[(x & (SPARSE_PAGE - 1)) as usize] = b;
            }
        }
    }

    #[cold]
    fn read_slow(&mut self, a: u32, size: u32) -> u32 {
        if let Some(d) = self.io.dsp.as_ref() {
            if a.wrapping_sub(crate::syntakt::MBOX_BASE) < crate::syntakt::MBOX_SIZE {
                return d.read(a, size);
            }
        }
        // The Syntakt DSP's view of the mailbox: FlexBus CS0 at 0.
        if let Some(d) = self.io.dsp_side.as_ref() {
            if a < crate::syntakt::MBOX_SIZE {
                return d.read(a, size);
            }
        }
        if let Some(n) = self.nor.as_ref() {
            if a < crate::nor::NOR_SIZE {
                return n.read(&self.sparse, a, size);
            }
        }
        if let Some(c) = self.codec.as_mut() {
            if a == crate::mk1::DSPI1_POPR && size == 4 {
                return c.pop();
            }
        }
        if let Some(f) = self.spi.as_mut() {
            if a == crate::spiflash::DSPI0_POPR && size == 4 {
                return f.pop();
            }
        }
        // SSI1 receive data: with the analog model on, each frame's words come
        // from it, rendered from the current control frame.
        if (a == 0xFC0C_8008 || a == 0xFC0C_800C) && self.io.ssi.input.is_empty() {
            if let Some(an) = self.analog.as_mut() {
                let o = (an.frame_addr as usize) & (SRAM_SIZE - 1);
                let dac = self.io.ssi.tx_last;
                let fx = (an.fx_addr as usize) & (SRAM_SIZE - 1);
                let inputs = crate::analog::Inputs {
                    audio: self.io.ssi.audio_in.pop_front().unwrap_or((0.0, 0.0)),
                    ext: self.io.ssi.ext_in.pop_front().unwrap_or((0.0, 0.0)),
                };
                let f = match &self.analog_sent {
                    Some(v) => &v[..],
                    None => &self.sram[o..o + crate::analog::FRAME_LEN],
                };
                let out = an.frame(f, &dac, &self.sram[fx..fx + 24], inputs);
                if self.io.ssi0.on {
                    // The Rytm MKI's 10-word receive frame: words 0..5 on
                    // SSI0, 6..9 on SSI1 [inferred: the MKII's order].
                    self.io.ssi0.input.extend(&out.rx[..6]);
                    self.io.ssi.input.extend(&out.rx[6..10]);
                } else {
                    self.io.ssi.input.extend(out.rx);
                }
                self.io.ssi.voices.push_back(out.mix);
                if self.io.ssi.voices.len() > 48_000 {
                    self.io.ssi.voices.pop_front();
                }
            } else if let Some(a4) = self.a4.as_mut() {
                // The A4's codec (docs/A4-PORT.md section 12) [measured slots,
                // inferred levels]: RX 0..3 each voice's signal before its VCA
                // (the OS scales it by the voice's level and its sends),
                // RX 4/5 EXT IN L/R, 6/7 unused here; TX 4/5 the effects'
                // return L/R, mixed with the analog dry path.
                let o = (a4.frame_addr as usize) & (SRAM_SIZE - 1);
                let out = a4.frame(&self.sram[o..o + crate::a4voice::FRAME_LEN]);
                // CV OUT A..D: frame words 56..59 (Io::cv_out; a CONTROL IN
                // patched to one reads it, Io::cv_in).
                for (n, w) in self.io.cv_out.iter_mut().enumerate() {
                    let i = 2 * (56 + n);
                    *w = u16::from_be_bytes([self.sram[o + i], self.sram[o + i + 1]]);
                }
                let ext = self.io.ssi.ext_in.pop_front().unwrap_or((0.0, 0.0));
                let w = |x: f32| (x.clamp(-1.0, 1.0) * crate::a4voice::RX_SCALE) as i32;
                let s = out.send;
                self.io.ssi.input.extend([w(s[0]), w(s[1]), w(s[2]), w(s[3]), w(ext.0), w(ext.1), 0, 0]);
                let tx = &self.io.ssi.tx_last;
                let ret = |x: i32| x as f32 / 8_388_608.0 * crate::a4voice::RETURN_GAIN;
                let mix = (out.mix.0 + ret(tx[4]), out.mix.1 + ret(tx[5]));
                self.io.ssi.voices.push_back(mix);
                if self.io.ssi.voices.len() > 48_000 {
                    self.io.ssi.voices.pop_front();
                }
            } else if let Some(st) = self.st.as_mut() {
                // The Syntakt (docs/SYNTAKT-PORT.md section 12): the frame from
                // the buffer eDMA channel 15 sends, the DAC frame in.
                let t = crate::stanalog::FRAME_CHAN * 32;
                let sa = u32::from_be_bytes(self.io.edma.tcd[t..t + 4].try_into().unwrap());
                let o = (crate::stanalog::StAnalog::frame_base(sa) as usize) & (SRAM_SIZE - 1);
                let out = st.frame(&self.sram[o..o + 4 * crate::stanalog::FRAME_WORDS], sa, &self.io.ssi.tx_last);
                self.io.ssi.input.extend(out.rx);
                // The analog part of the speakers' mix; the DAC's main pair
                // joins it (Machine::speaker_pair).
                self.io.ssi.voices.push_back(out.mix);
                if self.io.ssi.voices.len() > 48_000 {
                    self.io.ssi.voices.pop_front();
                }
            }
        }
        if crate::usbdev::is_usb(a) {
            let base = a & !0x3FFF;
            let cmd = self.plain_read(base + 0x140, 4);
            if let Some(v) = crate::usbdev::read(a & 0x3FFF, size, |o| if o == 0x140 { cmd } else { 0 }) {
                return v;
            }
        }
        // EPORT EPFR: the flags a model raised (Io::epfr) over plain memory.
        if a == 0xFC09_0006 && size == 1 {
            return self.plain_read(a, 1) | self.io.epfr as u32;
        }
        if Self::is_io(a) {
            // A model may answer; otherwise the register reads back as memory.
            let pc = self.pc;
            let (io, ddr) = (&mut self.io, &mut self.ddr);
            if let Some(v) = io.read(a, size, 0, pc, ddr) {
                return v;
            }
        }
        self.plain_read(a, size)
    }

    #[cold]
    fn write_slow(&mut self, a: u32, size: u32, v: u32) {
        if let Some(d) = self.io.dsp.as_mut() {
            if a.wrapping_sub(crate::syntakt::MBOX_BASE) < crate::syntakt::MBOX_SIZE {
                return d.write(a, size, v);
            }
        }
        if let Some(d) = self.io.dsp_side.as_mut() {
            if a < crate::syntakt::MBOX_SIZE {
                return d.write(a, size, v);
            }
        }
        if let Some(n) = self.nor.as_mut() {
            if a < crate::nor::NOR_SIZE {
                return n.write(&mut self.sparse, a, size, v);
            }
        }
        if let Some(c) = self.codec.as_mut() {
            if a == crate::mk1::DSPI1_PUSHR && size == 4 {
                return c.push(v);
            }
        }
        if let Some(f) = self.spi.as_mut() {
            if a == crate::spiflash::DSPI0_PUSHR && size == 4 {
                return f.push(v, self.io.now, self.io.ips);
            }
            // MCR stays plain memory too.
            if a == crate::spiflash::DSPI0_MCR && size == 4 {
                f.mcr(v);
            }
        }
        if crate::usbdev::is_usb(a) {
            let base = a & !0x3FFF;
            let plain: Vec<u32> = (0..0x80).map(|k| self.plain_read(base + 0x100 + 4 * k, 4)).collect();
            let get = |o: u32| if (0x100..0x300).contains(&o) { plain[((o - 0x100) / 4) as usize] } else { 0 };
            if let Some(ws) = crate::usbdev::write(a & 0x3FFF, size, v, get) {
                for (o, x) in ws {
                    self.plain_write(base + o, 4, x);
                }
                self.io.usb_irq = crate::usbhost::line(self);
                self.io.update_irq();
                return;
            }
        }
        if a == 0xFC09_0006 && size == 1 && self.io.epfr & v as u8 != 0 {
            // Writing 1 clears an EPORT flag.
            self.io.epfr &= !(v as u8);
            self.io.update_irq();
        }
        if Self::is_io(a) {
            let pc = self.pc;
            let (io, ddr) = (&mut self.io, &mut self.ddr);
            if io.write(a, size, v, pc, ddr) {
                if self.io.edma.kick != 0 && !self.in_dma {
                    self.run_dma();
                }
                return;
            }
        }
        self.plain_write(a, size, v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_faults_unmapped_and_mirror_only() {
        let mut b = Bus::new(16);
        b.set_strict(true);
        b.pc = 0x4000_1000;
        b.read32(0x8000_FFFC); // real SRAM
        b.write8(0x4000_0000, 1); // DDR
        b.read32(0xFC0B_0140); // allowed peripheral page
        assert_eq!(b.strict_pending, None);
        b.read16(0x0000_0010); // NULL + 16
        assert_eq!(b.strict_pending, Some((0x4000_1000, 0x10)));
        b.strict_pending = None;
        b.write32(0x8001_0000, 5); // the SRAM mirror
        assert_eq!(b.strict_pending, Some((0x4000_1000, 0x8001_0000)));
        b.strict_pending = None;
        b.pc = 0x4012_119A; // stock's own NULL store
        b.write16(0, 0);
        assert_eq!(b.strict_pending, None);
        assert_eq!(b.strict_log.len(), 2);
        // Off: the mirror and unmapped pages are plain memory again.
        let mut b = Bus::new(16);
        b.write32(0x8001_0000, 7);
        assert_eq!(b.read32(0x8000_0000), 7);
        b.read8(0x10);
        assert_eq!(b.strict_pending, None);
    }
}
