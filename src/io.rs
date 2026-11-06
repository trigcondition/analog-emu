//! Peripheral models and their dispatch.
//!
//! Time is the CPU's instruction count (`now`). Peripheral clocks convert
//! through `ips`, emulated instructions per emulated second, against the
//! 132 MHz bus clock the reference established from three independent PIT
//! rates landing on round numbers (100 Hz, 119.996 Hz, 29.999 Hz). The PIT
//! counts the bus clock divided by 2^PRE: on a Rytm MKII running OS 1.74 the
//! UI tick driven by PIT2 was measured at 119.99 Hz over 416 s.
//!
//! Interrupt lines are level-sensitive, as on the silicon: a source stays
//! asserted until the firmware's handler clears the flag that raised it.

use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;
use std::collections::{HashMap, VecDeque};

pub const F_BUS: f64 = 132_000_000.0;

pub const INTC_BASE: [u32; 3] = [0xFC04_8000, 0xFC04_C000, 0xFC05_0000];
pub const PIT_BASE: u32 = 0xFC08_0000;
pub const DTIM_BASE: u32 = 0xFC07_0000;
pub const EDMA_BASE: u32 = 0xFC04_4000;
pub const TCD_BASE: u32 = 0xFC04_5000;
pub const UART8_BASE: u32 = 0xEC07_0000;
pub const UART9_BASE: u32 = 0xEC07_4000;
pub const GPIO_BASE: u32 = 0xEC09_4000;

/// An interrupt source: (controller, source number).
pub type Src = (usize, u32);

pub const SRC_PIT: [Src; 4] = [(2, 13), (2, 14), (2, 15), (2, 16)];
pub const SRC_DTIM: [Src; 4] = [(0, 32), (0, 33), (0, 34), (0, 35)];
pub const SRC_UART8: Src = (1, 52);
pub const SRC_UART9: Src = (1, 53);
pub const SRC_ESDHC: Src = (2, 31);

// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub struct Intc {
    pub imr: u64,
    pub frc: u64,
    #[serde(with = "BigArray")]
    pub icr: [u8; 64],
    pub iconfig: u16,
    /// Device lines, recomputed by `Io::update_irq`.
    pub lines: u64,
}

impl Intc {
    fn new() -> Intc {
        Intc { imr: u64::MAX, frc: 0, icr: [0; 64], iconfig: 0, lines: 0 }
    }
    fn pending(&self) -> u64 {
        self.lines | self.frc
    }

