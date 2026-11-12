//! Save and restore the whole machine.
//!
//! A snapshot holds the CPU registers, every peripheral model, DDR (LZ4
//! compressed; most of 128 MB is zeros), the SRAM, the sparse register
//! pages, the card's sectors, and the machine's own bookkeeping. It is tied
//! to the MAIN OS image by hash: restoring onto a different build would run
//! one firmware's RAM against another's code.

use crate::cpu::Cpu;
use crate::io::Io;
use crate::machine::{DspStats, Machine, Stats};
use crate::syntakt::DspSide;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;

const MAGIC: &[u8; 8] = b"DTEMUSNP";
/// The SPI flash's trailer: after the main body (and the DSP's, if any). A
/// snapshot without it (older ones, MKI units) leaves the flash as it is.
const FLASH_TAG: &[u8; 4] = b"FLSH";
const VERSION: u32 = 2; // 2: panel held[16] + groups

#[derive(Serialize, Deserialize)]
struct CpuState {
    d: [u32; 8],
    a: [u32; 8],
    pc: u32,
    sr: u16,
    other_sp: u32,
    vbr: u32,
    cacr: u32,
    ctl: BTreeMap<u16, u32>,
    macsr: u32,
    mask: u32,
    acc: [u32; 4],
    accext: [u16; 4],
    stopped: bool,
    exc_counts: Vec<u64>,
}

#[derive(Serialize)]
struct SnapRef<'a> {
    main_sha256: String,
    cpu: CpuState,
    io: &'a Io,
    sram: &'a [u8],
    sparse: Vec<(u32, &'a [u8])>,
    ddr_lz4: Vec<u8>,
    stats: &'a Stats,
    frame: &'a Option<Vec<u8>>,
    frame_seq: u64,
}

#[derive(Deserialize)]
struct Snap {
    main_sha256: String,
    cpu: CpuState,
    io: Io,
    sram: Vec<u8>,
    sparse: Vec<(u32, Vec<u8>)>,
    ddr_lz4: Vec<u8>,
    stats: Stats,
    frame: Option<Vec<u8>>,
    frame_seq: u64,
}

impl CpuState {
    fn of(c: &Cpu) -> CpuState {
        CpuState {
            d: c.d,
            a: c.a,
            pc: c.pc,
            sr: c.sr,
            other_sp: c.other_sp,
            vbr: c.vbr,
            cacr: c.cacr,
            ctl: c.ctl.clone(),
            macsr: c.macsr,
            mask: c.mask,
            acc: [0, 1, 2, 3].map(|i| c.acc_phys(i).0),
            accext: [0, 1, 2, 3].map(|i| c.acc_phys(i).1),
            stopped: c.stopped,
            exc_counts: c.exc_counts.to_vec(),
        }
    }

    fn restore(self, c: &mut Cpu) {
        c.d = self.d;
        c.a = self.a;
        c.pc = self.pc;
        c.sr = self.sr;
        c.other_sp = self.other_sp;
        c.vbr = self.vbr;
        c.cacr = self.cacr;
        c.ctl = self.ctl;
        c.macsr = self.macsr;
        c.mask = self.mask;
        for i in 0..4 {
            c.set_acc_phys(i, self.acc[i], self.accext[i]);
        }
        c.stopped = self.stopped;
        for (i, v) in self.exc_counts.iter().enumerate().take(256) {
            c.exc_counts[i] = *v;
        }
        c.skip_hook = None;
    }
}

fn sparse_of(c: &Cpu) -> Vec<(u32, &[u8])> {
    let mut sparse: Vec<(u32, &[u8])> = c.bus.sparse.iter().map(|(k, v)| (*k, &v[..])).collect();
    sparse.sort_by_key(|x| x.0);
    sparse
}

