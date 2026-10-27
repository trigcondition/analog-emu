//! The Analog Rytm: CPU, memory, peripherals, and the few high-level stand-ins
//! for hardware that is not modelled at register level.

use crate::bus::Bus;
use crate::cpu::{Cpu, Stop};
use crate::firmware::Firmware;
use crate::symbols::{Device, Profile, MAIN_LOAD};

/// Where the OS finds its own container in the SPI flash: the updater writes
/// it there and the OS's flash reads (section tables, the panel MCU image)
/// look for it there.
pub const FLASH_SLOT: usize = 0x8_0000;
/// The Rytm MKII's flash (the A4 MKII's is 8 MB: `flash_part`).
pub const FLASH_SIZE: usize = 16 << 20;

/// The SPI flash part each unit is modelled with (spiflash.rs): the ones
/// read on units, else the Rytm's.
pub fn flash_part(d: Device) -> crate::spiflash::Part {
    match d {
        Device::A4Mk2 => crate::spiflash::W25Q64,
        _ => crate::spiflash::S25FL128S_256K,
    }
}
pub const INITIAL_SP: u32 = 0x4080_0000;
/// The MKI units' LCD: 122x32, 1 bit (the OS clears 488 B = 122 x 4 pages and
/// centres text on 0x7a) [measured, docs/MK1-PORT.md].
pub const MK1_LCD_W: usize = 122;
pub const MK1_LCD_H: usize = 32;
pub const DEFAULT_VBR: u32 = 0x4000_0000;
/// The argument the bootstrap passes MAIN OS when a panel answered: bit 20
/// always, bit 18 from the bootstrap's own check (reference FINDINGS, "The
/// boot flags"). The OS stores it at 0x401F55C0 and tests it in more than
/// twenty places; 0x40|0x20 would mean "no panel" and park the OS. Per device:
/// `Device::boot_flags` (the A4 MKII's differ).
pub const BOOT_FLAGS: u32 = 0x0014_0000;

/// What the factory console leaves at 0x48000000 before a software reset so
/// the bootstrap starts the next boot in test mode (boot flag 0x40).
pub const MAINTENANCE_MAGIC: u32 = 0xB0B0_DADA;

#[derive(Default, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Stats {
    pub flash_reads: u64,
    pub idle_hits: u64,
    pub idle_skipped: u64,
    pub tasks: Vec<(u32, u32, u32)>,
    pub frames: u64,
    pub intro_done_at: Option<u64>,
    pub abort_at: Option<(u64, u32)>,
    pub mainloop: u64,
}

pub struct Options {
    /// Treat an idle spin as wait-for-interrupt: jump the clock to the next
    /// event instead of executing the spin.
    pub fast_idle: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { fast_idle: true }
    }
}

/// A panel event queued for a clock (`Machine::nudge`).
#[derive(Clone, Copy, Debug)]
pub enum Nudge {
    Turn(u8, i8),
    Key(&'static str, bool),
}

/// What `Machine::power_off` saw.
#[derive(Debug)]
pub struct PowerOff {
    /// EPORT IRQ2 was enabled: the firmware was listening for the power sense.
    pub armed: bool,
    /// The handler finished with the card (GO_PRE_IDLE seen).
    pub done: bool,
    pub instructions: u64,
    pub sectors_written: u64,
    /// A valid state header ('COKI') at sector 0x7a000, 0x7c000 afterwards.
    pub slots: [bool; 2],
    pub stop: Stop,
    /// A flash erase or program the power cut short (spiflash.rs `cut`).
    pub flash_cut: Option<String>,
}

pub struct Machine {
    pub cpu: Cpu,
    pub prof: Profile,
    pub stats: Stats,
    pub opts: Options,
    /// The last complete frame the firmware handed to the panel.
    pub frame: Option<Vec<u8>>,
    pub frame_seq: u64,
    pub log: Vec<String>,
    /// Addresses at which `run_until` stops and returns `Stop::Hook`.
    pub breakpoints: Vec<u32>,
    /// Hit counters: address -> count.
    pub counters: Vec<(u32, u64)>,
    /// A buddy-allocator low-water probe (`add_heap_probe`).
    pub heap_probe: Option<HeapProbe>,
    /// The force a pad press reports (0..255; the OS ignores 10 and below).
    pub pad_force: u8,
    /// While the idle task is posting a USB MIDI message (`feed_usb_midi`): its
    /// stack pointer before the call, and the arguments pushed (2 for
    /// `queue_send`, then 1 for `sem_post`), popped when it is back.
    usb_call: Option<(u32, u32)>,
    /// The USB audio buffer the last receive-buffer copy wrote (read on the
    /// next call, when that copy is complete).
    usb_audio_last: Option<u32>,
    /// Overbridge host -> Rytm frames waiting to be mixed (oblink.rs), and the
    /// host link that fills them.
    pub ob_in: std::collections::VecDeque<crate::oblink::Frame>,
    pub ob_link: Option<crate::oblink::ObLink>,
    /// A simulated USB host on the Rytm's USB port (usbhost.rs): with it, the
    /// firmware's own Overbridge path carries the audio, both ways.
    pub usb_host: Option<crate::usbhost::UsbHost>,
    /// Panel events due at a clock (`nudge`), in time order.
    pub nudges: std::collections::VecDeque<(u64, Nudge)>,
    /// Per encoder: the clock of its last nudge (an encoder turned recently is
    /// awake: the firmware takes its turns at once).
    nudge_last: [u64; 16],
    /// ext_report's memory: the amp start command seen last block, and P1..P8 sent.
    ext_amp_cmd: [bool; 8],
    ext_params: [[u8; 8]; 8],
    /// The firmware's own USB MIDI sender runs (`set_usb_midi_real`): its
    /// request pool, its dTDs on endpoint 0x81, completed only as the
    /// simulated USB host reads them. Off: the emulator takes the sender's
    /// calls (`usb_midi_send`, `usb_midi_send_short`, `sysex_send`).
    pub usb_midi_real: bool,
    /// While `usb_midi_real`: sem_pend calls that would block (count <= 0)
    /// made with the interrupt mask above 0 - an RTOS wait from inside an
    /// interrupt handler: (level, semaphore, return address) -> (first clock, count).
    pub isr_pends: std::collections::BTreeMap<(u8, u32, u32), (u64, u64)>,
    /// The Syntakt's second CPU (syntakt.rs), once CPU1 has started it; its
    /// profile and program (section 7). None elsewhere, and with the
    /// stand-in (`DTEMU_SYNTAKT_DSP=stub`).
    pub dsp: Option<Box<Cpu>>,
    pub dsp_prof: Option<crate::syntakt::DspProfile>,
    dsp_image: Vec<u8>,
    pub dsp_stats: DspStats,
    /// Keep the DSP's rendered blocks (`dsp_take_capture`).
    pub dsp_capture: bool,
}

/// What the Syntakt's second CPU did.
#[derive(Default, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DspStats {
    /// Starts (CPU1 can reset and boot it again), and the last one's clock.
    pub boots: u64,
    pub started_at: u64,
    /// Clock skipped in its idle spins.
    pub idle_skipped: u64,
    /// A fault it could not take: (vector, pc).
    pub fault: Option<(u8, u32)>,
}

/// Instructions CPU1 runs before the DSP catches up (`Machine::run_until`):
/// the latency of each line between them [inferred: 50 us at 200 M a
/// second, against a 32-frame block of about 133 k].
pub const DSP_SLICE: u64 = 10_000;

/// The Rytm's pads in pad-number order (pad p reports as `0xAp`/`0xBp`;
/// `*0x40285608` maps p to the OS's pad index, identity in OS 1.73).
pub const RYTM_PADS: [&str; 12] = ["BD", "SD", "RS", "CP", "BT", "LT", "MT", "HT", "CH", "OH", "CY", "CB"];

impl Machine {
    pub fn new(fw: &Firmware, ddr_mb: usize) -> Result<Machine, String> {
        let os = fw.main_os().map_err(|e| e.to_string())?;
        if os.entry.dest != MAIN_LOAD {
            return Err(format!("MAIN OS loads at 0x{:08x}, expected 0x{MAIN_LOAD:08x}", os.entry.dest));
        }
        let prof = Profile::for_image(&os.data)?;
        // The A4 MKI has 64 MB of DDR (ACR0 0x4003e020; its stack starts at
        // 0x44000000) [measured, docs/MK1-PORT.md].
        let ddr_mb = if prof.device == Device::A4Mk1 { 64 } else { ddr_mb };
        let mut bus = Bus::new(ddr_mb);
        if prof.device.mk1() {
            bus.nor = Some(Default::default());
            bus.codec = Some(Default::default());
        }
        bus.poke_bytes(MAIN_LOAD, &os.data);
        // DTEMU_CACHE=1 (or =log): the I- and D-cache model (cache.rs). It
        // runs on the interpreter alone, without predecoded blocks.
        bus.cache = crate::cache::Caches::from_env().map(Box::new);
        let cached = bus.cache.is_some();
        if std::env::var_os("DTEMU_NO_ICACHE").is_none() && !cached {
            bus.icache_enable(os.data.len() as u32);
        }
        // DTEMU_STRICT=1: unmapped pages and the SRAM mirror fault (bus.rs). It
        // runs on the interpreter, which reports the exact faulting PC.
        let strict_env = std::env::var("DTEMU_STRICT").unwrap_or_default();
        let strict = !strict_env.is_empty() && strict_env != "0";
        bus.set_strict(strict);
        bus.strict_raise = strict_env != "log";
        let mut cpu = Cpu::new(bus);
        // The block compiler, on its own thread (DTEMU_JIT_SYNC=1: in line;
        // DTEMU_NO_JIT=1: the interpreter alone).
        if std::env::var_os("DTEMU_NO_JIT").is_none() && !strict && !cached {
            if std::env::var_os("DTEMU_JIT_SYNC").is_some() {
                cpu.jit = Some(Box::new(crate::jit::Jit::new(cpu.bus.icache_flushes)));
            } else {
                cpu.jitw = Some(crate::jit::Worker::new());
            }
        }
        cpu.pc = prof.entry;
        cpu.sr = 0x2700;
        // The OS entry reads its argument at 4(a7).
        cpu.a[7] = INITIAL_SP - 8;
        let flags = std::env::var("DTEMU_BOOT_FLAGS").ok()
            .and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok())
            .unwrap_or_else(|| prof.device.boot_flags());
        cpu.bus.poke32(INITIAL_SP - 4, flags);
        cpu.vbr = DEFAULT_VBR;

        if !prof.device.mk1() {
            let part = flash_part(prof.device);
            let mut flash = vec![0u8; part.size];
            let c = &fw.container;
            flash[FLASH_SLOT..FLASH_SLOT + c.len()].copy_from_slice(c);
            cpu.bus.spi = Some(crate::spiflash::SpiFlash::with_part(flash, part));
        }