    /// What can interrupt the CPU. Software-forced sources are not gated by
    /// IMR: OS 1.53 raises INTC1 source 63 (the audio render) and INTC0
    /// sources 44 and 57 (the sequencer tick) only through INTFRC and never
    /// unmasks them, having masked every source at reset.
    fn deliverable(&self, ignore_masks: bool) -> u64 {
        if ignore_masks {
            self.pending()
        } else {
            (self.lines & !self.imr) | self.frc
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Pit {
    pub pcsr: u16,
    pub pmr: u16,
    /// Instruction time the counter was (re)loaded, and with what.
    t0: f64,
    load: u32,
    /// When PIF next sets, while enabled.
    pub next: Option<f64>,
    pub fired: u64,
}

const PIT_EN: u16 = 0x01;
const PIT_RLD: u16 = 0x02;
const PIT_PIF: u16 = 0x04;
const PIT_PIE: u16 = 0x08;
const PIT_OVW: u16 = 0x10;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Dtim {
    pub dtmr: u16,
    pub dtxmr: u8,
    pub dter: u8,
    pub dtrr: u32,
    pub dtcr: u32,
    t0: f64,
    pub next: Option<f64>,
    pub fired: u64,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Uart {
    pub umr: [u8; 2],
    pub umr_ptr: usize,
    pub uimr: u8,
    pub ucsr: u8,
    pub rx: VecDeque<u8>,
    pub tx: Vec<u8>,
    pub rx_enabled: bool,
    pub tx_enabled: bool,
}

/// eDMA controller registers; transfers run in `crate::edma` on the bus.
#[derive(Clone, Serialize, Deserialize)]
pub struct Edma {
    pub cr: u32,
    pub erq: u64,
    pub eei: u64,
    pub int: u64,
    pub err: u64,
    pub tcd: Vec<u8>,
    /// Channels with a software start or hardware request waiting.
    pub kick: u64,
    /// Channel -> instruction time before which it may not run again.
    #[serde(with = "BigArray")]
    pub busy_until: [u64; 64],
    /// Channels that asked while busy, re-offered by the service loop.
    pub deferred: u64,
    #[serde(with = "BigArray")]
    pub majors: [u64; 64],
}

/// SSI1, the codec interface. Each frame at `rate` asks eDMA channel 54
/// (transmit, 8 bytes: a stereo pair) and channel 52 (receive) for a minor
/// loop; words written to STX0/STX1 are the output samples, 24-bit and
/// right-justified.
#[derive(Serialize, Deserialize)]
pub struct Ssi {
    pub rate: f64,
    pub next: Option<f64>,
    pub tx_pending: u32,
    pub rx_pending: u32,
    pub frames: u64,
    /// Output samples (interleaved L/R), until someone drains them.
    pub out: VecDeque<i32>,
    pub out_cap: usize,
    /// Input samples fed to SRX0 (interleaved L/R).
    pub input: VecDeque<i32>,
    /// The analog model's speaker mix, one stereo pair per receive frame
    /// (`Bus::analog`); drained by the window.
    #[serde(skip)]
    pub voices: VecDeque<(f32, f32)>,
    /// The next word written to STX starts a frame (set by the eDMA engine
    /// when channel 54 reads from a frame boundary of its source ring).
    #[serde(skip)]
    pub frame_start: bool,
    /// The Rytm's AUDIO IN L/R (sampling, USB) and EXT IN L/R (into the main
    /// mix), one stereo pair per frame, -1..1; fed by the host (window input,
    /// `dtboot --audio-in`), consumed by the analog model.
    #[serde(skip)]
    pub audio_in: VecDeque<(f32, f32)>,
    #[serde(skip)]
    pub ext_in: VecDeque<(f32, f32)>,
    /// The DAC frame being written (word index `tx_pos`) and the last complete
    /// one, for the analog model's tom exciters (words 8..10).
    #[serde(skip)]
    pub tx_pos: usize,
    #[serde(skip)]
    pub tx_frame: [i32; 16],
    #[serde(skip)]
    pub tx_last: [i32; 16],
    pub regs: [u32; 16],
    pub enabled: bool,
    /// Receive words come sign-extended from 24 bits when SRCR's RXEXT (bit
    /// 10) asks for it (MCF54418RM). On for the Syntakt, whose CPU1 reads
    /// receive words 0..3 as signed 32-bit (0x400962ac) [measured: zero-extended,
    /// every negative sample of a track read as a large positive one]; the
    /// A4, Rytm and Digitakt set RXEXT too but keep the zero extension they
    /// have always had here. Not in snapshots: carried over on load.
    #[serde(skip)]
    pub rx_sign_extend: bool,
}

pub const SSI_TX_CHAN: usize = 54;
pub const SSI_RX_CHAN: usize = 52;

/// SSI0, the Rytm MKI's second codec port (docs/MK1-PORT.md section 10). The
/// OS runs it beside SSI1 on the same frame: eDMA channel 50 sends 8 words a
/// frame (STX0, source words 0..7 of a 16-word ring frame; SSI1 sends 8..15),
/// channel 48 receives 6 (SRX0, SRMSK 0xffffffc0) into a 10-word receive frame
/// whose last 4 come from SSI1 (SRMSK 0xffffffcc) [measured: the TCDs and masks
/// the OS sets up at 0x40006732..0x400069ba]. Off (`on` false) everywhere else.
/// Not kept in snapshots: `on` is carried over on load, the rest restarts.
#[derive(Default)]
pub struct Ssi0 {
    pub on: bool,
    pub regs: [u32; 20],
    pub tx_pending: u32,
    pub rx_pending: u32,
    /// The next STX word's place in the frame (0..7).
    pub tx_pos: usize,
    /// Receive words waiting for SRX0.
    pub input: VecDeque<i32>,
    /// SSI1's STMSK and SRMSK (0xFC0C8048 / 0x4C), which the MKII's request
    /// count does not use.
    pub ssi1_masks: [u32; 2],
}

pub const SSI0_TX_CHAN: usize = 50;
pub const SSI0_RX_CHAN: usize = 48;

#[derive(Serialize, Deserialize)]
pub struct Io {
    /// The USB controller's interrupt line (usbhost.rs keeps it current); not
    /// saved: a restored machine has no host attached.
    #[serde(skip)]
    pub usb_irq: bool,
    /// The simulated USB host's microframe clock (125 us): the next tick, and
    /// ticks due for `Machine` to run (usbhost.rs). Off (None) without a host.
    #[serde(skip)]
    pub usb_uframe: Option<u64>,
    #[serde(skip)]
    pub usb_uframes_due: u32,
    pub now: u64,
    /// The CPU stops when `now` reaches this; lowered by any event.
    pub deadline: u64,
    pub ips: f64,
    pub irq_level: u8,
    pub intc: [Intc; 3],
    pub pit: [Pit; 4],
    pub dtim: [Dtim; 4],
    /// Frequency of the DMA timers' external DTIN clock (DTMR CLK = 11), in Hz.
    /// 0: not connected, so a timer clocked from it does not count.
    #[serde(default)]
    pub dtin_hz: f64,
    pub uart8: Uart,
    pub panel: crate::panel::PanelMcu,
    pub uart9: Uart,
    /// MIDI waiting to arrive on UART9, the Rytm's DIN MIDI port (31250 baud,
    /// set up at 0x4000201c with 0x7a12; byte handler 0x400813c0). Fed to the
    /// receiver at the line's rate, 3125 bytes a second.
    #[serde(skip)]
    pub midi_in: VecDeque<u8>,
    #[serde(skip)]
    midi_next: Option<f64>,
    /// Whole Elektron-protocol SysEx messages waiting to go in as USB MIDI
    /// (the machine feeds them to the OS's USB MIDI queue), when `usb_midi`.
    #[serde(skip)]
    pub usb_in: VecDeque<Vec<u8>>,
    #[serde(skip)]
    pub usb_midi: bool,
    /// What the firmware sent on its USB MIDI port.
    #[serde(skip)]
    pub usb_midi_out: VecDeque<u8>,
    /// When set, the USB audio stream (12 channels, 24-bit, interleaved) is
    /// collected here as the firmware builds it.
    #[serde(skip)]
    pub usb_audio_capture: bool,
    #[serde(skip)]
    pub usb_audio: Vec<i32>,
    /// MIDI the firmware sent on UART9, until someone drains it.
    #[serde(skip)]
    pub midi_out: VecDeque<u8>,
    pub edma: Edma,
    pub esdhc: crate::esdhc::Esdhc,
    pub ssi: Ssi,
    #[serde(skip)]
    pub ssi0: Ssi0,
    /// Port D bit 4 drives port C bit 3 on the board (the SD gate).
    pub gpio_d4: bool,
    /// The supply has gone (`Machine::power_off`): PPDSDR_C bit 2 reads 0.
    /// The firmware senses the supply on that pin, through EPORT IRQ2 on both
    /// edges (vector 66, level 7; OS 1.73 0x400004b0) [measured]. Not in
    /// snapshots: a snapshot is a running machine.
    #[serde(skip)]
    pub power_lost: bool,
    /// The supercap is not charged: PPDSDR_A bit 5 reads 0. The firmware saves
    /// the working state at power-off only once it has seen that pin high
    /// (OS 1.73 0x40100a58 polls it, 0x419cacc0) [measured]; how long the cap
    /// takes to charge after power-on is not known, so it reads charged.
    #[serde(skip)]
    pub supercap_flat: bool,
    /// EPORT EPFR bits a model raised (bit 2: the power sense's edge); the
    /// firmware clears them by writing 1s. Pin flags nothing models stay
    /// plain memory.
    #[serde(skip)]
    pub epfr: u8,
    /// The firmware wrote RCR's SOFTRST (0x80 to the byte at 0xEC090000), as
    /// the OS upgrade does once the flash is written (Rytm MKII OS 1.74
    /// `0x4009f53a`). `Machine::run_until` stops with `Stop::Reset`; what
    /// happens next (a boot from the flash) is the tools' business.
    #[serde(skip)]
    pub reset_request: bool,
    /// The Analog Four MKII's CONTROL IN 1 and 2 (EXP/CV IN 1, 2): what each
    /// input sees (`CvIn`), read by the OS as the ColdFire ADC's channels 8
    /// and 9 (ADRSLT8/9, 0xFC09_4012/14; `0x4011806c`). None: nothing
    /// plugged in, the registers read back as memory. Set by `--cv-in` or
    /// `DTEMU_CV_IN` (`Io::cv_in_from`).
    #[serde(skip)]
    pub cv_in: [Option<CvIn>; 2],
    /// The A4's CV OUT A..D as the OS last sent them: voice frame words 56..59
    /// (`0x80003ddc + 2c`) [measured: CV A as VALUE LIN 0..5 V, VAL 0, 64, 127
    /// gave word 56 = 0, 0x13ff, 0x27af; 57..59 stayed 0], kept by bus.rs.
    #[serde(skip)]
    pub cv_out: [u16; 4],
    /// Registers that read as a fixed value (status bits nothing else sets).
    pub forced: HashMap<u32, u32>,
    /// Ignore IMR masking, as the reference emulator effectively did.
    pub ignore_masks: bool,
    pub trace_intc: bool,
    /// PIT channels whose interrupts are held off (the intro policy).
    pub pit_hold: u8,
    #[serde(with = "BigArray")]
    pub irq_taken: [u64; 256],
    /// Bytes per UART character time, in instructions.
    pub uart_byte_instr: u64,
    /// The Syntakt's link to its second CPU (syntakt.rs): the FPGA mailbox at
    /// 0x10000000, the kick and IRQ lines, the boot stream. None elsewhere.
    #[serde(default)]
    pub dsp: Option<Box<crate::syntakt::DspLink>>,
    /// The Syntakt's second CPU's own `Io`: its end of that link (syntakt.rs).
    /// Not in a snapshot of this `Io` (`snapshot.rs` saves the DSP apart).
    #[serde(skip)]
    pub dsp_side: Option<Box<crate::syntakt::DspSide>>,
}

impl Default for Io {
    fn default() -> Self {
        Self::new()
    }
}

impl Io {
    pub fn new() -> Io {
        let mut forced = HashMap::new();
        // DSPI0 SR: TCF|EOQF|RFDF set, RXCTR nonzero -- transfers complete at
        // once. From the reference's boot harness.
        forced.insert(0xFC05_C02C, 0x1000_00F0);
        // A serial block's TX-done status bit.
        forced.insert(0xEC03_802C, 0x8000_0000);
        Io {
            now: 0,
            deadline: u64::MAX,
            ips: 200_000_000.0,
            irq_level: 0,
            intc: [Intc::new(), Intc::new(), Intc::new()],
            pit: Default::default(),
            dtim: Default::default(),
            dtin_hz: 0.0,
            uart8: Uart::default(),
            panel: Default::default(),
            uart9: Uart::default(),
            midi_in: VecDeque::new(),
            midi_next: None,
            usb_in: VecDeque::new(),
            usb_midi: false,
            usb_midi_out: VecDeque::new(),
            usb_audio_capture: false,
            usb_audio: Vec::new(),
            midi_out: VecDeque::new(),
            edma: Edma {
                cr: 0,
                erq: 0,
                eei: 0,
                int: 0,
                err: 0,
                tcd: vec![0; 64 * 32],
                kick: 0,
                busy_until: [0; 64],
                deferred: 0,
                majors: [0; 64],
            },
            esdhc: crate::esdhc::Esdhc::new(crate::esdhc::Card::new()),
            usb_irq: false,
            usb_uframe: None,
            usb_uframes_due: 0,
            ssi0: Ssi0::default(),
            ssi: Ssi {
                rate: 48_000.0,
                next: None,
                tx_pending: 0,
                rx_pending: 0,
                frames: 0,
                out: VecDeque::new(),
                out_cap: 48_000 * 2 * 4,
                input: VecDeque::new(),
                voices: VecDeque::new(),
                frame_start: false,
                audio_in: VecDeque::new(),
                ext_in: VecDeque::new(),
                tx_pos: 0,
                tx_frame: [0; 16],
                tx_last: [0; 16],
                regs: [0; 16],
                enabled: true,
                rx_sign_extend: false,
            },
            gpio_d4: false,
            power_lost: false,
            supercap_flat: false,
            epfr: 0,
            reset_request: false,
            cv_in: Io::cv_in_from(std::env::var("DTEMU_CV_IN").ok().as_deref()),
            cv_out: [0; 4],
            forced,
            ignore_masks: false,
            trace_intc: std::env::var_os("DTEMU_TRACE_INTC").is_some(),
            pit_hold: 0,
            irq_taken: [0; 256],
            uart_byte_instr: 2000,
            dsp: None,
            dsp_side: None,
        }
    }

    fn cyc(&self, cycles: f64) -> f64 {
        cycles / F_BUS * self.ips
    }

    fn lower_deadline(&mut self, t: f64) {
        let t = if t < self.now as f64 { self.now } else { t.ceil() as u64 };
        if t < self.deadline {
            self.deadline = t;
        }
    }

    // -- interrupt controller ----------------------------------------------

    fn set_line(&mut self, s: Src, on: bool) {
        let bit = 1u64 << s.1;
        if on {
            self.intc[s.0].lines |= bit;
        } else {
            self.intc[s.0].lines &= !bit;
        }
    }

    /// Recompute every device line and the level the CPU sees.
    pub fn update_irq(&mut self) {
        for ch in 0..4 {
            let p = &self.pit[ch];
            let on = p.pcsr & PIT_PIF != 0 && p.pcsr & PIT_PIE != 0 && self.pit_hold & (1 << ch) == 0;
            self.set_line(SRC_PIT[ch], on);
            let d = &self.dtim[ch];
            let on = (d.dter & 0x02 != 0 && d.dtmr & 0x10 != 0 && d.dtxmr & 0x80 == 0)
                || (d.dter & 0x01 != 0 && d.dtmr & 0xC0 != 0);
            self.set_line(SRC_DTIM[ch], on);
        }
        let u8l = self.uart_isr(8) & self.uart8.uimr != 0;
        self.set_line(SRC_UART8, u8l);
        let u9l = self.uart_isr(9) & self.uart9.uimr != 0;
        self.set_line(SRC_UART9, u9l);
        let sd = self.esdhc.line();
        self.set_line(SRC_ESDHC, sd);
        self.set_line((2, 17), self.usb_irq);
        // EPORT IRQ1..7 are INTC0 sources 1..7.
        self.set_line((0, 2), self.epfr & 0x04 != 0);
        // IRQ4: the Syntakt's DSP (syntakt.rs); nothing else raises it here.
        self.set_line((0, 4), self.epfr & 0x10 != 0);
        // eDMA: channels 0-15 on INTC0 sources 8-23, 16-55 on INTC1 sources
        // 8-47 (source = channel - 8), 56-63 OR-ed onto INTC2 source 0.
        let int = self.edma.int;
        let lo = (int & 0xFFFF) << 8;
        self.intc[0].lines = (self.intc[0].lines & !(0xFFFFu64 << 8)) | lo;
        let mid = ((int >> 16) & ((1u64 << 40) - 1)) << 8;
        self.intc[1].lines = (self.intc[1].lines & !(((1u64 << 40) - 1) << 8)) | mid;
        self.set_line((2, 0), int >> 56 != 0);

        let mut best = 0u8;
        for c in &self.intc {
            let mut p = c.deliverable(self.ignore_masks);
            while p != 0 {
                let s = p.trailing_zeros();
                p &= p - 1;
                let l = c.icr[s as usize] & 7;
                if l > best {
                    best = l;
                }
            }
        }
        self.irq_level = best;
    }

    /// The CPU takes an interrupt: -> (vector, level) of the highest pending
    /// source above `ipl`. Ties go to the highest source number.
    pub fn ack_irq(&mut self, ipl: u8) -> Option<(u8, u8)> {
        let mut best: Option<(u8, usize, u32)> = None;
        for (ci, c) in self.intc.iter().enumerate() {
            let mut p = c.deliverable(self.ignore_masks);
            while p != 0 {
                let s = 63 - p.leading_zeros();
                p &= !(1u64 << s);
                let l = c.icr[s as usize] & 7;
                if l == 0 || (l <= ipl && l != 7) {
                    continue;
                }
                let better = match best {
                    None => true,
                    Some((bl, bc, bs)) => l > bl || (l == bl && (ci, s) > (bc, bs)),
                };
                if better {
                    best = Some((l, ci, s));
                }
            }
        }
        let (l, c, s) = best?;
        let vec = (64 + 64 * c as u32 + s) as u8;
        self.irq_taken[vec as usize] += 1;
        Some((vec, l))
    }

    fn intc_read(&mut self, c: usize, off: u32, size: u32) -> Option<u32> {
        let i = &self.intc[c];
        let p = i.pending();
        let v32 = |w: u64| w as u32;
        let r = match (off, size) {
            (0x00, 4) => v32(p >> 32),
            (0x04, 4) => v32(p),
            (0x08, 4) => v32(i.imr >> 32),
            (0x0C, 4) => v32(i.imr),
            (0x10, 4) => v32(i.frc >> 32),
            (0x14, 4) => v32(i.frc),
            (0x18, 2) => i.iconfig as u32,
            (0x40..=0x7F, 1) => i.icr[(off - 0x40) as usize] as u32,
            (0x1C..=0x1F, 1) => 0,
            _ => return None,
        };
        Some(r)
    }

    fn intc_write(&mut self, c: usize, off: u32, size: u32, v: u32) -> bool {
        if self.trace_intc && off < 0x20 {
            eprintln!("intc{c} +{off:02x}.{size} <- {v:08x} @ {}", self.now);
        }
        let i = &mut self.intc[c];
        match (off, size) {
            (0x08, 4) => i.imr = (i.imr & 0xFFFF_FFFF) | ((v as u64) << 32),
            (0x0C, 4) => i.imr = (i.imr & !0xFFFF_FFFF) | v as u64,
            (0x10, 4) => i.frc = (i.frc & 0xFFFF_FFFF) | ((v as u64) << 32),
            (0x14, 4) => i.frc = (i.frc & !0xFFFF_FFFF) | v as u64,
            (0x18, 2) => i.iconfig = v as u16,
            // SIMR / CIMR: an index, not a mask; bit 6 means every source.
            (0x1C, 1) => {
                if v & 0x40 != 0 {
                    i.imr = u64::MAX;
                } else {
                    i.imr |= 1u64 << (v & 0x3F);
                }
            }
            (0x1D, 1) => {
                if v & 0x40 != 0 {
                    i.imr = 0;
                } else {
                    i.imr &= !(1u64 << (v & 0x3F));
                }
            }
            (0x1E..=0x1F, 1) => {}
            (0x40..=0x7F, 1) => i.icr[(off - 0x40) as usize] = v as u8,
            (0x40..=0x7F, 2) => {
                i.icr[(off - 0x40) as usize] = (v >> 8) as u8;
                if off < 0x7F {
                    i.icr[(off - 0x3F) as usize] = v as u8;
                }
            }
            (0x40..=0x7F, 4) => {
                for k in 0..4 {
                    if off + k <= 0x7F {
                        i.icr[(off - 0x40 + k) as usize] = (v >> (24 - 8 * k)) as u8;
                    }
                }
            }
            (0x00..=0x07, _) => {}
            // Byte and word writes into the 32-bit registers.
            (0x08..=0x17, _) => {
                let reg = off & !3;
                let cur = self.intc_read(c, reg, 4).unwrap_or(0);
                let shift = 8 * (4 - size - (off & 3));
                let m = if size == 4 { u32::MAX } else { ((1u32 << (8 * size)) - 1) << shift };
                let nv = (cur & !m) | ((v << shift) & m);
                return self.intc_write(c, reg, 4, nv);
            }
            _ => return false,
        }
        true
    }

    // -- PIT ---------------------------------------------------------------

    fn pit_tick(&self, ch: usize) -> f64 {
        let pre = 1u32 << ((self.pit[ch].pcsr >> 8) & 0xF);
        self.cyc(pre as f64)
    }

    fn pit_restart(&mut self, ch: usize, load: u32) {
        let now = self.now as f64;
        let tick = self.pit_tick(ch);
        let p = &mut self.pit[ch];
        p.t0 = now;
        p.load = load;
        if p.pcsr & PIT_EN != 0 {
            let t = now + (load as f64 + 1.0) * tick;
            p.next = Some(t);
            self.lower_deadline(t);
        } else {
            p.next = None;
        }
    }

    fn pit_counter(&self, ch: usize) -> u32 {
        let p = &self.pit[ch];
        match p.next {
            Some(t) => {
                let left = ((t - self.now as f64) / self.pit_tick(ch)).floor();
                (left.max(0.0) as u32).saturating_sub(0).min(0xFFFF)
            }
            None => p.load & 0xFFFF,
        }
    }

    fn pit_read(&mut self, ch: usize, off: u32, size: u32) -> Option<u32> {
        let p = &self.pit[ch];
        Some(match (off, size) {
            (0, 2) => p.pcsr as u32,
            (0, 4) => (p.pcsr as u32) << 16 | p.pmr as u32,
            (1, 1) => p.pcsr as u32 & 0xFF,
            (0, 1) => (p.pcsr >> 8) as u32,
            (2, 2) => p.pmr as u32,
            (4, 2) => self.pit_counter(ch),
            (4, 4) => self.pit_counter(ch) << 16,
            _ => return None,
        })
    }

    fn pit_write(&mut self, ch: usize, off: u32, size: u32, v: u32) -> bool {
        match (off, size) {
            (0, 4) => {
                self.pit_write(ch, 0, 2, v >> 16);
                self.pit_write(ch, 2, 2, v & 0xFFFF);
            }
            (0, 2) | (1, 1) | (0, 1) => {
                let v = match (off, size) {
                    (1, 1) => (self.pit[ch].pcsr & 0xFF00) as u32 | (v & 0xFF),
                    (0, 1) => (self.pit[ch].pcsr & 0x00FF) as u32 | ((v & 0xFF) << 8),
                    _ => v,
                } as u16;
                let old = self.pit[ch].pcsr;
                // PIF is write-one-to-clear.
                let pif = if v & PIT_PIF != 0 { 0 } else { old & PIT_PIF };
                let new = (v & !PIT_PIF) | pif;
                self.pit[ch].pcsr = new;
                let restart = (new & PIT_EN != 0 && old & PIT_EN == 0) || ((new ^ old) & 0x0F00 != 0);
                if new & PIT_EN == 0 {
                    self.pit[ch].next = None;
                } else if restart {
                    let pmr = self.pit[ch].pmr as u32;
                    self.pit_restart(ch, pmr);
                }
            }
            (2, 2) => {
                self.pit[ch].pmr = v as u16;
                let p = &self.pit[ch];
                if p.pcsr & PIT_OVW != 0 || p.pcsr & PIT_EN == 0 {
                    self.pit_restart(ch, v & 0xFFFF);
                }
            }
            _ => return false,
        }
        true
    }

    // -- DMA timers ----------------------------------------------------------

    fn dtim_tick(&self, ch: usize) -> Option<f64> {
        let d = &self.dtim[ch];
        if d.dtmr & 1 == 0 {
            return None;
        }
        let div = match (d.dtmr >> 1) & 3 {
            1 => 1.0,
            2 => 16.0,
            3 if self.dtin_hz > 0.0 => {
                // DTIN: one count per external clock period, after the prescaler.
                return Some(self.ips / self.dtin_hz * ((d.dtmr >> 8) as f64 + 1.0));
            }
            _ => return None,
        };
        Some(self.cyc(((d.dtmr >> 8) as f64 + 1.0) * div))
    }

    fn dtim_schedule(&mut self, ch: usize) {
        let tick = self.dtim_tick(ch);
        let d = &mut self.dtim[ch];
        d.next = tick.map(|t| d.t0 + (d.dtrr as f64 + 1.0) * t);
        if let Some(t) = d.next {
            self.lower_deadline(t);
        }
    }

    fn dtim_counter(&self, ch: usize) -> u32 {
        match self.dtim_tick(ch) {
            None => 0,
            Some(t) => {
                let d = &self.dtim[ch];
                let n = ((self.now as f64 - d.t0) / t).max(0.0) as u64;
                if d.dtmr & 0x08 != 0 {
                    (n % (d.dtrr as u64 + 1)) as u32
                } else {
                    n as u32
                }
            }
        }
    }

    fn dtim_read(&mut self, ch: usize, off: u32, size: u32) -> Option<u32> {
        let d = &self.dtim[ch];
        Some(match (off, size) {
            (0, 2) => d.dtmr as u32,
            (0, 4) => (d.dtmr as u32) << 16 | (d.dtxmr as u32) << 8 | d.dter as u32,
            (2, 1) => d.dtxmr as u32,
            (3, 1) => d.dter as u32,
            (2, 2) => (d.dtxmr as u32) << 8 | d.dter as u32,
            (4, 4) => d.dtrr,
            (8, 4) => d.dtcr,
            (0xC, 4) => self.dtim_counter(ch),
            _ => return None,
        })
    }

    fn dtim_write(&mut self, ch: usize, off: u32, size: u32, v: u32) -> bool {
        let now = self.now as f64;
        match (off, size) {
            (0, 4) => {
                self.dtim_write(ch, 0, 2, v >> 16);
                self.dtim_write(ch, 2, 1, (v >> 8) & 0xFF);
                self.dtim_write(ch, 3, 1, v & 0xFF);
            }
            (0, 2) => {
                let old = self.dtim[ch].dtmr;
                let new = v as u16;
                self.dtim[ch].dtmr = new;
                if new & 1 == 0 || old & 1 == 0 || (old ^ new) & 0xFF06 != 0 {
                    self.dtim[ch].t0 = now;
                }
                self.dtim_schedule(ch);
            }
            (2, 1) => self.dtim[ch].dtxmr = v as u8,
            (3, 1) => self.dtim[ch].dter &= !(v as u8),
            (2, 2) => {
                self.dtim[ch].dtxmr = (v >> 8) as u8;
                self.dtim[ch].dter &= !(v as u8);
            }
            (4, 4) => {
                self.dtim[ch].dtrr = v;
                self.dtim_schedule(ch);
            }
            (8, 4) => self.dtim[ch].dtcr = v,
            (0xC, 4) => {
                self.dtim[ch].t0 = now;
                self.dtim_schedule(ch);
            }
            _ => return false,
        }
        true
    }

    // -- UART ----------------------------------------------------------------

    fn uart(&mut self, n: u32) -> &mut Uart {
        if n == 8 {
            &mut self.uart8
        } else {
            &mut self.uart9
        }
    }

    fn uart_usr(&self, n: u32) -> u32 {
        let u = if n == 8 { &self.uart8 } else { &self.uart9 };
        // TXEMP | TXRDY always: transmission completes instantly.
        let mut v = 0x0C;
        if !u.rx.is_empty() {
            v |= 0x01;
        }
        if u.rx.len() >= 4 {
            v |= 0x02;
        }
        v
    }

    fn uart_isr(&self, n: u32) -> u8 {
        let u = if n == 8 { &self.uart8 } else { &self.uart9 };
        let mut v = 0x01; // TXRDY
        if !u.rx.is_empty() {
            v |= 0x02;
        }
        v
    }

    fn uart_read(&mut self, n: u32, off: u32) -> Option<u32> {
        Some(match off {
            0x00 => {
                let u = self.uart(n);
                let v = u.umr[u.umr_ptr];
                u.umr_ptr = 1;
                v as u32
            }
            0x04 => self.uart_usr(n),
            0x0C => {
                let v = self.uart(n).rx.pop_front().unwrap_or(0);
                self.update_irq();
                v as u32
            }
            0x14 => self.uart_isr(n) as u32,
            0x10 => 0,
            _ => return None,
        })
    }

    fn uart_write(&mut self, n: u32, off: u32, v: u32) -> bool {
        let v = v as u8;
        match off {
            0x00 => {
                let u = self.uart(n);
                let p = u.umr_ptr;
                u.umr[p] = v;
                u.umr_ptr = 1;
            }
            0x04 => self.uart(n).ucsr = v,
            0x08 => {
                let u = self.uart(n);
                match (v >> 4) & 7 {
                    1 => u.umr_ptr = 0,
                    2 => u.rx.clear(),
                    _ => {}
                }
                match v & 3 {
                    1 => u.rx_enabled = true,
                    2 => u.rx_enabled = false,
                    _ => {}
                }
                match (v >> 2) & 3 {
                    1 => u.tx_enabled = true,
                    2 => u.tx_enabled = false,
                    _ => {}
                }
            }
            0x0C => {
                let u = self.uart(n);
                if u.tx.len() < (1 << 16) {
                    u.tx.push(v);
                }
                if n == 9 {
                    if self.midi_out.len() >= 1 << 16 {
                        self.midi_out.pop_front();
                    }
                    self.midi_out.push_back(v);
                }
                if n == 8 {
                    let mut reply = Vec::new();
                    self.panel.from_host(v, &mut reply);
                    if !reply.is_empty() {
                        self.uart_receive(8, &reply);
                    }
                }
            }
            0x14 => {
                self.uart(n).uimr = v;
                self.update_irq();
            }
            _ => return false,
        }
        true
    }

    /// Queue bytes on the DIN MIDI input; they arrive at the line's rate.
    ///
    /// Elektron-protocol SysEx (F0 00 20 3C 10: Transfer, elektroid) goes in as
    /// USB MIDI instead when `usb_midi`: a real Rytm serves that protocol only
    /// on USB, and DIN drops SysEx over 32767 bytes, less than an upload block.
    pub fn midi_send(&mut self, bytes: &[u8]) {
        if self.usb_midi {
            let mut rest = Vec::new();
            let mut i = 0;
            while i < bytes.len() {
                let end = if bytes[i] == 0xF0 { bytes[i..].iter().position(|&b| b == 0xF7).map(|e| i + e + 1) } else { None };
                match end {
                    Some(e) if bytes[i..e].starts_with(&[0xF0, 0x00, 0x20, 0x3C, 0x10]) => {
                        self.usb_in.push_back(bytes[i..e].to_vec());
                        i = e;
                    }
                    _ => {
                        rest.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            self.midi_in.extend(rest);
        } else {
            self.midi_in.extend(bytes.iter().copied());
        }
        let now = self.now;
        self.lower_deadline(now as f64);
    }

    /// MIDI arriving on the USB MIDI port: every message goes to the OS's USB
    /// MIDI queue (the machine feeds it in, short messages and SysEx alike).
    pub fn usb_midi_send(&mut self, bytes: &[u8]) {
        let mut i = 0;
        let mut running = 0u8;
        while i < bytes.len() {
            let b = bytes[i];
            let n = if b == 0xF0 {
                bytes[i..].iter().position(|&x| x == 0xF7).map_or(bytes.len() - i, |e| e + 1)
            } else if b >= 0xF8 {
                1
            } else if b >= 0x80 {
                running = if b < 0xF0 { b } else { 0 };
                1 + match b {
                    0xC0..=0xDF | 0xF1 | 0xF3 => 1,
                    0x80..=0xEF | 0xF2 => 2,
                    _ => 0,
                }
            } else if running != 0 {
                // running status: re-insert the status byte
                let n = match running {
                    0xC0..=0xDF => 1,
                    _ => 2,
                };
                let mut m = vec![running];
                m.extend_from_slice(&bytes[i..(i + n).min(bytes.len())]);
                self.usb_in.push_back(m);
                i += n;
                continue;
            } else {
                i += 1;
                continue;
            };
            self.usb_in.push_back(bytes[i..(i + n).min(bytes.len())].to_vec());
            i += n;
        }
    }

    /// Bytes arriving on a UART's receive line.
    pub fn uart_receive(&mut self, n: u32, data: &[u8]) {
        self.uart(n).rx.extend(data.iter().copied());
        // UART8 receive is DMA-driven (channel 34) when the firmware has
        // enabled that channel; the bus drains it.
        if n == 8 && self.edma.erq & (1 << 34) != 0 {
            self.edma.kick |= 1 << 34;
        }
        self.update_irq();
    }

    // -- eDMA registers ------------------------------------------------------

    fn edma_read(&mut self, off: u32, size: u32) -> Option<u32> {
        let e = &self.edma;
        Some(match (off, size) {
            (0x00, 4) => e.cr,
            (0x04, 4) => 0,
            (0x08, 4) => (e.erq >> 32) as u32,
            (0x0C, 4) => e.erq as u32,
            (0x10, 4) => (e.eei >> 32) as u32,
            (0x14, 4) => e.eei as u32,
            (0x18..=0x1F, 1) => 0,
            (0x20, 4) => (e.int >> 32) as u32,
            (0x24, 4) => e.int as u32,
            (0x28, 4) => (e.err >> 32) as u32,
            (0x2C, 4) => e.err as u32,
            (0x30, 4) | (0x34, 4) => 0,
            _ => return None,
        })
    }

    fn edma_write(&mut self, off: u32, size: u32, v: u32) -> bool {
        let e = &mut self.edma;
        let sel = |v: u32| -> u64 {
            if v & 0x40 != 0 {
                u64::MAX
            } else {
                1u64 << (v & 0x3F)
            }
        };
        match (off, size) {
            (0x00, 4) => e.cr = v,
            (0x08, 4) => e.erq = (e.erq & 0xFFFF_FFFF) | ((v as u64) << 32),
            (0x0C, 4) => e.erq = (e.erq & !0xFFFF_FFFF) | v as u64,
            (0x10, 4) => e.eei = (e.eei & 0xFFFF_FFFF) | ((v as u64) << 32),
            (0x14, 4) => e.eei = (e.eei & !0xFFFF_FFFF) | v as u64,
            (0x18, 1) => {
                e.erq |= sel(v);
                let req = self.dma_requesting();
                self.edma.kick |= sel(v) & req;
            }
            (0x19, 1) => self.edma.erq &= !sel(v),
            (0x1A, 1) => self.edma.eei |= sel(v),
            (0x1B, 1) => self.edma.eei &= !sel(v),
            (0x1C, 1) => {
                self.edma.int &= !sel(v);
                self.update_irq();
            }
            (0x1D, 1) => self.edma.err &= !sel(v),
            (0x1E, 1) => {
                // SSRT: software start.
                let m = sel(v);
                for ch in 0..64 {
                    if m & (1 << ch) != 0 {
                        let o = ch * 32 + 0x1E;
                        self.edma.tcd[o + 1] |= 0x01;
                    }
                }
                self.edma.kick |= m;
            }
            (0x1F, 1) => {
                let m = sel(v);
                for ch in 0..64 {
                    if m & (1 << ch) != 0 {
                        self.edma.tcd[ch * 32 + 0x1F] &= !0x80;
                    }
                }
            }
            (0x18, 4) => {
                for k in 0..4 {
                    self.edma_write(0x18 + k, 1, (v >> (24 - 8 * k)) & 0xFF);
                }
            }
            (0x1C, 4) => {
                for k in 0..4 {
                    self.edma_write(0x1C + k, 1, (v >> (24 - 8 * k)) & 0xFF);
                }
            }
            (0x20, 4) => {
                self.edma.int &= !((v as u64) << 32);
                self.update_irq();
            }
            (0x24, 4) => {
                self.edma.int &= !(v as u64);
                self.update_irq();
            }
            (0x28, 4) => self.edma.err &= !((v as u64) << 32),
            (0x2C, 4) => self.edma.err &= !(v as u64),
            (0x100..=0x13F, 1) => {}
            _ => return false,
        }
        true
    }

    /// Channels whose peripheral is asking for service right now.
    pub fn dma_requesting(&self) -> u64 {
        let mut m = 0u64;
        // UART8 TX is always ready; UART8 RX when it has bytes.
        m |= 1 << 35;
        if !self.uart8.rx.is_empty() {
            m |= 1 << 34;
        }
        // UART9 (the Rytm's DIN MIDI) likewise: TX channel 37, RX channel 36.
        m |= 1 << 37;
        if !self.uart9.rx.is_empty() {
            m |= 1 << 36;
        }
        if self.esdhc.dma_request() {
            m |= 1 << crate::esdhc::DMA_CHAN;
        }
        if self.ssi.tx_pending > 0 {
            m |= 1 << SSI_TX_CHAN;
        }
        if self.ssi.rx_pending > 0 {
            m |= 1 << SSI_RX_CHAN;
        }
        if self.ssi0.tx_pending > 0 {
            m |= 1 << SSI0_TX_CHAN;
        }
        if self.ssi0.rx_pending > 0 {
            m |= 1 << SSI0_RX_CHAN;
        }
        // The Syntakt's DSP clocks its boot stream out of DSPI2 (slave).
        if self.dsp.as_ref().is_some_and(|d| d.streaming(self.edma.majors[crate::syntakt::BOOT_CHAN])) {
            m |= 1 << crate::syntakt::BOOT_CHAN;
        }
        m
    }

    fn tcd_write(&mut self, off: u32, size: u32, v: u32) {
        let o = off as usize;
        for k in 0..size as usize {
            self.edma.tcd[o + k] = (v >> (8 * (size as usize - 1 - k))) as u8;
        }
        // Writing CSR with START set starts the channel.
        let ch = o / 32;
        let csr_lo = ch * 32 + 0x1F;
        if (o..o + size as usize).contains(&csr_lo) && self.edma.tcd[csr_lo] & 0x01 != 0 {
            self.edma.kick |= 1 << ch;
        }
    }

    /// Words in one SSI frame: STCCR's DC field + 1 (Digitakt 2, Rytm 16).
    pub fn ssi_frame_words(&self) -> u32 {
        ((self.ssi.regs[0x24 / 4] >> 8) & 0x1F) + 1
    }

    /// DMA requests channel `ch` gets per SSI frame: the frame's words over the
    /// words one request moves (its minor loop; NBYTES is the low 10 bits when a
    /// minor-loop offset is enabled). Digitakt 2/2 = 1; Rytm transmit 16/8 = 2,
    /// receive 16/6 -> 2 (its receive buffer keeps 12 words a frame).
    fn ssi_requests(&self, ch: usize) -> u32 {
        let nb = self.tcd_read((ch * 32 + 8) as u32, 4);
        let bytes = if nb & 0xC000_0000 != 0 { nb & 0x3FF } else { nb };
        let words = (bytes / 4).max(1);
        // The Rytm MKI masks time slots (STMSK / SRMSK): only the open ones
        // carry words, so SSI1's 4-word receive is one request a frame.
        let slots = if self.ssi0.on {
            let fw = self.ssi_frame_words().min(32);
            let all = if fw == 32 { u32::MAX } else { (1u32 << fw) - 1 };
            let mask = match ch {
                SSI_TX_CHAN => self.ssi0.ssi1_masks[0],
                SSI_RX_CHAN => self.ssi0.ssi1_masks[1],
                SSI0_TX_CHAN => self.ssi0.regs[0x48 / 4],
                _ => self.ssi0.regs[0x4C / 4],
            };
            (!mask & all).count_ones()
        } else {
            self.ssi_frame_words()
        };
        (slots / words).max(1)
    }

    fn tcd_read(&self, off: u32, size: u32) -> u32 {
        let o = off as usize;
        let mut v = 0u32;
        for k in 0..size as usize {
            v = v << 8 | self.edma.tcd[o + k] as u32;
        }
        v
    }

    // -- dispatch ------------------------------------------------------------

    /// -> Some(value) when a model answers the read.
    pub fn read(&mut self, a: u32, size: u32, _plain: u32, _pc: u32, _ddr: &mut [u8]) -> Option<u32> {
        if let Some(&v) = self.forced.get(&(a & !3)) {
            let sh = 8 * (4 - size - (a & 3));
            let m = if size == 4 { u32::MAX } else { (1u32 << (8 * size)) - 1 };
            return Some((v >> sh) & m);
        }
        if (a == 0xFC09_4012 || a == 0xFC09_4014) && size == 2 {
            if let Some(v) = self.adc_result(((a - 0xFC09_4012) / 2) as usize) {
                return Some(v);
            }
        }
        match a & 0xFFFF_C000 {
            0xFC04_8000 => return self.intc_read(0, a & 0x3FFF, size),
            0xFC04_C000 => return self.intc_read(1, a & 0x3FFF, size),
            0xFC05_0000 => return self.intc_read(2, a & 0x3FFF, size),
            0xFC08_0000 | 0xFC08_4000 | 0xFC08_8000 | 0xFC08_C000 => {
                return self.pit_read(((a >> 14) & 3) as usize, a & 0x3FFF, size)
            }
            0xFC07_0000 | 0xFC07_4000 | 0xFC07_8000 | 0xFC07_C000 => {
                return self.dtim_read(((a >> 14) & 3) as usize, a & 0x3FFF, size)
            }
            0xFC04_4000 => {
                let off = a & 0x3FFF;
                if (0x1000..0x1800).contains(&off) {
                    return Some(self.tcd_read(off - 0x1000, size));
                }
                return self.edma_read(off, size);
            }
            0xFC0B_C000 if self.ssi0.on && a & 0x3FFF < 0x50 => {
                let off = a & 0x7F;
                if (off == 0x08 || off == 0x0C) && size == 4 {
                    return Some(self.ssi0.input.pop_front().unwrap_or(0) as u32 & 0x00FF_FFFF);
                }
                if size == 4 && off & 3 == 0 {
                    return Some(self.ssi0.regs[(off / 4) as usize]);
                }
                return None;
            }
            0xFC0C_8000 if a & 0x3FFF < 0x40 => {
                let off = a & 0x3F;
                if (off == 0x08 || off == 0x0C) && size == 4 {
                    let v = self.ssi.input.pop_front().unwrap_or(0);
                    if self.ssi.rx_sign_extend && self.ssi.regs[0x20 / 4] & 0x400 != 0 {
                        return Some((v << 8 >> 8) as u32);
                    }
                    return Some(v as u32 & 0x00FF_FFFF);
                }
                if size == 4 && off & 3 == 0 {
                    return Some(self.ssi.regs[(off / 4) as usize]);
                }
                return None;
            }
            0xFC0C_C000 => {
                let r = self.esdhc.read(a & 0x3FFF, size);
                if r.is_some() {
                    // A DATPORT read may complete the transfer.
                    self.update_irq();
                }
                return r;
            }
            0xEC07_0000 if size == 1 && a & 3 == 0 => return self.uart_read(8, a & 0x3F),
            0xEC07_4000 if size == 1 && a & 3 == 0 => return self.uart_read(9, a & 0x3F),
            // PPDSDR_C: bit 3 follows the port D bit 4 the board loops back;
            // bit 2 is the power sense.
            0xEC09_4000 if a == 0xEC09_401A && size == 1 => {
                return Some(if self.gpio_d4 { 0x08 } else { 0 } | if self.power_lost { 0 } else { 0x04 });
            }
            // The Syntakt DSP's XC3S50A: INIT_B (PPDSDR_E bit 0) and DONE
            // (PPDSDR_F bit 7) read high: configured at once [inferred].
            0xEC09_4000 if self.dsp_side.is_some() && size == 1 && (a == 0xEC09_401C || a == 0xEC09_401D) => {
                return Some(if a == 0xEC09_401C { 0x01 } else { 0x80 });
            }
            // PPDSDR_A: bit 5 is the supercap's charged signal.
            0xEC09_4000 if a == 0xEC09_4018 && size == 1 => return Some(if self.supercap_flat { 0 } else { 0x20 }),
            _ => {}
        }
        None
    }

    /// -> true when a model consumed the write.
    pub fn write(&mut self, a: u32, size: u32, v: u32, _pc: u32, _ddr: &mut [u8]) -> bool {
        let r = match a & 0xFFFF_C000 {
            0xFC04_8000 => self.intc_write(0, a & 0x3FFF, size, v),
            0xFC04_C000 => self.intc_write(1, a & 0x3FFF, size, v),
            0xFC05_0000 => self.intc_write(2, a & 0x3FFF, size, v),
            0xFC08_0000 | 0xFC08_4000 | 0xFC08_8000 | 0xFC08_C000 => {
                self.pit_write(((a >> 14) & 3) as usize, a & 0x3FFF, size, v)
            }
            0xFC07_0000 | 0xFC07_4000 | 0xFC07_8000 | 0xFC07_C000 => {
                self.dtim_write(((a >> 14) & 3) as usize, a & 0x3FFF, size, v)
            }
            0xFC04_4000 => {
                let off = a & 0x3FFF;
                if (0x1000..0x1800).contains(&off) {
                    self.tcd_write(off - 0x1000, size, v);
                    true
                } else {
                    self.edma_write(off, size, v)
                }
            }
            0xFC0B_C000 if self.ssi0.on && a & 0x3FFF < 0x50 => {
                let off = a & 0x7F;
                if (off == 0x00 || off == 0x04) && size == 4 {
                    // SSI0's words are DAC frame words 0..7 (SSI1's 8..15).
                    let smp = ((v << 8) as i32) >> 8;
                    let p = self.ssi0.tx_pos;
                    if p < 8 {
                        self.ssi.tx_frame[p] = smp;
                    }
                    self.ssi0.tx_pos += 1;
                    true
                } else if size == 4 && off & 3 == 0 {
                    // Kept here and in memory, where a snapshot keeps them
                    // (`snapshot::load` reads the masks back).
                    self.ssi0.regs[(off / 4) as usize] = v;
                    false
                } else {
                    false
                }
            }
            0xFC0C_8000 if self.ssi0.on && (a == 0xFC0C_8048 || a == 0xFC0C_804C) && size == 4 => {
                self.ssi0.ssi1_masks[((a >> 2) & 1) as usize] = v;
                false
            }
            0xFC0C_8000 if a & 0x3FFF < 0x40 => {
                let off = a & 0x3F;
                if (off == 0x00 || off == 0x04) && size == 4 {
                    let smp = ((v << 8) as i32) >> 8;
                    let fw = self.ssi_frame_words().max(1) as usize;
                    let ssi = &mut self.ssi;
                    if ssi.frame_start {
                        // Keep `out` whole frames from word 0: drop a partial
                        // frame left before this one.
                        ssi.frame_start = false;
                        ssi.tx_pos = 0;
                        let n = ssi.out.len();
                        ssi.out.truncate(n - n % fw);
                    }
                    if ssi.out.len() >= ssi.out_cap {
                        ssi.out.drain(..fw.min(ssi.out.len()));
                    }
                    ssi.out.push_back(smp);
                    // With SSI0 on, SSI1 carries DAC words 8..15.
                    let at = if self.ssi0.on { 8 + ssi.tx_pos } else { ssi.tx_pos };
                    if at < 16 {
                        ssi.tx_frame[at] = smp;
                    }
                    ssi.tx_pos += 1;
                    if ssi.tx_pos == fw {
                        ssi.tx_last = ssi.tx_frame;
                        ssi.tx_pos = 0;
                    }
                    true
                } else if size == 4 && off & 3 == 0 {
                    self.ssi.regs[(off / 4) as usize] = v;
                    true
                } else {
                    false
                }
            }
            0xFC0C_C000 => {
                let r = self.esdhc.write(a & 0x3FFF, size, v);
                // Issuing a data command asks channel 59 for service.
                if self.edma.erq & (1 << crate::esdhc::DMA_CHAN) != 0 && self.esdhc.dma_request() {
                    self.edma.kick |= 1 << crate::esdhc::DMA_CHAN;
                }
                r
            }
            0xEC07_0000 if size == 1 && a & 3 == 0 => self.uart_write(8, a & 0x3F, v),
            0xEC07_4000 if size == 1 && a & 3 == 0 => self.uart_write(9, a & 0x3F, v),
            0xEC03_8000 if self.dsp.is_some() => {
                let majors = self.edma.majors[crate::syntakt::BOOT_CHAN];
                let d = self.dsp.as_mut().unwrap();
                // DSPI2 (slave): the DSP's boot port. MCR out of HALT (and
                // not master) after a reset opens the stream; PUSHR's low
                // byte is what the DSP reads.
                if a == 0xEC03_8000 && size == 4 && v & 0x8000_0001 == 0 && d.dsp.phase == crate::syntakt::Phase::Off {
                    d.stream_open(majors);
                    self.edma.kick |= 1 << crate::syntakt::BOOT_CHAN;
                }
                if a == 0xEC03_8037 && size == 1 {
                    d.boot_byte(v as u8);
                }
                false
            }
            // The Syntakt DSP's IRQ to CPU1: port G bit 2 (PPDSDR_G sets,
            // PCLRR_G clears).
            0xEC09_4000 if self.dsp_side.is_some() && size == 1 && (a == 0xEC09_401E || a == 0xEC09_402A) => {
                let now = self.now;
                let s = self.dsp_side.as_mut().unwrap();
                if a == 0xEC09_401E && v & 0x04 != 0 {
                    s.set_irq_line(true, now);
                }
                if a == 0xEC09_402A && v & 0x04 == 0 {
                    s.set_irq_line(false, now);
                }
                false
            }
            0xEC09_4000 if self.dsp.is_some() && size == 1 && matches!(a, 0xEC09_4018 | 0xEC09_4024 | 0xEC09_4026) => {
                let now = self.now;
                let d = self.dsp.as_mut().unwrap();
                match a {
                    // Port A bit 4: the kick line (PPDSDR sets, PCLRR clears).
                    0xEC09_4018 if v & 0x10 != 0 => d.set_kick_line(true, now),
                    0xEC09_4024 if v & 0x10 == 0 => d.set_kick_line(false, now),
                    // PCLRR_C bit 4 low starts each boot attempt (0x40082c6e):
                    // the DSP is held in reset [inferred].
                    0xEC09_4026 if v & 0x10 == 0 => d.hold_reset(),
                    _ => {}
                }
                if let Some(t) = d.irq_at {
                    self.lower_deadline(t as f64);
                }
                false
            }
            0xEC09_0000 if a == 0xEC09_0000 && size == 1 && v & 0x80 != 0 => {
                self.reset_request = true;
                let now = self.now;
                self.lower_deadline(now as f64);
                false
            }
            0xEC09_4000 => {
                // PPDSDR_D: writing 1 sets; PCLRR_D: writing 0 clears.
                if a == 0xEC09_401B && size == 1 && v & 0x10 != 0 {
                    self.gpio_d4 = true;
                }
                if a == 0xEC09_4027 && size == 1 && v & 0x10 == 0 {
                    self.gpio_d4 = false;
                }
                false
            }
            _ => false,
        };
        if r {
            self.update_irq();
        }
        r
    }

    /// Advance timers to `now`; set flags for everything due. Then set
    /// `deadline` to the next event.
    pub fn service(&mut self) {
        let now = self.now as f64;
        let mut next = f64::INFINITY;
        for ch in 0..4 {
            if let Some(t) = self.pit[ch].next {
                if now >= t {
                    let tick = self.pit_tick(ch);
                    let p = &mut self.pit[ch];
                    p.pcsr |= PIT_PIF;
                    p.fired += 1;
                    let reload = if p.pcsr & PIT_RLD != 0 { p.pmr as u32 } else { 0xFFFF };
                    p.load = reload;
                    let mut nt = t + (reload as f64 + 1.0) * tick;
                    if nt <= now {
                        nt = now + (reload as f64 + 1.0) * tick;
                    }
                    p.t0 = t;
                    p.next = Some(nt);
                }
                next = next.min(self.pit[ch].next.unwrap());
            }
            if let Some(t) = self.dtim[ch].next {
                if now >= t {
                    let tick = self.dtim_tick(ch).unwrap_or(1.0);
                    let d = &mut self.dtim[ch];
                    d.dter |= 0x02;
                    d.fired += 1;
                    let period = (d.dtrr as f64 + 1.0) * tick;
                    if d.dtmr & 0x08 != 0 {
                        let mut nt = t + period;
                        if nt <= now {
                            nt = now + period;
                        }
                        d.t0 = nt - period;
                        d.next = Some(nt);
                    } else {
                        // Free run: the counter goes round all 32 bits.
                        d.next = Some(t + 4_294_967_296.0 * tick);
                    }
                }
                next = next.min(self.dtim[ch].next.unwrap());
            }
        }
        // The SSI frame clock runs while either of its channels is enabled.
        let ssi_on = self.ssi.enabled
            && self.edma.erq & ((1 << SSI_TX_CHAN) | (1 << SSI_RX_CHAN)) != 0;
        if ssi_on {
            let period = self.ips / self.ssi.rate;
            let t = *self.ssi.next.get_or_insert(now + period);
            if now >= t {
                let mut n = ((now - t) / period).floor() as u64 + 1;
                // Frames the DMA could not keep up with are dropped, as a
                // real FIFO underrun would drop them.
                n = n.min(64);
                self.ssi.next = Some(t + n as f64 * period);
                if self.ssi.next.unwrap() <= now {
                    self.ssi.next = Some(now + period);
                }
                self.ssi.frames += n;
                if self.edma.erq & (1 << SSI_TX_CHAN) != 0 {
                    self.ssi.tx_pending += n as u32 * self.ssi_requests(SSI_TX_CHAN);
                    self.edma.kick |= 1 << SSI_TX_CHAN;
                }
                if self.edma.erq & (1 << SSI_RX_CHAN) != 0 {
                    self.ssi.rx_pending += n as u32 * self.ssi_requests(SSI_RX_CHAN);
                    self.edma.kick |= 1 << SSI_RX_CHAN;
                }
                if self.ssi0.on {
                    for (ch, rx) in [(SSI0_TX_CHAN, false), (SSI0_RX_CHAN, true)] {
                        if self.edma.erq & (1 << ch) != 0 {
                            let k = n as u32 * self.ssi_requests(ch);
                            if rx {
                                self.ssi0.rx_pending += k;
                            } else {
                                self.ssi0.tx_pending += k;
                            }
                            self.edma.kick |= 1 << ch;
                        }
                    }
                }
            }
            next = next.min(self.ssi.next.unwrap());
        } else {
            self.ssi.next = None;
        }
        // USB microframes, 8000 a second, while a simulated host is attached.
        if let Some(t) = self.usb_uframe {
            let period = (self.ips / 8000.0) as u64;
            let mut t = t;
            while self.now >= t {
                self.usb_uframes_due = self.usb_uframes_due.saturating_add(1).min(64);
                t += period;
            }
            self.usb_uframe = Some(t);
            next = next.min(t as f64);
        }
        // DIN MIDI in, one byte per 1/3125 s.
        if self.midi_in.is_empty() {
            self.midi_next = None;
        } else {
            let period = self.ips / 3125.0;
            let t = *self.midi_next.get_or_insert(now);
            if now >= t {
                if let Some(b) = self.midi_in.pop_front() {
                    self.uart9.rx.push_back(b);
                    // UART9 receive is DMA-driven on the Rytm (channel 36, into a
                    // ring at 0x4b800000; transmit is channel 37) [measured].
                    if self.edma.erq & (1 << 36) != 0 {
                        self.edma.kick |= 1 << 36;
                    }
                    self.update_irq();
                }
                self.midi_next = Some(t.max(now) + period);
            }
            next = next.min(self.midi_next.unwrap());
        }
        // The Syntakt DSP: CPU1's kicks are edges on DTIN0, which DTIM0
        // captures (DTMR CE, `0x40000e40`: 0xc1, either edge).
        if let Some(s) = self.dsp_side.as_mut() {
            let mut cap = false;
            while s.kicks.front().is_some_and(|&t| t <= self.now) {
                s.kicks.pop_front();
                s.kicks_taken += 1;
                cap = true;
            }
            if let Some(&t) = s.kicks.front() {
                next = next.min(t as f64);
            }
            if cap && self.dtim[0].dtmr & 0xC0 != 0 && self.dtim[0].dtmr & 1 != 0 {
                self.dtim[0].dter |= 0x01;
                self.dtim[0].dtcr = self.dtim_counter(0);
            }
        }
        if let Some(d) = self.dsp.as_mut() {
            let majors = self.edma.majors[crate::syntakt::BOOT_CHAN];
            if let Some(t) = d.service(self.now, majors) {
                next = next.min(t as f64);
            }
            if d.take_irq(self.now) {
                self.epfr |= 0x10;
            }
            if let Some(t) = d.irq_at {
                next = next.min(t as f64);
            }
        }
        for ch in 0..64 {
            let b = self.edma.busy_until[ch];
            if b > self.now {
                next = next.min(b as f64);
            }
        }
        self.update_irq();
        self.deadline = if next.is_finite() { next.ceil().max(now + 1.0) as u64 } else { u64::MAX };
    }
}


/// What a CONTROL IN of the Analog Four MKII sees (`Io::cv_in`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CvIn {
    /// A fixed voltage, in millivolts (a CV source or a pedal held still).
    Volts(i32),
    /// One of the A4's own CV outputs (0..3: A..D) patched into it with a cable.
    Out(usize),
}

/// The CV outputs' and inputs' scales. Out: the OS's own word for a voltage
/// with the emulator's calibration record (gains 0x40000000, offsets 0;
/// machine.rs) [measured: VALUE LIN 0..5 V at VAL 127 sends 0x27af, at 64
/// 0x13ff], so 10159 a word is 5000 mV (a unit's DAC and its calibration are
/// not modelled). The input side follows
/// the OS's own uncalibrated ADC conversion (`0x401182b0`: ADC units = 16295 +
/// mV x 32000 / 15676, the image's defaults at `0x40219fd4`/`0x40219fdc`), so
/// a voltage reads back as the same millivolts through the OS's CV ZERO and
/// MAX LEVEL settings; the result register holds those 15-bit units with the
/// low 3 bits clear (a 12-bit result, left-justified).
pub const CV_OUT_WORD_5V: i32 = 10_159;
pub const ADC_ZERO: i32 = 16_295;
pub const ADC_GAIN: i32 = 15_676;

impl Io {
    /// `--cv-in` / `DTEMU_CV_IN`: comma-separated `PORT=SOURCE`, PORT 1 or 2,
    /// SOURCE `A`..`D` (that CV output, looped) or millivolts (`2500`, `-1200mV`).
    /// Anything else is ignored with a message.
    pub fn cv_in_from(spec: Option<&str>) -> [Option<CvIn>; 2] {
        let mut out = [None, None];
        for part in spec.unwrap_or("").split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let parsed = part.split_once('=').and_then(|(p, s)| {
                let port = match p.trim() { "1" => 0, "2" => 1, _ => return None };
                let s = s.trim();
                let src = match s.to_ascii_uppercase().as_str() {
                    "A" => CvIn::Out(0),
                    "B" => CvIn::Out(1),
                    "C" => CvIn::Out(2),
                    "D" => CvIn::Out(3),
                    v => CvIn::Volts(v.trim_end_matches("MV").trim().parse().ok()?),
                };
                Some((port, src))
            });
            match parsed {
                Some((port, src)) => out[port] = Some(src),
                None => eprintln!("cv-in: '{part}' ignored (PORT=SOURCE: 1 or 2 = A..D or millivolts)"),
            }
        }
        out
    }

    /// CV OUT n's voltage, in millivolts.
    pub fn cv_out_mv(&self, n: usize) -> i32 {
        self.cv_out[n] as i16 as i32 * 5_000 / CV_OUT_WORD_5V
    }

    /// ADRSLT8/9 for CONTROL IN `port` (0, 1), or None when nothing is plugged in.
    fn adc_result(&self, port: usize) -> Option<u32> {
        let mv = match self.cv_in.get(port).copied().flatten()? {
            CvIn::Volts(mv) => mv,
            CvIn::Out(n) => self.cv_out_mv(n),
        };
        let units = (ADC_ZERO as i64 + mv as i64 * 32_000 / ADC_GAIN as i64).clamp(0, 0x7FF8) as u32;
        Some(units & !7)
    }
}