fn sparse_from(v: Vec<(u32, Vec<u8>)>) -> std::collections::HashMap<u32, Box<[u8; 4096]>> {
    v.into_iter()
        .map(|(k, v)| {
            let mut b = Box::new([0u8; 4096]);
            b[..v.len().min(4096)].copy_from_slice(&v[..v.len().min(4096)]);
            (k, b)
        })
        .collect()
}

/// The Syntakt's second CPU, after the main body when it runs (a snapshot
/// without it loads as before; one with it carries a trailer).
#[derive(Serialize)]
struct DspRef<'a> {
    cpu: CpuState,
    io: &'a Io,
    side: &'a DspSide,
    sram: &'a [u8],
    sparse: Vec<(u32, &'a [u8])>,
    ddr_lz4: Vec<u8>,
    stats: &'a DspStats,
}

#[derive(Deserialize)]
struct DspSnap {
    cpu: CpuState,
    io: Io,
    side: DspSide,
    sram: Vec<u8>,
    sparse: Vec<(u32, Vec<u8>)>,
    ddr_lz4: Vec<u8>,
    stats: DspStats,
}

pub fn save(m: &Machine, path: &Path) -> std::io::Result<()> {
    let c = &m.cpu;
    let cpu = CpuState::of(c);
    let sparse = sparse_of(c);
    let snap = SnapRef {
        main_sha256: m.prof.sha256.clone(),
        cpu,
        io: &c.bus.io,
        sram: &c.bus.sram,
        sparse,
        ddr_lz4: lz4_flex::compress_prepend_size(&c.bus.ddr),
        stats: &m.stats,
        frame: &m.frame,
        frame_seq: m.frame_seq,
    };
    let mut body = bincode::serialize(&snap).map_err(std::io::Error::other)?;
    if let Some(d) = m.dsp.as_ref() {
        let dsp = DspRef {
            cpu: CpuState::of(d),
            io: &d.bus.io,
            side: d.bus.io.dsp_side.as_ref().expect("DSP side"),
            sram: &d.bus.sram,
            sparse: sparse_of(d),
            ddr_lz4: lz4_flex::compress_prepend_size(&d.bus.ddr),
            stats: &m.dsp_stats,
        };
        body.extend(bincode::serialize(&dsp).map_err(std::io::Error::other)?);
    }
    if let Some(f) = c.bus.spi.as_ref() {
        body.extend_from_slice(FLASH_TAG);
        body.extend(bincode::serialize(&f.state()).map_err(std::io::Error::other)?);
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(MAGIC)?;
        f.write_all(&VERSION.to_le_bytes())?;
        f.write_all(&body)?;
    }
    std::fs::rename(tmp, path)
}