        let mut m = Machine {
            cpu,
            prof,
            stats: Stats::default(),
            opts: Options::default(),
            frame: None,
            frame_seq: 0,
            log: Vec::new(),
            breakpoints: Vec::new(),
            counters: Vec::new(),
            heap_probe: None,
            pad_force: 100,
            usb_call: None,
            usb_audio_last: None,
            ob_in: std::collections::VecDeque::new(),
            ob_link: None,
            usb_host: None,
            nudges: std::collections::VecDeque::new(),
            nudge_last: [0; 16],
            ext_amp_cmd: [false; 8],
            ext_params: [[255; 8]; 8],
            usb_midi_real: false,
            isr_pends: Default::default(),
            dsp: None,
            dsp_prof: None,
            dsp_image: Vec::new(),
            dsp_stats: DspStats::default(),
            dsp_capture: false,
        };
        match m.prof.device {
            Device::RytmMk2 => m.rytm_bootstrap_state(),
            Device::A4Mk2 => m.a4_bootstrap_state(),
            Device::RytmMk1 | Device::A4Mk1 => m.mk1_bootstrap_state(),
            Device::Syntakt => {
                // The second CPU's version word, as section 7 carries it at
                // 0x4004005a (sent in its `HO HA`, 0x40000ecc) [measured].
                let ver = fw.section(7).and_then(|s| s.data.get(0x3_FC5A..0x3_FC5C)).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]));
                let mut link = crate::syntakt::DspLink::new(ver);
                // The second CPU runs section 7 unless the stand-in is asked
                // for (DTEMU_SYNTAKT_DSP=stub).
                let stub = std::env::var("DTEMU_SYNTAKT_DSP").is_ok_and(|v| v == "stub");
                if let (false, Some(s7)) = (stub, fw.section(7)) {
                    let p = crate::syntakt::DspProfile::for_image(&s7.data)?;
                    m.dsp_image = s7.data.clone();
                    m.dsp_prof = Some(p);
                    link.real = true;
                }
                m.cpu.bus.io.dsp = Some(Box::new(link));
                m.syntakt_bootstrap_state()
            }
            Device::Digitakt => {}
        }
        m.install_hooks();
        if std::env::var_os("DTEMU_USB_MIDI_REAL").is_some_and(|v| v != "0") {
            m.set_usb_midi_real(true);
        }
        Ok(m)
    }

    /// Run the firmware's own USB MIDI sender (`usb_midi_real`; also
    /// `DTEMU_USB_MIDI_REAL=1`): unhook its stand-ins, and watch `sem_pend`
    /// for waits from interrupt level (`isr_pends`). Needs a USB host
    /// (`usb_host`) to read anything; the host's `midi_in` is set as it
    /// attaches (`run_until`).
    pub fn set_usb_midi_real(&mut self, on: bool) {
        self.usb_midi_real = on;
        let p = self.prof.clone();
        for a in [p.usb_midi_send, p.usb_midi_send_short, p.sysex_send] {
            if a != 0 {
                self.cpu.set_hook(a, !on);
            }
        }
        if p.sem_pend != 0 {
            self.cpu.set_hook(p.sem_pend, on);
        }
    }

    /// A synthetic analog calibration record, as a calibrated Rytm keeps in its
    /// SPI flash at 0x340000 (and a copy at 0x300000), so the OS boots as on a
    /// calibrated unit. The analog side is stubbed, so the values only have to
    /// pass the OS's checks (OS 1.73, `0x400fade6` and its helpers):
    ///
    ///   +0      magic "RBOW"
    ///   +4      12490, block 1's size
    ///   +8      format: 5 is current (the OS's own default record carries 5)
    ///   +12     checksum of block 1's 12474 data bytes from +16: sum of
    ///           (byte_i XOR i), i from 1 (`0x400fabfc`)
    ///   +16..   block 1 data; its first word is the calibration version:
    ///           0xFFFF = missing, 1 = an old calibration ("improved tuning"
    ///           prompt); `DTEMU_CAL_VERSION`, default 2 [inferred]
    ///   +12492  block 2 (checked from format 2): end offset, then the checksum
    ///           of the bytes from +12500 to that end minus 16
    ///
    /// The record is 110706 bytes; everything not named here is 0.
    ///
    /// On by default since 2026-10-06 (`DTEMU_NO_CAL=1` for none: MISSING CAL,
    /// and silent toms). A nonzero version word makes the
    /// machine engines use the calibration values (`tstw +16` at 0x4011b07a;
    /// 0 means the built-in defaults), and with zeros there the first engine to
    /// run divides by zero (`remul +3126`, 0x4011b112) and the OS stops in its
    /// fatal handler. Booting as calibrated needs plausible values for the
    /// fields the engines read; see docs/RYTM-PORT.md.
    fn rytm_calibration(mk1: bool) -> Vec<u8> {
        const LEN: usize = 110_706;
        const B1: usize = 12_490;
        let mut r = vec![0u8; LEN];
        // The Rytm MKI reads the version word the other way round: 1 is the
        // current calibration (0x400f4254; anything else gives "MISSING CAL:
        // SYNTH" and the "improved tuning" offer) [measured].
        let version: u16 = std::env::var("DTEMU_CAL_VERSION").ok().and_then(|v| v.parse().ok()).unwrap_or(if mk1 { 1 } else { 2 });
        r[16..18].copy_from_slice(&version.to_be_bytes());
        let sum = |d: &[u8]| -> u32 {
            d.iter().enumerate().fold(0u32, |s, (i, &b)| s.wrapping_add((b as u32) ^ (i as u32 + 1)))
        };
        // The toms (4 groups: BT, LT, MT, HT) [measured layout, 0x4011b000]:
        // nine period counts each at +3126 + 36g + 4k; the OS fits
        // y = 49152000/count linearly against its x table (`0x401f7ecc`) and a
        // tom routine (`0x4011b534`) writes its tune CV as
        // ((y_target - b) / s)^2 / 2048, y_target being its exponential pitch
        // value / 4 (about 57143 at BT's default TUN) [measured arithmetic].
        // The counts describe a made-up tom that this emulator's model then
        // follows: y = 1142.8 f and f = 0.1 x, so f = 4.53 sqrt(CV) Hz, up to
        // about 1.16 kHz; BT's default TUN comes out near 100 Hz [synthetic]. Block 2's +12500 + 36g + 4k hold x
        // points for the OS's second (decay) fit [synthetic].
        const TOM_X: [u32; 9] = [362, 512, 724, 1024, 1448, 2048, 2896, 4096, 5793];
        for g in 0..4 {
            for (k, &x) in TOM_X.iter().enumerate() {
                let y = crate::analog::TOM_Y_PER_HZ as f64 * crate::analog::TOM_HZ_PER_X as f64 * x as f64;
                let count = (49_152_000.0 / y).round() as u32;
                let o = 3126 + 36 * g + 4 * k;
                r[o..o + 4].copy_from_slice(&count.to_be_bytes());
                let o = 12_500 + 36 * g + 4 * k;
                r[o..o + 4].copy_from_slice(&x.to_be_bytes());
            }
        }
        // Block 2: header at +B1+2 (0x30CC), data from +B1+10 (0x30D4).
        let end = LEN as u32;
        let b2 = B1 + 2;
        let b2_len = (end - 12_516) as usize;
        r[b2..b2 + 4].copy_from_slice(&end.to_be_bytes());
        let s2 = sum(&r[b2 + 8..b2 + 8 + b2_len]);
        r[b2 + 4..b2 + 8].copy_from_slice(&s2.to_be_bytes());
        // Block 1 header.
        r[0..4].copy_from_slice(b"RBOW");
        r[4..8].copy_from_slice(&(B1 as u32).to_be_bytes());
        r[8..12].copy_from_slice(&5u32.to_be_bytes());
        let s1 = sum(&r[16..16 + 12_474]);
        r[12..16].copy_from_slice(&s1.to_be_bytes());
        r
    }

    /// What the Rytm's bootstrap leaves set up that MAIN OS relies on, since the
    /// emulator enters MAIN OS directly. The bootstrap (OS 1.73, 0x070207c2) sets
    /// DTIM2's DTMR to 7 - enabled, clocked from DTIN - and clears its counter;
    /// MAIN OS then times a delay on that counter (0x400055b2) before setting up
    /// SSI1. What drives DTIN2 is not known: 12.288 MHz, an audio master clock, is
    /// [inferred], and only the length of that wait depends on it.
    fn rytm_bootstrap_state(&mut self) {
        if std::env::var_os("DTEMU_NO_CAL").is_none() {
            let cal = Self::rytm_calibration(false);
            for at in [0x30_0000usize, 0x34_0000] {
                self.flash_mut()[at..at + cal.len()].copy_from_slice(&cal);
            }
        }
        self.cpu.bus.io.dtin_hz = 12_288_000.0;
        // The Rytm's panel has 11 button groups (the OS's slot table, panel.rs).
        self.cpu.bus.io.panel.groups = 11;
        self.cpu.bus.io.panel.card = crate::panel::CARD_RYTM;
        // Its analog voices, modelled from the control frame (analog.rs).
        if std::env::var_os("DTEMU_NO_ANALOG").is_none() {
            self.cpu.bus.analog = Some(Default::default());
        }
        self.cpu.bus.write16(0xFC07_8000, 0x0007);
        self.cpu.bus.write32(0xFC07_800C, 0);
    }

    /// A synthetic calibration record for the Analog Four MKII OS 1.56, as a
    /// calibrated unit keeps in its SPI flash at 0x300000 (3670 B, read by
    /// 0x400f18e8 into the buffer *0x40214c10). Format [measured, code
    /// reading; docs/A4-PORT.md section 11]:
    ///
    ///   +0      magic "RBOW"; +4 size 3670; +8 format (2 current, 1 is
    ///           upgraded at 0x400f1a38); +12 checksum of the size - 16 bytes
    ///           from +16: sum of (byte_i XOR i), i from 1 (0x400f1884)
    ///   +0x10   CV calibrated (nonzero); +0x14/+0x24/+0x34 + 4c: CV out c's
    ///           offset and two gains (0x400fc9a2 -> 0x41864a64/54/44)
    ///   +0x44   AUD (voice) calibrated (nonzero); +0x48.. per voice v and
    ///           oscillator k = 2v + osc the voice tables 0x40218e48..0x40218fa7
    ///           (loaded by 0x40104148)
    ///   +0x24c  KBD calibrated (u16, nonzero; read only from format 2);
    ///           +0x24e six 256-entry u16 tables; +0xe4e 4 x u16 input ranges
    ///           (0x400649d2: gain 0x800000 / (v - 34))
    ///
    /// The values are neutral: AUD's are solved from the OS's own uncalibrated
    /// tables (read from the loaded image, so the voices run as uncalibrated);
    /// KBD's are the OS's own format-1 upgrade defaults (ramps 0, 10 .. 2550;
    /// ranges 150); CV's are offset 0, gains 0x40000000 [synthetic].
    ///
    /// `tab_shift`: how far below the MKII's the OS's uncalibrated tables sit
    /// (0 on the A4 MKII; 0x26750 on the A4 MKI OS 1.56, where xmatch places
    /// 0x40218e48 / 0x40218ea8 / 0x40218ed8 / 0x40218ee8 / 0x40218f28 /
    /// 0x40218f68 at that distance [measured], the other three words at the
    /// same distance [inferred]).
    fn a4_calibration(&self, tab_shift: u32) -> Vec<u8> {
        const LEN: usize = 0xE56;
        let mut r = vec![0u8; LEN];
        let put = |r: &mut Vec<u8>, o: usize, v: i32| r[o..o + 4].copy_from_slice(&v.to_be_bytes());
        let peek = |a: u32| self.cpu.bus.peek32(a - tab_shift) as i32;
        // CV: flag, offsets 0, both gains 0.5 (Q31).
        put(&mut r, 0x10, 1);
        for c in 0..4 {
            put(&mut r, 0x24 + 4 * c, 0x4000_0000);
            put(&mut r, 0x34 + 4 * c, 0x4000_0000);
        }
        // AUD (0x40104148), per voice v (4) and oscillator k = 2v + o (8).
        put(&mut r, 0x44, 1);
        // The "span" pair: S = (lo + hi) << 15, D = (((hi + 2000) - (lo - 2000)
        // (hi + 2000 clamped to 0x7fff when `clamp`)) << 15) / 63 << 6.
        let span = |s: i32, d: i32, clamp: bool| -> (i32, i32) {
            let sum = s >> 15;
            let f = |hi: i32| {
                let lo = sum - hi;
                let h = if clamp { (hi + 2000).min(0x7fff) } else { hi + 2000 };
                ((h - (lo - 2000)).wrapping_shl(15) / 63).wrapping_shl(6)
            };
            let est = ((d as f64 / 64.0 * 63.0 / 32768.0 - 4000.0 + sum as f64) / 2.0).round() as i32;
            let hi = (est - 64..=est + 64).min_by_key(|&h| (f(h) as i64 - d as i64).abs()).unwrap();
            (sum - hi, hi)
        };
        for k in 0..8u32 {
            let ko = 8 * k as usize;
            // Copied as is: 0x40218f68 + 8k.
            put(&mut r, 0x48 + ko, peek(0x4021_8F68 + 8 * k));
            put(&mut r, 0x4C + ko, peek(0x4021_8F6C + 8 * k));
            // (D at 0x40218f28 + 8k, S at +4), clamped; (D at 0x40218ee8 + 8k, S at +4).
            let (lo, hi) = span(peek(0x4021_8F2C + 8 * k), peek(0x4021_8F28 + 8 * k), true);
            put(&mut r, 0x88 + ko, lo);
            put(&mut r, 0x8C + ko, hi);
            let (lo, hi) = span(peek(0x4021_8EEC + 8 * k), peek(0x4021_8EE8 + 8 * k), false);
            put(&mut r, 0xC8 + ko, lo);
            put(&mut r, 0xCC + ko, hi);
        }
        for v in 0..4u32 {
            let vo = 4 * v as usize;
            // 0x40218ea8 + 8v = (0xb8cfe d) >> 16 (unsigned), +4 = lo - 8d; d = hi - lo.
            let (a, b) = (peek(0x4021_8EA8 + 8 * v), peek(0x4021_8EAC + 8 * v));
            let d = (-40000..40000).min_by_key(|&d: &i32| ((0xB8CFEi32.wrapping_mul(d) as u32 >> 16) as i64 - a as i64).abs()).unwrap();
            let lo = b + ((d.wrapping_shl(19) >> 16) as i16 as i32);
            put(&mut r, 0x13C + vo, lo);
            put(&mut r, 0x14C + vo, lo + d);
            put(&mut r, 0x16C + vo, peek(0x4021_8ED8 + 4 * v));
            // 0x40218e48 + 8v = (0xb8cfe d) >> 16 (signed), +4 = lo - 9d.
            let (a, b) = (peek(0x4021_8E48 + 8 * v), peek(0x4021_8E4C + 8 * v));
            let d = (-40000..40000).min_by_key(|&d: &i32| ((0xB8CFEi32.wrapping_mul(d) >> 16) as i64 - a as i64).abs()).unwrap();
            let lo = b + ((d.wrapping_mul(0x9_0000) >> 16) as i16 as i32);
            put(&mut r, 0x1DC + vo, lo);
            put(&mut r, 0x1EC + vo, lo + d);
            put(&mut r, 0x1FC + vo, peek(0x4021_8EC8 + 4 * v));
        }
        // KBD: the OS's own format-1 upgrade values (0x400f1a42).
        r[0x24C..0x24E].copy_from_slice(&1u16.to_be_bytes());
        for t in 0..6 {
            for i in 0..256 {
                let o = 0x24E + 0x200 * t + 2 * i;
                r[o..o + 2].copy_from_slice(&(10 * i as u16).to_be_bytes());
            }
        }
        for i in 0..4 {
            r[0xE4E + 2 * i..0xE50 + 2 * i].copy_from_slice(&150u16.to_be_bytes());
        }
        // Header.
        r[0..4].copy_from_slice(b"RBOW");
        r[4..8].copy_from_slice(&(LEN as u32).to_be_bytes());
        r[8..12].copy_from_slice(&2u32.to_be_bytes());
        let sum = r[16..].iter().enumerate().fold(0u32, |s, (i, &b)| s.wrapping_add((b as u32) ^ (i as u32 + 1)));
        r[12..16].copy_from_slice(&sum.to_be_bytes());
        r
    }

    /// The Analog Four MKII's start state (docs/A4-PORT.md). Its main task sets
    /// DTIM2 up itself (0x40106018) and reads the counter, so DTIN must count; the
    /// UI card is type 6. A synthetic calibration record (`a4_calibration`;
    /// `DTEMU_NO_CAL=1` for none: "MISSING CAL: AUD CV KBD").
    fn a4_bootstrap_state(&mut self) {
        if std::env::var_os("DTEMU_NO_CAL").is_none() {
            let cal = self.a4_calibration(0);
            self.flash_mut()[0x30_0000..0x30_0000 + cal.len()].copy_from_slice(&cal);
        }
        self.cpu.bus.io.dtin_hz = 12_288_000.0;
        self.cpu.bus.io.panel.card = crate::panel::CARD_A4;
        // 12 button groups (the OS's slot table, panel.rs A4_KEYS).
        self.cpu.bus.io.panel.groups = 12;
        crate::panel::use_a4_panel();
        // Its voices, modelled from the voice frame (a4voice.rs).
        if std::env::var_os("DTEMU_NO_ANALOG").is_none() {
            self.cpu.bus.a4 = Some(Default::default());
        }
    }

    /// The MKI units' start state (docs/MK1-PORT.md). There is no bootstrap
    /// section: the emulator enters MAIN OS directly, as the bootstrap in the
    /// flash's boot block would. So the flash holds what a unit's does: the
    /// bootstrap MAIN OS carries (`boot_blob`) at 0 and its version record at
    /// the end of the boot block, which the entry compares before deciding on
    /// a bootstrap upgrade [measured]. The boot flags (the entry's argument,
    /// kept at 0x401d8ea4 on the A4) are the panel's UI firmware version in
    /// the top byte, as the bootstrap leaves them, and 0 below (a blank card
    /// adds 0x12, `attach_card`): on the A4 bit 20 with a top byte
    /// of 0, 1 or 3 starts a panel (UI) upgrade (0x40100f7c), bit 0 clears the
    /// CS1 memory, bits 1, 2 and 31 restore settings from it (0x40000f7a)
    /// [measured]; `DTEMU_BOOT_FLAGS=HEX` overrides. MAIN OS sets DTIM2 up itself
    /// (internal clock), so DTIN is not needed [measured].
    fn mk1_bootstrap_state(&mut self) {
        let p = self.prof.clone();
        let bus = &mut self.cpu.bus;
        let blob = bus.peek_bytes(p.boot_blob.0, (p.boot_blob.1 - p.boot_blob.0) as usize);
        crate::nor::load(&mut bus.sparse, 0, &blob);
        let ver = bus.peek_bytes(p.boot_ver, 8);
        crate::nor::load(&mut bus.sparse, p.boot_ver_flash, &ver);
        // The bootstrap puts the panel MCU's UI firmware version (byte 1 of its
        // reply to `70 00`, the UI card query) in the flags' top byte (Rytm MKI
        // blob 0x8a4); with 0 there the Rytm MKI OS reports "OLD UI FW"
        // (0x400a19be) [measured].
        let ui = (bus.io.panel.card[1] as u32) << 24;
        let flags = std::env::var("DTEMU_BOOT_FLAGS").ok().and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok()).unwrap_or(ui);
        bus.poke32(INITIAL_SP - 4, flags);
        // The A4 MKI keeps its calibration record (the A4 MKII's format, 3670 B)
        // in the NOR flash at 0x7f0000 (written by 0x400ebcde) [measured].
        if p.device == Device::A4Mk1 {
            // Its panel: 11 key groups (the OS's slot table, panel::A4MK1_KEYS).
            self.cpu.bus.io.panel.groups = 11;
            crate::panel::use_a4mk1_panel();
        }
        if p.device == Device::RytmMk1 {
            // Its panel: 10 key groups (the OS's slot table, panel::RYTMMK1_KEYS).
            self.cpu.bus.io.panel.groups = 10;
            crate::panel::use_rytmmk1_panel();
        }
        // The Rytm MKI reads the Rytm MKII's record format (RBOW, format 5,
        // block 1 12490 B, 110706 B in all; 0x400f3e40) from its NOR flash at
        // 0x780000, with a copy at 0x7c0000 [measured].
        if p.device == Device::RytmMk1 && std::env::var_os("DTEMU_NO_CAL").is_none() {
            let cal = Self::rytm_calibration(true);
            for at in [0x78_0000, 0x7C_0000] {
                crate::nor::load(&mut self.cpu.bus.sparse, at, &cal);
            }
        }
        if p.device == Device::A4Mk1 && std::env::var_os("DTEMU_NO_CAL").is_none() {
            let cal = self.a4_calibration(0x2_6750);
            crate::nor::load(&mut self.cpu.bus.sparse, 0x7F_0000, &cal);
        }
        let bus = &mut self.cpu.bus;
        // The voice frame goes out on DSPI2 by eDMA channel 29; the sender
        // (A4 MKI 0x40061b7e) waits for TCF (SR bit 28) from the last one
        // first. Transfers complete at once here [inferred], so TCF reads set.
        bus.io.forced.insert(0xEC03_802C, 0x9000_0000);
        // The Rytm MKI: SSI0 beside SSI1, and the Rytm MKII's analog model on
        // its own frame and FX parameter addresses.
        if p.device == Device::RytmMk1 {
            bus.io.ssi0.on = true;
            if std::env::var_os("DTEMU_NO_ANALOG").is_none() {
                let mut a = crate::analog::Analog::default();
                a.frame_addr = 0x8000_4CE0;
                a.fx_addr = 0x8000_66E6;
                bus.analog = Some(a);
            }
        }
        // The A4 MKI's voices: the A4 MKII's model on its own frame address.
        if p.device == Device::A4Mk1 && std::env::var_os("DTEMU_NO_ANALOG").is_none() {
            let mut v = crate::a4voice::A4Voices::default();
            v.frame_addr = 0x8000_32B0;
            bus.a4 = Some(v);
        }
        // DTRR's reset value is 0xFFFFFFFF (MCF54418RM): the OS runs DTIM0 and
        // DTIM3 free from the bus clock with FRR set (0x400ffc30) and reads
        // their counters without writing DTRR [measured].
        for d in bus.io.dtim.iter_mut() {
            d.dtrr = u32::MAX;
        }
        // The bootstrap starts DTIM0 that way (DTMR = 0x0b; the Rytm MKI's blob
        // at 0x4028cf58), and the Rytm MKI OS times its codec start-up on it
        // without setting it up itself (0x400066a2) [measured].
        bus.write16(0xFC07_0000, 0x000B);
    }

    /// A synthetic analog calibration record for the Syntakt OS 1.42, as a
    /// calibrated unit keeps in its SPI flash at 0x300000 (the factory copy is
    /// at 0x340000) [measured, code reading: reader 0x4006ecb8, check
    /// 0x4006edca]:
    ///
    ///   +0      magic "RBOZ"; +4 size 0x22d30; +8 format 1; +12 checksum of the
    ///           0x22d30 bytes from +16: sum of (byte_i XOR i), i from 1
    ///           (0x4006eb5c)
    ///   +0x10   u16 1 = calibrated (else "ANALOG CALIBRATION MISSING [-1]")
    ///   +0x17, +0x18, +0x19, +0xda, +0x14f, +0x15c, +0x1885, +0x2fae: one
    ///           flag byte per calibrated part, nonzero = done (a zero one
    ///           reports -2 .. -9 and offers RUN WARM-UP CALIBRATION)
    ///
    /// The record is 0x22d40 B; everything else is 0 [synthetic: what the
    /// parts' values mean is not traced; the analog voices are not modelled].
    fn syntakt_calibration() -> Vec<u8> {
        const LEN: usize = 0x22D40;
        let mut r = vec![0u8; LEN];
        r[0..4].copy_from_slice(b"RBOZ");
        r[4..8].copy_from_slice(&0x22D30u32.to_be_bytes());
        r[8..12].copy_from_slice(&1u32.to_be_bytes());
        r[0x10..0x12].copy_from_slice(&1u16.to_be_bytes());
        for o in [0x17, 0x18, 0x19, 0xDA, 0x14F, 0x15C, 0x1885, 0x2FAE] {
            r[o] = 1;
        }
        let sum = r[16..].iter().enumerate().fold(0u32, |s, (i, &b)| s.wrapping_add((b as u32) ^ (i as u32 + 1)));
        r[12..16].copy_from_slice(&sum.to_be_bytes());
        r
    }

    /// The Syntakt's start state (docs/SYNTAKT-PORT.md): UI card type 0x0B on
    /// the Digitakt's panel MCU.
    fn syntakt_bootstrap_state(&mut self) {
        self.cpu.bus.io.panel.card = crate::panel::CARD_SYNTAKT;
        crate::panel::use_syntakt_panel();
        if std::env::var_os("DTEMU_NO_CAL").is_none() {
            let cal = Self::syntakt_calibration();
            for at in [0x30_0000usize, 0x34_0000] {
                self.flash_mut()[at..at + cal.len()].copy_from_slice(&cal);
            }
        }
        // The board revision register, a byte on FlexBus CS1 at 0 (CS1 64 KB
        // at 0, 0x40082510): bits 0-4 the PCB revision r, 5-7 a variant v;
        // the self-test prints PCBA0046 'A'+r, v+1 (0x40081c7a). r < 3, or r = 3
        // with v < 3, is "TOO OLD HARDWARE FOR THIS OS", and 0x82, 0x23, 0x43
        // "OLD HARDWARE" (0x4000aec0) [measured]. 0x63 reads PCBA0046D4: the
        // owner's board says PCBA0046D [community photo; the digit inferred].
        self.cpu.bus.poke8(0, 0x63);
        // As on the Rytm MKII: MAIN OS times a delay on DTIM2's counter
        // (0x40000f66) that its bootstrap started from DTIN [measured: the
        // wait; DTIN's clock inferred].
        self.cpu.bus.io.dtin_hz = 12_288_000.0;
        self.cpu.bus.write16(0xFC07_8000, 0x0007);
        self.cpu.bus.write32(0xFC07_800C, 0);
        // The analog voice frame goes out on DSPI1 by eDMA channel 15; the
        // sender (0x4010223e) first waits for TCF (SR bit 28) from the last
        // one (0x4010222a). Transfers complete at once here [inferred].
        self.cpu.bus.io.forced.insert(0xFC03_C02C, 0x9000_0000);
        // Its analog voices and analog FX block, from that frame (stanalog.rs),
        // into receive words read sign-extended (io.rs `rx_sign_extend`).
        self.cpu.bus.io.ssi.rx_sign_extend = true;
        if std::env::var_os("DTEMU_NO_ANALOG").is_none() {
            self.cpu.bus.st = Some(Default::default());
        }
    }

    fn install_hooks(&mut self) {
        let p = self.prof.clone();
        for a in [p.flash_read, p.task_create, p.intro_done, p.panel_diff, p.abort_loop, p.mainloop, p.sysex_send, p.usb_midi_send, p.usb_midi_send_short, p.usb_audio_copy, p.voice_output_mix, p.voice_frame_sent] {
            if a == 0 {
                continue; // not found in this image
            }
            self.cpu.set_hook(a, true);
        }
        for &a in &p.idle_spins {
            self.cpu.set_hook(a, true);
        }
    }

    /// Stand-in for the Rytm-side report of the EXT machine (rytm-mods 0021) to an
    /// Overbridge host: trigs and P1..P8 of every voice whose track plays
    /// machine 44, sent over the link (oblink.rs). A trig is the voice's amp
    /// envelope record getting its start command (0x4000 in the first word,
    /// control frame +168 + 8v) [measured]; note and velocity from the track
    /// (pitch table 0x80006388 + 4t, 16.16, note + 24 when played chromatically;
    /// velocity byte 0x800067c4 + 2t); the owner track from 0x40290dd8[v];
    /// machines at 0x8000ea00 [measured, rytm-mods 0020 design.md]. OS 1.73/1.74
    /// RAM.
    /// Only while machine 44's engine is DISABLE's own (0x40113efc): a build with
    /// ext_render reports for itself, as USB MIDI SysEx.
    fn ext_report(&mut self) {
        const EXT: u8 = 44;
        let b = &self.cpu.bus;
        if b.peek32(0x4029_037C + 4 * EXT as u32) != 0x4011_3EFC {
            return;
        }
        for v in 0..8u32 {
            let track = b.peek8(0x4029_0DD8 + v) as u32;
            if track >= 12 || b.peek8(0x8000_EA00 + track) != EXT {
                self.ext_amp_cmd[v as usize] = false;
                continue;
            }
            let start = b.peek16(0x8000_63C0 + 168 + 8 * v) == 0x4000;
            let was = self.ext_amp_cmd[v as usize];
            self.ext_amp_cmd[v as usize] = start;
            let params: [u8; 8] = std::array::from_fn(|k| (b.peek16(0x8000_F7BA + 84 * track + 2 * k as u32) >> 8) as u8);
            let note = ((b.peek32(0x8000_6388 + 4 * track) >> 16) as u8).min(127);
            let vel = b.peek8(0x8000_67C4 + 2 * track) & 0x7F;
            if let Some(l) = self.ob_link.as_ref() {
                if params != self.ext_params[v as usize] {
                    l.send_params(v as u8, params);
                }
                if start && !was {
                    l.send_trig(v as u8, note, vel);
                }
            }
            self.ext_params[v as usize] = params;
        }
    }

    pub fn add_counter(&mut self, pc: u32) {
        self.counters.push((pc, 0));
        self.cpu.set_hook(pc, true);
    }

    /// Watch a binary buddy allocator's free bytes: at each PC in `pcs`
    /// (points where the caller holds the allocator's lock), walk its free
    /// lists and keep the lowest total. `state`: +4 the arena base, +12 the
    /// free-list heads by order (unit indices, -1 none), +16 the top order,
    /// +24 the live allocations; a free block's next index is at +8; units
    /// are 16 bytes. Addresses come from the caller (per build).
    pub fn add_heap_probe(&mut self, state: u32, pcs: Vec<u32>) {
        for &pc in &pcs {
            self.cpu.set_hook(pc, true);
        }
        self.heap_probe = Some(HeapProbe { state, pcs, min_free: u64::MAX, min_at: 0, min_live: 0, max_live: 0, samples: 0 });
    }

    /// The allocator's free bytes and live allocations now (None before it is set up).
    pub fn heap_free(&self, state: u32) -> Option<(u64, u32)> {
        let b = &self.cpu.bus;
        if b.peek32(state) == 0 {
            return None;
        }
        let base = b.peek32(state + 4);
        let heads = b.peek32(state + 12);
        let top = b.peek32(state + 16).min(31);
        let mut free = 0u64;
        for k in 0..=top {
            let mut u = b.peek32(heads + 4 * k);
            let mut n = 0;
            while u != u32::MAX && n < 1_000_000 {
                free += 16u64 << k;
                u = b.peek32(base.wrapping_add(u << 4) + 8);
                n += 1;
            }
        }
        Some((free, b.peek32(state + 24)))
    }

    fn heap_sample(&mut self) {
        let state = self.heap_probe.as_ref().unwrap().state;
        let Some((free, live)) = self.heap_free(state) else { return };
        let now = self.now();
        let h = self.heap_probe.as_mut().unwrap();
        h.samples += 1;
        h.max_live = h.max_live.max(live);
        if free < h.min_free {
            h.min_free = free;
            h.min_at = now;
            h.min_live = live;
        }
    }

    /// The SPI flash image (spiflash.rs). Panics on an MKI unit, whose flash
    /// is the parallel NOR in the bus's sparse pages.
    pub fn flash(&self) -> &[u8] {
        &self.cpu.bus.spi.as_ref().expect("no SPI flash on this unit").mem
    }

    pub fn flash_mut(&mut self) -> &mut [u8] {
        &mut self.cpu.bus.spi.as_mut().expect("no SPI flash on this unit").mem
    }

    /// `--flash FILE`: the SPI flash from a file a previous run saved (the OS's
    /// writes, an upgraded OS included), replacing the one built from the
    /// `.syx`. A missing file leaves the built one, to be saved there.
    pub fn load_flash(&mut self, path: &std::path::Path) -> Result<bool, String> {
        let Some(f) = self.cpu.bus.spi.as_mut() else {
            return Err("this unit has no SPI flash (an MKI keeps its OS in NOR)".into());
        };
        match std::fs::read(path) {
            Ok(d) if d.len() == f.mem.len() => {
                f.mem = d;
                f.dirty = false;
                Ok(true)
            }
            Ok(d) => Err(format!("{}: {} bytes, a flash image is {}", path.display(), d.len(), f.mem.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Write the SPI flash back to `path` (a host file, nothing else).
    pub fn save_flash(&mut self, path: &std::path::Path) -> Result<(), String> {
        let f = self.cpu.bus.spi.as_mut().ok_or("no SPI flash")?;
        // An operation under way finishes: the power stays on.
        f.settle(u64::MAX);
        std::fs::write(path, &f.mem).map_err(|e| format!("{}: {e}", path.display()))?;
        f.dirty = false;
        Ok(())
    }

    /// After `Stop::Reset`: boot again from the OS container now in the SPI
    /// flash at `FLASH_SLOT` (the new OS after an upgrade), keeping the flash
    /// and the +Drive card (written back to its file, then attached again);
    /// everything else starts cold, the tools' options included. The real
    /// bootstrap that would unpack the container is not run: the machine
    /// loads its MAIN OS as it does from a `.syx`, so the build must be one
    /// `symbols.rs` knows. One bootstrap rule is kept: a software reset with
    /// `MAINTENANCE_MAGIC` at 0x48000000 (the factory console's #REBOOT and
    /// #REBOOT_INTO_MAINTENANCE_MODE leave it, OS 1.74 `0x4013a234`) boots
    /// with flag 0x40, the test mode (the 1.74 bootstrap's `0x80000ac4`
    /// checks RSR's SOFT bit and that word) [read from the code]. -> the
    /// new profile's name.
    pub fn reboot_from_flash(&mut self) -> Result<&'static str, String> {
        let maintenance = self.cpu.bus.peek32(0x4800_0000) == MAINTENANCE_MAGIC;
        let mut flash = self.cpu.bus.spi.take().ok_or("no SPI flash")?;
        flash.settle(u64::MAX);
        let fw = match Firmware::from_flash(&flash.mem[FLASH_SLOT..]) {
            Ok(fw) => fw,
            Err(e) => {
                self.cpu.bus.spi = Some(flash);
                return Err(format!("no bootable OS in the flash at 0x{FLASH_SLOT:x}: {e}"));
            }
        };
        let mut m = match Machine::new(&fw, self.cpu.bus.ddr.len() >> 20) {
            Ok(m) => m,
            Err(e) => {
                self.cpu.bus.spi = Some(flash);
                return Err(e);
            }
        };
        m.cpu.bus.spi = Some(flash);
        if maintenance {
            if let Some(flag) = m.set_test_mode() {
                m.note(format!("reboot into test mode (0xB0B0DADA at 0x48000000): boot flag 0x{flag:x}"));
            }
        }
        let card = &mut self.cpu.bus.io.esdhc.card;
        match card.path.clone() {
            Some(p) => {
                card.flush().map_err(|e| format!("{}: {e}", p.display()))?;
                m.attach_card(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            }
            None => std::mem::swap(&mut m.cpu.bus.io.esdhc.card.sectors, &mut card.sectors),
        }
        *self = m;
        Ok(self.prof.name)
    }

    /// Boot into the factory-test console, as the bootstrap does for its key
    /// combo or a maintenance reboot: OR the device's flag into the boot
    /// flags (call before the first instruction). -> the flag, or None on a
    /// device without one here.
    pub fn set_test_mode(&mut self) -> Option<u32> {
        let flag = self.prof.device.test_mode_flag()?;
        let f = self.cpu.bus.peek32(INITIAL_SP - 4);
        self.cpu.bus.poke32(INITIAL_SP - 4, f | flag);
        Some(flag)
    }

    pub fn add_breakpoint(&mut self, pc: u32) {
        self.breakpoints.push(pc);
        self.cpu.set_hook(pc, true);
    }

    pub fn now(&self) -> u64 {
        self.cpu.icount()
    }

    fn note(&mut self, s: String) {
        if self.log.len() < 10_000 {
            self.log.push(format!("[{:>12}] {s}", self.now()));
        }
    }

    /// MIDI arriving on the DIN port, except Elektron-protocol SysEx on a build
    /// whose USB MIDI queue is known: that goes in as USB (`Io::midi_send`).
    pub fn midi_send(&mut self, bytes: &[u8]) {
        self.cpu.bus.io.usb_midi = self.prof.usb_midi_queue != 0;
        self.cpu.bus.io.midi_send(bytes);
    }

    /// Called in the idle loop at `pc`: post the next waiting USB MIDI message
    /// to the OS's USB MIDI queue as the USB driver would, as a chain of blocks
    /// from the MIDI pool handed to `queue_send`. The idle task makes the call
    /// (its frame is pushed here, returning to `pc`), so the queue's semaphore
    /// wakes the MIDI task the firmware's own way. No task can be inside the
    /// pool's allocator while the idle task runs. -> true when the CPU was
    /// redirected.
    fn feed_usb_midi(&mut self, pc: u32) -> bool {
        let p = &self.prof;
        if let Some((sp0, args)) = self.usb_call {
            if self.cpu.a[7] != sp0 - 4 * args {
                return false; // not back yet
            }
            self.cpu.a[7] = sp0;
            self.usb_call = None;
            if args == 2 {
                // Queued; now wake the MIDI task: sem_post(midi_event).
                let bus = &mut self.cpu.bus;
                bus.poke32(sp0 - 4, p.midi_event);
                bus.poke32(sp0 - 8, pc);
                self.cpu.a[7] = sp0 - 8;
                self.cpu.pc = p.sem_post;
                self.usb_call = Some((sp0, 1));
                return true;
            }
        }
        let (q, pool, map) = (p.usb_midi_queue, p.midi_pool, p.midi_pool_map);
        if q == 0 || self.cpu.bus.io.usb_in.is_empty() {
            return false;
        }

        let bus = &mut self.cpu.bus;
        // Queue: count at +4, index mask at +16.
        if bus.peek32(q + 4) >= bus.peek32(q + 16) || bus.peek32(p.midi_pool_sem) == 0 {
            return false;
        }
        let msg = bus.io.usb_in.front().unwrap().clone();
        if msg.first().is_some_and(|&b| b != 0xF0) && p.midi_short_ring != 0 {
            // A short message: one 4-byte slot `[status, d1, d2, len]`, taken
            // as the USB driver's allocator does (`0x400812de`).
            let idx = bus.peek32(p.midi_short_idx) & 0xFF;
            bus.poke32(p.midi_short_idx, (idx + 1) & 0xFF);
            let slot = p.midi_short_ring + 4 * idx;
            let mut s = [0u8; 4];
            for (k, &b) in msg.iter().take(3).enumerate() {
                s[k] = b;
            }
            s[3] = msg.len().min(3) as u8;
            bus.poke_bytes(slot, &s);
            bus.io.usb_in.pop_front();
            let sp = self.cpu.a[7];
            let bus = &mut self.cpu.bus;
            bus.poke32(sp - 4, slot);
            bus.poke32(sp - 8, q);
            bus.poke32(sp - 12, pc);
            self.cpu.a[7] = sp - 12;
            self.cpu.pc = p.queue_send;
            self.usb_call = Some((sp, 2));
            return true;
        }
        let need = msg.len().div_ceil(152);
        let mut free = Vec::new();
        'scan: for w in 0..32u32 {
            let bits = bus.peek32(map + 4 * w);
            for n in 0..32u32 {
                if bits >> n & 1 == 0 {
                    free.push(32 * w + n);
                    if free.len() == need {
                        break 'scan;
                    }
                }
            }
        }
        if free.len() < need {
            return false; // the pool is busy; try again later
        }
        for (k, (&idx, chunk)) in free.iter().zip(msg.chunks(152)).enumerate() {
            let w = map + 4 * (idx / 32);
            let bits = bus.peek32(w);
            bus.poke32(w, bits | 1 << (idx % 32));
            let blk = pool + 160 * idx;
            let next = free.get(k + 1).map_or(0, |&i| pool + 160 * i);
            bus.poke32(blk, chunk.len() as u32);
            bus.poke32(blk + 4, next);
            bus.poke_bytes(blk + 8, chunk);
        }
        bus.io.usb_in.pop_front();
        // The producer's message credit (`0x400813d4` pends on it before it
        // allocates; the MIDI task's free posts it back).
        let credit = bus.peek32(p.midi_pool_sem);
        bus.poke32(p.midi_pool_sem, credit - 1);
        let sp = self.cpu.a[7];
        let bus = &mut self.cpu.bus;
        bus.poke32(sp - 4, pool + 160 * free[0]);
        bus.poke32(sp - 8, q);
        bus.poke32(sp - 12, pc);
        self.cpu.a[7] = sp - 12;
        self.cpu.pc = p.queue_send;
        self.usb_call = Some((sp, 2));
        true
    }

    /// Handle a hooked PC. -> true to let the instruction there execute.
    fn on_hook(&mut self, pc: u32, until: u64) -> bool {
        let p = &self.prof;
        if self.usb_midi_real && pc == p.sem_pend {
            // sem_pend(sem): count at +0; it blocks (trap #0, a task switch)
            // when the count is <= 0. The caller's mask is SR's here, before
            // sem_pend raises it to 7.
            let level = ((self.cpu.sr >> 8) & 7) as u8;
            let sp = self.cpu.a[7];
            let (ret, sem) = (self.cpu.bus.peek32(sp), self.cpu.bus.peek32(sp + 4));
            if level > 0 && (self.cpu.bus.peek32(sem) as i32) <= 0 {
                let now = self.now();
                let e = self.isr_pends.entry((level, sem, ret)).or_insert((now, 0));
                e.1 += 1;
                if e.1 == 1 {
                    self.note(format!("BLOCKING sem_pend(0x{sem:08x}) at interrupt level {level}, return 0x{ret:08x}"));
                }
            }
            return true;
        }
        if pc == p.flash_read {
            // flash_read(offset, length, dest) -> 0, served from the image.
            let sp = self.cpu.a[7];
            let bus = &mut self.cpu.bus;
            let ret = bus.peek32(sp);
            let off = bus.peek32(sp + 4) as usize;
            let len = bus.peek32(sp + 8) as usize;
            let dest = bus.peek32(sp + 12);
            let now = bus.io.now;
            if let Some(f) = bus.spi.as_mut() {
                f.settle(now);
            }
            let data = match bus.spi.as_ref() {
                // Addresses wrap at the part's end, as on the part.
                Some(f) if len != 0 && dest != 0 && len <= f.mem.len() => {
                    let n = f.mem.len();
                    Some((0..len).map(|k| f.mem[(off + k) % n]).collect::<Vec<u8>>())
                }
                _ => None,
            };
            if let Some(data) = data {
                self.cpu.bus.poke_bytes(dest, &data);
            }
            self.stats.flash_reads += 1;
            self.cpu.d[0] = 0;
            self.cpu.a[7] = sp + 4;
            self.cpu.pc = ret;
            return false;
        }
        if p.idle_spins.contains(&pc) {
            self.stats.idle_hits += 1;
            if self.feed_usb_midi(pc) {
                return false;
            }
            if self.opts.fast_idle {
                // Nothing can change until an interrupt: skip to the next
                // event (or the end of the budget), as a WFI would.
                let io = &mut self.cpu.bus.io;
                let target = io.deadline.min(until);
                if target > io.now {
                    self.stats.idle_skipped += target - io.now;
                    io.now = target;
                }
            }
            return true;
        }
        if pc == p.task_create {
            let sp = self.cpu.a[7];
            let b = &self.cpu.bus;
            let (tcb, entry, prio) = (b.peek32(sp + 4), b.peek32(sp + 8), b.peek32(sp + 12));
            self.stats.tasks.push((entry, prio, tcb));
            self.note(format!("task_create entry=0x{entry:08x} prio={prio} tcb=0x{tcb:08x}"));
            return true;
        }
        if pc == p.intro_done {
            if self.stats.intro_done_at.is_none() {
                self.stats.intro_done_at = Some(self.now());
                self.note("intro done".into());
            }
            return true;
        }
        if pc == p.panel_diff {
            let ptr = self.cpu.bus.peek32(p.fb_front);
            if (0x4000_0000..0x5000_0000).contains(&ptr) {
                // 8 pages x 128 columns; the MKI's LCD 4 pages.
                let len = if p.device.mk1() { 512 } else { 1024 };
                self.frame = Some(self.cpu.bus.peek_bytes(ptr, len));
                self.frame_seq += 1;
                self.stats.frames += 1;
            }
            return true;
        }
        if pc == p.abort_loop {
            if self.stats.abort_at.is_none() {
                self.stats.abort_at = Some((self.now(), self.cpu.bus.peek32(self.cpu.a[7])));
                let caller = self.cpu.bus.peek32(self.cpu.a[7]);
                self.note(format!("ABORT: reached the abort loop, return address 0x{caller:08x}"));
            }
            return true;
        }
        if pc == p.mainloop {
            self.stats.mainloop += 1;
            return true;
        }
        if pc == p.voice_frame_sent {
            // The sender's frame argument (sp + 24 past its lea and moveml) is the voices' frame: latch what went out.
            let arg = self.cpu.bus.peek32(self.cpu.a[7].wrapping_add(24));
            if let Some(an) = self.cpu.bus.analog.as_ref() {
                if arg == an.frame_addr {
                    let v = self.cpu.bus.peek_bytes(arg, crate::analog::FRAME_LEN);
                    self.cpu.bus.analog_sent = Some(v);
                }
            }
            return true;
        }
        if pc == p.voice_output_mix {
            // The Overbridge input: this block's 32 frames into the half of the
            // double buffer the mix reads now (the firmware prepared it, with
            // zeros when no host sends).
            if let Some(l) = self.ob_link.as_mut() {
                l.poll(&mut self.ob_in);
                // Only while the USB MIDI input is short of USB_IN_HELD bytes (a Rytm not taking it leaves the
                // rest in the link's bounded queue); without a queue all of it, dropped below.
                let room = if p.usb_midi_queue != 0 {
                    let held: usize = self.cpu.bus.io.usb_in.iter().map(Vec::len).sum();
                    crate::oblink::USB_IN_HELD.saturating_sub(held)
                } else {
                    usize::MAX
                };
                let m = l.take_midi(room);
                // The host's MIDI into the Rytm's USB MIDI input, as `Machine::usb_midi_send` (the window's "usb
                // in" port) does, with or without a simulated USB host: usbhost.rs reads the MIDI streaming
                // interface's bulk IN (0x81) only and models no MIDI OUT endpoint, so the OS's USB MIDI queue is
                // the one way in either way.
                if !m.is_empty() {
                    if p.usb_midi_queue != 0 {
                        self.cpu.bus.io.usb_midi_send(&m);
                    } else if !l.midi_unrouted_logged {
                        l.midi_unrouted_logged = true;
                        eprintln!("overbridge link: host MIDI dropped: this build's profile has no USB MIDI queue (logged once)");
                    }
                }
            }
            let usb_streaming = self.usb_host.as_ref().is_some_and(|h| h.streaming());
            // Frames this block needs that the link's host has not sent (a whole block when none came).
            if !usb_streaming && self.ob_in.len() < 32 {
                if let Some(l) = self.ob_link.as_mut() {
                    if l.connected {
                        l.underruns += 32 - self.ob_in.len() as u64;
                    }
                }
            }
            // Without a USB host streaming, the stand-in writes the link's frames
            // straight into the firmware's input ring.
            if !usb_streaming && !self.ob_in.is_empty() {
                let half = self.cpu.bus.peek32(p.ob_in_index) & 1;
                let base = p.ob_in_buf + half * 1536;
                for f in 0..32u32 {
                    let fr = self.ob_in.pop_front().unwrap_or([0; crate::oblink::CHANNELS]);
                    for (c, &v) in fr.iter().enumerate() {
                        self.cpu.bus.poke32(base + 48 * f + 4 * c as u32, v as u32);
                    }
                }
            }
            if self.ob_link.is_some() {
                self.ext_report();
            }
            return true;
        }
        if pc == p.usb_audio_copy {
            if self.cpu.bus.io.usb_audio_capture {
                if let Some(dst) = self.usb_audio_last {
                    let words: Vec<i32> = (0..32 * 12).map(|k| self.cpu.bus.peek32(dst + 4 * k) as i32 >> 8).collect();
                    self.cpu.bus.io.usb_audio.extend(words);
                }
                self.usb_audio_last = Some(self.cpu.bus.peek32(self.cpu.a[7] + 8));
            }
            return true;
        }
        if pc == p.usb_midi_send || pc == p.usb_midi_send_short {
            // The firmware's USB MIDI output, `(len, ptr)`: taken here, not
            // handed to the unmodelled USB controller.
            let sp = self.cpu.a[7];
            let bus = &self.cpu.bus;
            let (len, ptr) = (bus.peek32(sp + 4), bus.peek32(sp + 8));
            let bytes = bus.peek_bytes(ptr, len.min(4096) as usize);
            self.usb_out(&bytes);
            self.cpu.pc = self.cpu.bus.peek32(sp);
            self.cpu.a[7] = sp + 4;
            return false;
        }
        if pc == p.sysex_send {
            // A SysEx reply for USB (interface 2 or 4) goes to the USB MIDI
            // output instead of the unmodelled USB controller, and only there:
            // a reply to a USB request does not appear on the DIN MIDI port.
            let sp = self.cpu.a[7];
            let bus = &self.cpu.bus;
            let iface = bus.peek32(sp + 16);
            if iface == 2 || iface == 4 {
                let (buf, len) = (bus.peek32(sp + 4), bus.peek32(sp + 8));
                let bytes = bus.peek_bytes(buf, len.min(1 << 20) as usize);
                self.usb_out(&bytes);
                self.cpu.d[0] = 1;
                self.cpu.pc = self.cpu.bus.peek32(sp);
                self.cpu.a[7] = sp + 4;
                return false;
            }
            return true;
        }
        true
    }

    /// Run until the clock reaches `until` or something stops the machine.
    /// Run the simulated USB host for the microframes that are due.
    fn usb_microframes(&mut self) {
        let Some(h) = self.usb_host.as_mut() else { return };
        let io = &mut self.cpu.bus.io;
        if io.usb_uframe.is_none() {
            io.usb_uframe = Some(io.now);
            io.deadline = io.deadline.min(io.now);
        }
        let n = std::mem::take(&mut io.usb_uframes_due);
        h.midi_in = self.usb_midi_real;
        for _ in 0..n {
            h.microframe(&mut self.cpu.bus, &mut self.ob_in);
        }
        let midi: Vec<u8> = h.midi_out.drain(..).collect();
        // The Rytm's outputs go to the link's host; without one they are dropped.
        if let Some(l) = self.ob_link.as_ref() {
            if h.from_rytm.len() >= 32 {
                let v: Vec<crate::oblink::Frame> = h.from_rytm.drain(..).collect();
                l.send_rytm(&v);
            }
        }
        if !midi.is_empty() {
            self.usb_out(&midi);
        }
    }

    /// The Rytm's USB MIDI output: to `io.usb_midi_out` (the window's "usb out" port, dtboot's dump) and, when the
    /// Overbridge link's host asked for it (oblink.rs HELLO_MIDI), to that host as well.
    fn usb_out(&mut self, bytes: &[u8]) {
        self.cpu.bus.io.usb_midi_out.extend(bytes.iter().copied());
        if let Some(l) = self.ob_link.as_ref() {
            l.send_midi(bytes);
        }
    }

    /// The Overbridge link's state for a log line (None without a link).
    pub fn ob_link_report(&self) -> Option<String> {
        let l = self.ob_link.as_ref()?;
        let (to, from) = l.midi_counts();
        let usb = self.usb_host.as_ref().map(|h| format!(", usb OUT frames short {}", h.stats.out_underruns)).unwrap_or_default();
        let d = l.dropped();
        let dropped = if d == crate::oblink::Dropped::default() {
            String::new()
        } else {
            format!(
                ", dropped: to host {} msgs, from host {} frames, {} midi msgs ({} B)",
                d.to_host, d.frames, d.midi, d.midi_bytes
            )
        };
        Some(format!(
            "overbridge link: host {}, flags {:#x}, frames short {}{usb}, midi to host {to} B, from host {from} B{dropped}",
            if l.connected { "connected" } else { "none" },
            l.granted(),
            l.underruns
        ))
    }

    pub fn run_until(&mut self, until: u64) -> Stop {
        if self.dsp_prof.is_none() {
            return self.run_main(until);
        }
        // Two CPUs: CPU1 runs a slice, then the DSP catches up to the same
        // clock, taking the kicks CPU1 made in it at their clocks; its IRQ
        // edges reach CPU1 at the start of the next slice.
        loop {
            let t = until.min(self.now() + DSP_SLICE);
            let s = self.run_main(t);
            self.dsp_catch_up();
            match s {
                Stop::Budget if self.now() < until => {}
                Stop::Stopped if self.now() < until => {
                    // A sleeping CPU1 the DSP may wake.
                    let io = &mut self.cpu.bus.io;
                    io.now = io.now.max(t);
                    io.service();
                }
                s => return s,
            }
        }
    }

    /// The DSP's rendered blocks since the last call, as frames of 8 words
    /// (tracks 1..8, signed 32-bit), while `dsp_capture` is set.
    pub fn dsp_take_capture(&mut self) -> Vec<i32> {
        self.dsp
            .as_mut()
            .and_then(|c| c.bus.io.dsp_side.as_mut())
            .and_then(|s| s.capture.as_mut())
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// One codec frame's DAC words (24-bit) as the speakers' stereo pair.
    /// The Digitakt's frame is the pair (words 0/1). The Syntakt's digital
    /// mix leaves on two stereo buses [measured: CPU1 `0x4008fca2..0x4008fde0`
    /// writes 0x8000dad0's pair to words 7 and 2 and 0x8000dfd0's to words 5
    /// and 3; FUNC + FX routing moves a track from 3/5 to 2/7]: the one into
    /// the analog FX block and the one straight to the main mix, summed in
    /// analog after the block. With the analog model (stanalog.rs) the block
    /// is modelled there and its output joins the analog voices in
    /// `Ssi::voices`, so this is the main pair alone, L = 5, R = 3; without it
    /// the block passes its input through, L = 7 + 5, R = 2 + 3 [inferred:
    /// which word of a pair is left]. Words 4, 1, 0 and 6 are the digital
    /// layers of tracks 9..12 (docs/SYNTAKT-PORT.md section 12), inputs to the
    /// analog voices.
    pub fn speaker_pair(&self, w: &[i32]) -> (f32, f32) {
        let s = |x: i32| x as f32 / 8_388_608.0;
        if self.is_syntakt() && w.len() >= 8 {
            if self.cpu.bus.st.is_some() {
                return (s(w[5]), s(w[3]));
            }
            (s(w[7]) + s(w[5]), s(w[2]) + s(w[3]))
        } else if w.len() >= 2 {
            (s(w[0]), s(w[1]))
        } else {
            (0.0, 0.0)
        }
    }

    /// Whether the Syntakt's second CPU is a real core (not the stand-in).
    pub fn dsp_real(&self) -> bool {
        self.dsp_prof.is_some()
    }

    /// Start the DSP at clock `t`: section 7 in its DDR at its link address,
    /// entered as section 6 leaves it (its stack at the top of SRAM, interrupts
    /// masked) [inferred: section 6's own state; it is not run].
    fn dsp_boot(&mut self, t: u64) {
        let p = self.dsp_prof.clone().expect("DSP profile");
        let mut c = self.dsp_core(t);
        c.pc = p.entry;
        c.sr = 0x2700;
        // Section 6's stack (its SP 0x80010000); the entry keeps 4(a7).
        c.a[7] = 0x8001_0000 - 8;
        self.dsp = Some(Box::new(c));
        self.dsp_stats.boots += 1;
        self.dsp_stats.started_at = t;
        self.dsp_stats.fault = None;
        self.dsp_stats.idle_skipped = 0;
        self.note(format!("DSP started (section 7 at 0x{:08x}, entry 0x{:08x})", crate::syntakt::DSP_LOAD, p.entry));
    }

    /// A second CPU at clock `t` with section 7 in its DDR, its link end, the
    /// block cache and compiler as CPU1 has them, and its idle hooks.
    pub fn dsp_core(&self, t: u64) -> Cpu {
        let p = self.dsp_prof.as_ref().expect("DSP profile");
        let mut bus = Bus::new(crate::syntakt::DSP_DDR_MB);
        bus.poke_bytes(crate::syntakt::DSP_LOAD, &self.dsp_image);
        if std::env::var_os("DTEMU_NO_ICACHE").is_none() {
            bus.icache_enable(self.dsp_image.len() as u32);
        }
        bus.io.now = t;
        bus.io.ips = self.cpu.bus.io.ips;
        // The IRQ line starts high: section 7's own copy of it (0x40040054)
        // starts at 1, so its first toggle drives it low [inferred: section 6
        // leaves it so; a first edge is what CPU1 waits for].
        bus.io.dsp_side = Some(Box::new(crate::syntakt::DspSide { irq_line: true, ..Default::default() }));
        let mut c = Cpu::new(bus);
        if self.cpu.jitw.is_some() {
            c.jitw = Some(crate::jit::Worker::new());
        } else if self.cpu.jit.is_some() {
            c.jit = Some(Box::new(crate::jit::Jit::new(c.bus.icache_flushes)));
        }
        for &a in &p.idle_spins {
            c.set_hook(a, true);
        }
        // DTEMU_DSP_PROFILE=1: sample its PCs (dtboot lists the hottest).
        if std::env::var_os("DTEMU_DSP_PROFILE").is_some() {
            c.profile = Some(Default::default());
        }
        c
    }

    /// Run the DSP up to CPU1's clock, then hand its IRQ edges to CPU1.
    fn dsp_catch_up(&mut self) {
        let now = self.now();
        let Some(link) = self.cpu.bus.io.dsp.as_mut() else { return };
        if link.dsp.phase == crate::syntakt::Phase::Off && self.dsp.is_some() {
            self.dsp = None; // held in reset
        }
        if self.dsp.is_none() {
            match link.boot_at {
                Some(t) if t <= now => {
                    link.boot_at = None;
                    link.kick_times.retain(|&k| k >= t);
                    self.dsp_boot(t);
                }
                _ => {
                    link.kick_times.retain(|&k| link.boot_at.is_some_and(|b| k >= b));
                    return;
                }
            }
        }
        let link = self.cpu.bus.io.dsp.as_mut().unwrap();
        let core = self.dsp.as_mut().unwrap();
        if self.dsp_stats.fault.is_some() {
            link.kick_times.clear();
            core.bus.io.now = now;
            return;
        }
        let side = core.bus.io.dsp_side.as_mut().unwrap();
        if self.dsp_capture != side.capture.is_some() {
            side.capture = self.dsp_capture.then(Vec::new);
        }
        side.kicks.extend(link.kick_times.drain(..));
        std::mem::swap(&mut link.mbox, &mut side.mbox);
        core.bus.io.service();
        let idle = &self.dsp_prof.as_ref().unwrap().idle_spins;
        loop {
            match core.run(now) {
                Stop::Budget => break,
                Stop::Event => {
                    core.bus.io.service();
                    core.bus.dma_service();
                }
                Stop::Stopped => {
                    let io = &mut core.bus.io;
                    io.now = io.deadline.min(now).max(io.now);
                    if io.now >= now {
                        break;
                    }
                    core.bus.io.service();
                    core.bus.dma_service();
                }
                Stop::Hook(pc) => {
                    if idle.contains(&pc) {
                        // Only an interrupt ends it: on to the next event.
                        let io = &mut core.bus.io;
                        let target = io.deadline.min(now);
                        if target > io.now {
                            self.dsp_stats.idle_skipped += target - io.now;
                            io.now = target;
                        }
                    }
                    core.skip_hook = Some(pc);
                }
                // The DSP's RCR is not its board's reset.
                Stop::Reset => break,
                Stop::Fault(v, p) => {
                    self.dsp_stats.fault = Some((v, p));
                    break;
                }
            }
        }
        let side = core.bus.io.dsp_side.as_mut().unwrap();
        std::mem::swap(&mut link.mbox, &mut side.mbox);
        let edges: Vec<u64> = side.irq_edges.drain(..).collect();
        for t in edges {
            link.raise(t, 0);
        }
        if link.irq_at.is_some() {
            let io = &mut self.cpu.bus.io;
            io.deadline = io.deadline.min(io.now);
        }
        if let Some((v, p)) = self.dsp_stats.fault {
            self.note(format!("DSP FAULT: vector {v} at 0x{p:08x}"));
        }
    }

    fn run_main(&mut self, until: u64) -> Stop {
        self.usb_microframes();
        self.run_nudges();
        self.cpu.bus.io.service();
        loop {
            let s = self.cpu.run(until);
            if self.cpu.bus.io.reset_request {
                return Stop::Reset;
            }
            match s {
                Stop::Budget => return Stop::Budget,
                Stop::Event => {
                    self.cpu.bus.io.service();
                    self.cpu.bus.dma_service();
                    self.usb_microframes();
                    if !self.nudges.is_empty() {
                        self.run_nudges();
                    }
                }
                Stop::Stopped => {
                    // STOP: sleep until the next event.
                    let io = &mut self.cpu.bus.io;
                    let target = io.deadline.min(until);
                    if target > io.now {
                        io.now = target;
                    }
                    if io.now >= until {
                        return Stop::Budget;
                    }
                    self.cpu.bus.io.service();
                    self.cpu.bus.dma_service();
                    self.usb_microframes();
                    // An interrupt at or below the mask cannot wake it; the
                    // CPU takes any that can in `run`.
                    if self.cpu.bus.io.deadline == u64::MAX && self.cpu.bus.io.irq_level <= self.cpu.ipl() {
                        return Stop::Stopped;
                    }
                }
                Stop::Hook(pc) => {
                    if self.breakpoints.contains(&pc) {
                        self.breakpoints.retain(|&b| b != pc);
                        self.cpu.skip_hook = Some(pc);
                        return Stop::Hook(pc);
                    }
                    for c in self.counters.iter_mut() {
                        if c.0 == pc {
                            c.1 += 1;
                        }
                    }
                    if self.heap_probe.as_ref().is_some_and(|h| h.pcs.contains(&pc)) {
                        self.heap_sample();
                    }
                    if self.on_hook(pc, until) {
                        self.cpu.skip_hook = Some(pc);
                    }
                }
                f @ (Stop::Fault(..) | Stop::Reset) => return f,
            }
        }
    }

    /// Every task seen created: (entry, prio, tcb, parked pc, stack words).
    pub fn task_report(&self) -> String {
        let b = &self.cpu.bus;
        let cur = b.peek32(self.prof.current_tcb);
        let mut out = String::new();
        for &(entry, prio, tcb) in &self.stats.tasks {
            let sp = b.peek32(tcb + 0x48);
            let pc = b.peek32(sp + 4);
            let words: Vec<String> = (2..14).map(|i| format!("{:08x}", b.peek32(sp + 4 * i))).collect();
            out += &format!(
                "  prio {prio:>2} entry {entry:08x} tcb {tcb:08x}{} sp {sp:08x} pc {pc:08x} | {}\n",
                if tcb == cur { " (running)" } else { "" },
                words.join(" ")
            );
        }
        out
    }

    /// Attach a +Drive card image (read now; `flush_card` writes it back).
    /// A missing or empty image becomes a fresh card with the sample area
    /// formatted, as a factory unit's is (the OS never creates it itself).
    /// -> whether the card is fresh.
    pub fn attach_card(&mut self, path: &std::path::Path) -> std::io::Result<bool> {
        let mut card = crate::esdhc::Card::open(path)?;
        let fresh = card.sectors.is_empty();
        if fresh {
            crate::ekfs::format(&mut card);
        }
        // An MKI OS meeting a +Drive without its header at byte 0 (A4 MKI
        // 0xBEEFBABA, 0x400708ec; Rytm MKI 0xBEEFBACE, 0x40091128) runs its
        // factory-reset sequence with "keep the working project" set (A4 MKI
        // 0x40096938). On the A4 MKI that copies CS1's project into project
        // slot 1 and depacks the CS1-sized default image (0x401bb5a0,
        // 2096892 B) into slot 2, 35876 B past the slots' end (0x40839774)
        // over live statics, and the OS dies in its abort loop: the image's own
        // code, so a unit would do the same [measured]; units leave the
        // factory with an initialised +Drive. The bootstrap menu's FACTORY
        // RESET with keys 0x81 and 0x82 held passes boot flags 0x12 (both MKI
        // blobs), and bit 4 skips the copy, so the default lands in slot 1
        // [measured]. A blank card boots that way here, as if the user had run
        // that reset (the Rytm MKI then also lays out its own ekFS, version 1
        // at sector 0x180000); DTEMU_BOOT_FLAGS overrides.
        let magic = match self.prof.device {
            Device::A4Mk1 => Some(0xBEEF_BABA),
            Device::RytmMk1 => Some(0xBEEF_BACE),
            _ => None,
        };
        let header = card.sectors.get(&0).map(|d| u32::from_be_bytes([d[0], d[1], d[2], d[3]]));
        if magic.is_some() && self.now() == 0 && header != magic && std::env::var_os("DTEMU_BOOT_FLAGS").is_none() {
            let f = self.cpu.bus.peek32(INITIAL_SP - 4);
            self.cpu.bus.poke32(INITIAL_SP - 4, f | 0x12);
            self.note("blank +Drive: boot flags 0x12 (the bootstrap's FACTORY RESET)".into());
        }
        self.cpu.bus.io.esdhc.card = card;
        Ok(fresh)
    }

    pub fn flush_card(&mut self) -> std::io::Result<()> {
        self.cpu.bus.io.esdhc.card.flush()
    }

    /// Switch the power off, as pulling the Rytm's supply does: the power
    /// sense (PPDSDR_C bit 2) falls and EPORT flags IRQ2's edge (level 7).
    /// The firmware's handler then runs on the supercap and writes the working
    /// state (the 2 MB at 0x405c8e10 on OS 1.73, 'COKI' header) to one of two
    /// raw eMMC slots, sectors 0x7a000 and 0x7c000, and invalidates the other;
    /// the next boot loads the valid one (docs/RYTM-PORT.md, "Power-off and
    /// the working state"). Runs until the handler has put the card in
    /// pre-idle (CMD0 0xF0F0F0F0), plus 2M to let it return, or for `limit`
    /// instructions. The machine is then off: nothing after it is meaningful
    /// except the card.
    pub fn power_off(&mut self, limit: u64) -> PowerOff {
        let t0 = self.now();
        let writes0 = self.cpu.bus.io.esdhc.card.writes;
        let pre0 = self.cpu.bus.io.esdhc.pre_idle;
        let armed = self.cpu.bus.peek8(0xFC09_0003) & 0x04 != 0; // EPIER: IRQ2 enabled
        let io = &mut self.cpu.bus.io;
        io.power_lost = true;
        io.epfr |= 0x04; // the flag sets whether or not EPIER lets it interrupt
        io.update_irq();
        io.deadline = io.now;
        let mut stop = Stop::Budget;
        let mut done = false;
        while self.now() < t0 + limit {
            let to = (self.now() + 1_000_000).min(t0 + limit);
            stop = self.run_until(to);
            if matches!(stop, Stop::Fault(..) | Stop::Hook(_) | Stop::Stopped) {
                break;
            }
            if self.cpu.bus.io.esdhc.pre_idle > pre0 {
                done = true;
                let to = self.now() + 2_000_000;
                stop = self.run_until(to);
                break;
            }
        }
        let card = &self.cpu.bus.io.esdhc.card;
        let slots = [0x7a000u32, 0x7c000].map(|s| card.sectors.get(&s).is_some_and(|d| &d[..4] == b"COKI"));
        let (writes, instructions) = (card.writes - writes0, self.now() - t0);
        // The CPU has parked; the flash keeps power for DTEMU_SPI_HOLD_MS more
        // (default 0), then an erase or program still under way stops. How
        // long the supercap holds the flash's rail is a board property the
        // firmware does not show, so this is a setting.
        let hold_ms: f64 = std::env::var("DTEMU_SPI_HOLD_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let now = self.now() + (hold_ms * 1e-3 * self.cpu.bus.io.ips) as u64;
        let flash_cut = self.cpu.bus.spi.as_mut().and_then(|f| f.cut(now));
        PowerOff { armed, done, instructions, sectors_written: writes, slots, stop, flash_cut }
    }

    /// The firmware's own "+Drive mounted" flag.
    pub fn drive_mounted(&self) -> bool {
        self.prof.mounted != 0 && self.cpu.bus.peek32(self.prof.mounted) == 1
    }

    /// Press or release a key by control code (see `panel::key_name`).
    pub fn key(&mut self, code: u8, down: bool) {
        if let Some(msg) = self.cpu.bus.io.panel.key(code, down) {
            self.panel_send(&msg);
        }
    }

    /// A key from the Digitakt-shaped panel (window and upstream control codes).
    /// On a Rytm the button's label is pressed by name instead, so the window is
    /// usable before it has a Rytm panel: 1..16 -> T1..T16, knob pushes A..H ->
    /// PUSH_A..PUSH_H, LEVEL/DATA -> PUSH_LEVEL, SAMPLE -> SAMPLER; PTN and BANK
    /// have no single Rytm key and do nothing.
    pub fn key_compat(&mut self, code: u8, down: bool) {
        if self.is_a4() || self.is_mk1() || self.is_syntakt() {
            if let Some(name) = crate::panel::key_name(code) {
                self.key_named(name, down);
            }
            return;
        }
        if !self.is_rytm() {
            return self.key(code, down);
        }
        if code >= crate::panel::RYTM_CODE_BASE {
            // The window's Rytm panel: the code names a RYTM_KEYS entry.
            if let Some(name) = crate::panel::key_name(code) {
                self.key_named(name, down);
            }
            return;
        }
        let Some(label) = crate::panel::key_name(code) else { return };
        let name = match label {
            "SAMPLE" => "SAMPLER".to_string(),
            "LEVEL/DATA" => "PUSH_LEVEL".to_string(),
            l if l.len() == 1 && l.as_bytes()[0].is_ascii_uppercase() => format!("PUSH_{l}"),
            l if l.parse::<u8>().is_ok() => format!("T{l}"),
            l => l.to_string(),
        };
        self.key_named(&name, down);
    }

    /// Words the codec link carries per audio frame (SSI STCCR: Digitakt 2,
    /// Analog Rytm MKII 16) [measured].
    pub fn ssi_frame_words(&self) -> usize {
        self.cpu.bus.io.ssi_frame_words() as usize
    }

    /// Whether this machine runs an Analog Rytm MKII OS.
    pub fn is_rytm(&self) -> bool {
        self.prof.device == Device::RytmMk2
    }

    /// Whether this machine runs an Analog Four MKII OS.
    pub fn is_a4(&self) -> bool {
        self.prof.device == Device::A4Mk2
    }

    /// Whether this machine runs a Syntakt OS.
    pub fn is_syntakt(&self) -> bool {
        self.prof.device == Device::Syntakt
    }

    /// Whether this machine is an MKI unit (Analog Rytm MKI, Analog Four MKI).
    pub fn is_mk1(&self) -> bool {
        self.prof.device.mk1()
    }

    /// Press or release a key by name (Rytm: `panel::RYTM_KEYS`; Digitakt:
    /// `panel::key_name`). -> false if this machine has no such key.
    pub fn key_named(&mut self, name: &str, down: bool) -> bool {
        // A Rytm pad is a pressure sensor: besides its button bit, the panel
        // sends `0xAp force` when it is struck and `0xBp 0` when released
        // (parser 0x40082084: type A posts a pad-hit event with a velocity
        // curve above the threshold at 0x40285624, type B a release) [measured:
        // `A0 7F` alone triggers BD]. Pad p = the pad's place in RYTM_PADS.
        // The Rytm MKI's parser (0x400806c2) takes the same `9p`, `Ap`, `Bp`
        // messages through its pad map (*0x40249dd0, identity) [measured].
        if self.is_rytm() || self.prof.device == Device::RytmMk1 {
            if let Some(p) = RYTM_PADS.iter().position(|&n| n.eq_ignore_ascii_case(name)) {
                let msg = if down { [0xA0 | p as u8, self.pad_force] } else { [0xB0 | p as u8, 0] };
                self.panel_send(&msg);
            }
        }
        let msg = if self.is_rytm() || self.is_a4() || self.is_mk1() || self.is_syntakt() {
            crate::panel::rytm_key(name).and_then(|(ch, bit)| self.cpu.bus.io.panel.wire(ch, bit, down))
        } else {
            (0..=47u8)
                .find(|&c| crate::panel::key_name(c).is_some_and(|n| n.eq_ignore_ascii_case(name)))
                .and_then(|c| self.cpu.bus.io.panel.key(c, down))
        };
        match msg {
            Some(m) => {
                self.panel_send(&m);
                true
            }
            None => false,
        }
    }

    /// Write 24-bit samples (interleaved, `chans` channels at 48 kHz) as a
    /// 16-bit WAV.
    pub fn write_wav16(path: &std::path::Path, samples: &[i32], chans: u16) -> std::io::Result<()> {
        let data: Vec<u8> = samples.iter().flat_map(|&s| ((s >> 8) as i16).to_le_bytes()).collect();
        let mut f = Vec::with_capacity(44 + data.len());
        f.extend_from_slice(b"RIFF");
        f.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        f.extend_from_slice(b"WAVEfmt ");
        f.extend_from_slice(&16u32.to_le_bytes());
        f.extend_from_slice(&1u16.to_le_bytes());
        f.extend_from_slice(&chans.to_le_bytes());
        f.extend_from_slice(&48_000u32.to_le_bytes());
        f.extend_from_slice(&(48_000 * 2 * chans as u32).to_le_bytes());
        f.extend_from_slice(&(2 * chans).to_le_bytes());
        f.extend_from_slice(&16u16.to_le_bytes());
        f.extend_from_slice(b"data");
        f.extend_from_slice(&(data.len() as u32).to_le_bytes());
        f.extend_from_slice(&data);
        std::fs::write(path, f)
    }

    /// MIDI arriving on the USB MIDI port (the window's "rytm-emu usb in").
    pub fn usb_midi_send(&mut self, bytes: &[u8]) {
        if self.prof.usb_midi_queue != 0 {
            self.cpu.bus.io.usb_midi_send(bytes);
        }
    }

    /// The Rytm pad (place in `RYTM_PADS`) a window key code names, if any.
    pub fn pad_of_code(&self, code: u8) -> Option<usize> {
        if !(self.is_rytm() || self.prof.device == Device::RytmMk1) || code < crate::panel::RYTM_CODE_BASE {
            return None;
        }
        let name = crate::panel::key_name(code)?;
        RYTM_PADS.iter().position(|&n| n.eq_ignore_ascii_case(name))
    }

    /// A pad's pressure while it is held: the panel streams `0x9p value`, which
    /// the OS posts as a pad-pressure event with the raw value (parser
    /// 0x40082276, event type 2) [measured]; the strike (`0xAp`) carries the
    /// velocity.
    pub fn pad_pressure(&mut self, pad: usize, value: u8) {
        self.panel_send(&[0x90 | pad as u8, value]);
    }

    /// Turn an encoder (0..8: A..H, then LEVEL/DATA) by `delta` counts.
    /// One value step on an encoder, as a person's single detent would make it
    /// (the window's arrow keys). The Rytm takes turns once per UI frame, spends
    /// the first two frames of a turn waking the knob (its value popup), then
    /// moves about one step per 4 counts and accelerates on faster turning
    /// [measured, OS 1.73, frames 10M apart: 8, 8, 4 counts -> +1; 8, 8, 4, 4 ->
    /// +2; 8, 8, 8 -> +2; turns under 8 do not wake it (1, 1, 4 -> 0); bursts 1M
    /// apart mostly lost; a knob stays awake 2..4 s after its last turn]. So:
    /// unless this knob was nudged within 1.5 s, two 8-count turns a frame apart
    /// to wake it, then 4 counts. With `func`, FUNC is held across the burst
    /// (what FUNC + a turn does on the hardware).
    pub fn nudge(&mut self, enc: u8, dir: i8, func: bool) {
        self.nudge_steps(enc, dir, 1, func);
    }

    /// `steps` value steps in one go: the wake (if needed), then one 4-count turn
    /// a frame for each step (4 counts a frame stay below the acceleration).
    pub fn nudge_steps(&mut self, enc: u8, dir: i8, steps: u8, func: bool) {
        const FRAME: u64 = 10_000_000;
        let now = self.now();
        let e = (enc as usize) & 15;
        let tail = self.nudges.back().map(|n| n.0 + FRAME).unwrap_or(now).max(now);
        // The A4 MKI takes encoder counts one to one, with no wake-up turn: one
        // count of +1 is one step [measured, OS 1.56: five turns of 1, 2, 4 and 8
        // counts 50 ms apart move OSC1 TUN by 5, 10, 20 and 40].
        let awake = self.is_mk1() || (self.nudge_last[e] != 0 && self.nudge_last[e] + 30 * FRAME > tail);
        let count = if self.is_mk1() { 1 } else { 4 };
        let mut t = tail;
        if func {
            self.nudges.push_back((t, Nudge::Key("FUNC", true)));
            t += FRAME / 2;
        }
        if !awake {
            for _ in 0..2 {
                self.nudges.push_back((t, Nudge::Turn(enc, dir.signum() * 8)));
                t += FRAME;
            }
        }
        for k in 0..steps.max(1) {
            if k > 0 {
                t += FRAME;
            }
            self.nudges.push_back((t, Nudge::Turn(enc, dir.signum() * count)));
        }
        self.nudge_last[e] = t;
        if func {
            self.nudges.push_back((t + FRAME / 2, Nudge::Key("FUNC", false)));
        }
    }

    /// Send the nudges that are due.
    pub fn run_nudges(&mut self) {
        let now = self.now();
        while let Some(&(at, n)) = self.nudges.front() {
            if at > now {
                break;
            }
            self.nudges.pop_front();
            match n {
                Nudge::Turn(e, d) => self.turn(e, d),
                Nudge::Key(name, down) => {
                    self.key_named(name, down);
                }
            }
        }
    }

    pub fn turn(&mut self, enc: u8, delta: i8) {
        // The A4 MKI's panel numbers its encoders B C D E G H I J A F LEVEL
        // (the OS's map 0x401ebdf8: wire channel -> knob) [measured]; `enc`
        // is the knob, A..J = 0..9, LEVEL 10.
        // The Rytm MKI's: B C D LEVEL F G H A E (0x40249fb8) [measured]; `enc`
        // is A..H = 0..7, LEVEL 8.
        const A4MK1_WIRE: [u8; 11] = [8, 0, 1, 2, 3, 9, 4, 5, 6, 7, 10];
        const RYTMMK1_WIRE: [u8; 9] = [7, 0, 1, 2, 8, 4, 5, 6, 3];
        let enc = match self.prof.device {
            Device::A4Mk1 => A4MK1_WIRE.get(enc as usize).copied().unwrap_or(enc),
            Device::RytmMk1 => RYTMMK1_WIRE.get(enc as usize).copied().unwrap_or(enc),
            _ => enc,
        };
        let msg = crate::panel::PanelMcu::encoder(enc, delta);
        self.panel_send(&msg);
    }

    /// Encoder counts per value step: the Digitakt's panel sends 1 per detent;
    /// the Rytm's sends about 8 - five turns of -2 left TRK LEVEL at 100, of -4
    /// moved it 1, of -8 moved it 4 (the first turn only shows the value)
    /// [measured, OS 1.73]; the A4 MKI takes one count per step (`nudge_steps`).
    /// The Syntakt is like the Rytm: on its tempo screen five turns of 2
    /// counts left the BPM alone, five of 4 or three of 8 moved it one step
    /// [measured, OS 1.42; 8 per detent inferred from the Rytm's].
    pub fn counts_per_detent(&self) -> i8 {
        if self.is_rytm() || self.is_a4() || self.is_syntakt() { 8 } else { 1 }
    }

    /// Turn an encoder by `detents` steps, as a user's knob turn (the window).
    /// `turn` takes raw panel counts.
    pub fn turn_detents(&mut self, enc: u8, detents: i8) {
        let d = (detents as i32 * self.counts_per_detent() as i32).clamp(-127, 127) as i8;
        self.turn(enc, d);
    }

    pub fn release_all_keys(&mut self) {
        let msg = self.cpu.bus.io.panel.release_all();
        if !msg.is_empty() {
            self.panel_send(&msg);
        }
    }

    /// Bytes from the panel MCU to the ColdFire, on UART8.
    pub fn panel_send(&mut self, bytes: &[u8]) {
        self.cpu.bus.io.uart_receive(8, bytes);
        self.cpu.bus.run_dma();
    }

    /// Panel frame as rows of pixels (true = lit), y = 0 at the top. A
    /// 1024-byte frame is the MKII's 128x64 OLED; a 512-byte one the MKI's
    /// 4-page LCD framebuffer (`frame_pixels_mk1`).
    pub fn frame_pixels(buf: &[u8]) -> Vec<Vec<bool>> {
        if buf.len() == 512 {
            return Self::frame_pixels_mk1(buf);
        }
        // SSD1306-style pages, page 0 at the bottom (a remapped COM scan):
        // byte = page + 8 * column, bit n = row 8 * (7 - page) + n.
        let mut rows = vec![vec![false; 128]; 64];
        for (y, row) in rows.iter_mut().enumerate() {
            for (x, px) in row.iter_mut().enumerate() {
                let b = buf[(7 - y / 8) + 8 * x];
                *px = (b >> (y % 8)) & 1 != 0;
            }
        }
        rows
    }

    /// The MKI's framebuffer as 122x32 pixels: byte = page + 4 * column
    /// (`0x4007333a` walks it so), bit n = row 8 * (3 - page) + n: page 0 is the
    /// bottom band, as on the MKII's OLED [measured: the main screen reads
    /// upright only so].
    pub fn frame_pixels_mk1(buf: &[u8]) -> Vec<Vec<bool>> {
        let mut rows = vec![vec![false; MK1_LCD_W]; MK1_LCD_H];
        for (y, row) in rows.iter_mut().enumerate() {
            for (x, px) in row.iter_mut().enumerate() {
                let b = buf[(3 - y / 8) + 4 * x];
                *px = (b >> (y % 8)) & 1 != 0;
            }
        }
        rows
    }

    /// The display as the panel MCU holds it (its tiles, `PanelMcu::oled`), as
    /// rows of pixels: the MKII's 128x64 OLED, or the MKI's 122x32 LCD, whose
    /// 4 pages arrive as tiles `0x10|page` (stored at page + 8 * column) with
    /// page 0 the bottom band, as the framebuffer's [measured].
    pub fn panel_pixels(&self) -> Vec<Vec<bool>> {
        let oled = &self.cpu.bus.io.panel.oled;
        if self.is_mk1() {
            let mut rows = vec![vec![false; MK1_LCD_W]; MK1_LCD_H];
            for (y, row) in rows.iter_mut().enumerate() {
                for (x, px) in row.iter_mut().enumerate() {
                    *px = (oled[(3 - y / 8) + 8 * x] >> (y % 8)) & 1 != 0;
                }
            }
            return rows;
        }
        Self::frame_pixels(oled)
    }

    /// Rows of pixels as text, two rows per line.
    pub fn pixels_ascii(rows: &[Vec<bool>]) -> String {
        let mut s = String::new();
        for y in (0..rows.len()).step_by(2) {
            for (&a, &b) in rows[y].iter().zip(&rows[y + 1]) {
                s.push(match (a, b) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    _ => ' ',
                });
            }
            s.push('\n');
        }
        s
    }

    pub fn frame_ascii(buf: &[u8]) -> String {
        let rows = Self::frame_pixels(buf);
        let mut s = String::new();
        // Two pixel rows per text line.
        for y in (0..rows.len()).step_by(2) {
            for (&a, &b) in rows[y].iter().zip(&rows[y + 1]) {
                s.push(match (a, b) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    _ => ' ',
                });
            }
            s.push('\n');
        }
        s
    }

    pub fn lit(buf: &[u8]) -> usize {
        buf.iter().map(|b| b.count_ones() as usize).sum()
    }
}

/// State of `Machine::add_heap_probe`.
pub struct HeapProbe {
    pub state: u32,
    pub pcs: Vec<u32>,
    pub min_free: u64,
    pub min_at: u64,
    pub min_live: u32,
    pub max_live: u32,
    pub samples: u64,
}