pub fn load(m: &mut Machine, path: &Path) -> Result<(), String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut head = [0u8; 12];
    f.read_exact(&mut head).map_err(|e| e.to_string())?;
    if &head[..8] != MAGIC {
        return Err(format!("{} is not a snapshot", path.display()));
    }
    let ver = u32::from_le_bytes(head[8..12].try_into().unwrap());
    if ver != VERSION {
        return Err(format!("snapshot version {ver}, this build reads {VERSION}"));
    }
    let mut body = Vec::new();
    f.read_to_end(&mut body).map_err(|e| e.to_string())?;
    let mut cur = std::io::Cursor::new(&body[..]);
    let s: Snap = bincode::deserialize_from(&mut cur).map_err(|e| e.to_string())?;
    let at_flash = |cur: &std::io::Cursor<&[u8]>| body[cur.position() as usize..].starts_with(FLASH_TAG);
    let dsp: Option<DspSnap> = if (cur.position() as usize) < body.len() && !at_flash(&cur) {
        Some(bincode::deserialize_from(&mut cur).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let flash: Option<crate::spiflash::FlashState> = if at_flash(&cur) {
        cur.set_position(cur.position() + FLASH_TAG.len() as u64);
        Some(bincode::deserialize_from(&mut cur).map_err(|e| e.to_string())?)
    } else {
        None
    };
    if s.main_sha256 != m.prof.sha256 {
        return Err("snapshot was taken on a different MAIN OS".into());
    }
    let ddr = lz4_flex::decompress_size_prepended(&s.ddr_lz4).map_err(|e| e.to_string())?;
    if ddr.len() != m.cpu.bus.ddr.len() {
        return Err(format!("snapshot DDR is {} bytes, machine has {}", ddr.len(), m.cpu.bus.ddr.len()));
    }
    let c = &mut m.cpu;
    c.bus.ddr = ddr;
    c.bus.icache_flush();
    c.bus.sram = s.sram;
    c.bus.sparse = sparse_from(s.sparse);
    // Host-side settings are the running machine's, not the snapshot's.
    let (ips, ignore_masks, trace) = (c.bus.io.ips, c.bus.io.ignore_masks, c.bus.io.trace_intc);
    let ssi0_on = c.bus.io.ssi0.on;
    let rx_sign_extend = c.bus.io.ssi.rx_sign_extend;
    let card_path = c.bus.io.esdhc.card.path.clone();
    c.bus.io = s.io;
    c.bus.io.ips = ips;
    c.bus.io.ignore_masks = ignore_masks;
    c.bus.io.trace_intc = trace;
    c.bus.io.ssi0.on = ssi0_on;
    c.bus.io.ssi.rx_sign_extend = rx_sign_extend;
    if ssi0_on {
        // The slot masks the request counts use, from the register pages.
        for (i, a) in [0xFC0C_8048u32, 0xFC0C_804C].into_iter().enumerate() {
            c.bus.io.ssi0.ssi1_masks[i] = c.bus.peek32(a);
        }
        for a in [0xFC0B_C048u32, 0xFC0B_C04C] {
            c.bus.io.ssi0.regs[((a & 0x7F) / 4) as usize] = c.bus.peek32(a);
        }
    }
    c.bus.io.esdhc.card.path = card_path;
    // Host-side too: unit timing is the running machine's; a busy in flight
    // was not saved, so it ends now.
    if c.bus.timing.is_some() {
        c.bus.io.esdhc.timing = crate::esdhc::CardTiming::from_env().ok();
    }
    c.bus.io.epier = c.bus.peek8(0xFC09_0003);
    c.bus.io.esdhc.settle_after_restore();
    c.bus.io.deadline = c.bus.io.now;
    s.cpu.restore(c);
    // Snapshots carry memory only: the caches start empty (cache.rs).
    if let Some(k) = c.bus.cache.as_mut() {
        k.reset();
    }
    c.cache_sync();
    m.stats = s.stats;
    m.dsp = None;
    if let Some(d) = dsp {
        if !m.dsp_real() {
            return Err("snapshot carries the Syntakt's second CPU; this machine runs the stand-in (DTEMU_SYNTAKT_DSP=stub)".into());
        }
        let ddr = lz4_flex::decompress_size_prepended(&d.ddr_lz4).map_err(|e| e.to_string())?;
        let mut core = m.dsp_core(d.io.now);
        if ddr.len() != core.bus.ddr.len() {
            return Err(format!("snapshot DSP DDR is {} bytes, the DSP has {}", ddr.len(), core.bus.ddr.len()));
        }
        core.bus.ddr = ddr;
        core.bus.icache_flush();
        core.bus.sram = d.sram;
        core.bus.sparse = sparse_from(d.sparse);
        let ips = core.bus.io.ips;
        core.bus.io = d.io;
        core.bus.io.ips = ips;
        core.bus.io.dsp_side = Some(Box::new(d.side));
        core.bus.io.deadline = core.bus.io.now;
        d.cpu.restore(&mut core);
        m.dsp = Some(Box::new(core));
        m.dsp_stats = d.stats;
    }
    if let (Some(state), Some(f)) = (flash, m.cpu.bus.spi.as_mut()) {
        f.restore(state)?;
    }
    m.frame = s.frame;
    m.frame_seq = s.frame_seq;
    Ok(())
}
