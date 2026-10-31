//! A simulated USB host on the Rytm's USB port, driving the device controller
//! (`usbdev.rs`) the way a real host would: VBUS, bus reset, a high-speed port,
//! enumeration, then Overbridge's audio stream (Overwitch, src/engine.c:
//! configuration 1, interfaces 1 and 2 at alternate setting 3, interrupt
//! endpoints 0x83 IN and 0x03 OUT; blocks of a 32-byte header, `0x07ff` magic and
//! a frame counter in front, then 7 frames x 12 channels of big-endian int32).
//!
//! The controller side, ChipIdea-style (MCF5441x RM, USB OTG): endpoint queue
//! heads (dQH, 64 bytes, index 2 x endpoint + direction, at EPLISTADDR) hold the
//! setup packet at +0x28 and the overlay of the transfer descriptor list at +8;
//! transfer descriptors (dTD) are big-endian: +0 next (bit 0 terminate), +4 token
//! (total bytes 30:16, IOC bit 15, active bit 7), +8.. five buffer pointers.
//! Completing a dTD clears active, leaves the untransferred count in the token,
//! advances the overlay and, with IOC, sets the endpoint's EPCOMPLETE bit and
//! USBSTS.UI. The controller interrupts on INTC2 source 17 (vector 209,
//! handler 0x400040ae in OS 1.73) [measured].
//!
//! `device_stack` (the Digitakt, `Machine::usb_microframes`): the host reads the
//! configuration descriptor instead of assuming the Rytm's, so all of a USB
//! CONFIG mode's interfaces come up as the firmware offers them; it comes back
//! when the firmware leaves the bus and returns (USBCMD.RS, on a mode change);
//! MIDI goes to the device on the bulk OUT endpoint; the Overbridge OUT stream
//! takes the descriptor's packet size (Digitakt: 88 bytes, 2 channels), and the
//! vendor interface's interrupt IN (0x82) is read. Off, the Rytm's fixed host
//! runs exactly as before; the Rytm's USB MIDI input stays the OS queue stand-in.

use std::collections::VecDeque;

use crate::bus::Bus;

const R: u32 = 0xFC0B_0000;
const USBCMD: u32 = R + 0x140;
const USBSTS: u32 = R + 0x144;
const USBINTR: u32 = R + 0x148;
const FRINDEX: u32 = R + 0x14C;
const EPLISTADDR: u32 = R + 0x158;
const PORTSC1: u32 = R + 0x184;
const OTGSC: u32 = R + 0x1A4;
const EPSETUPSR: u32 = R + 0x1AC;
const EPSR: u32 = R + 0x1B8;
const EPCOMPLETE: u32 = R + 0x1BC;
const EPNAK: u32 = R + 0x178;

const STS_UI: u32 = 1 << 0;
const STS_PCI: u32 = 1 << 2;
const STS_URI: u32 = 1 << 6;
const STS_SRI: u32 = 1 << 7;
const OTG_BSV: u32 = 1 << 11;
const OTG_AVV: u32 = 1 << 9;
const OTG_ASV: u32 = 1 << 10;
const OTG_BSE: u32 = 1 << 12;
const OTG_BSVIS: u32 = 1 << 19;

pub const OB_FRAMES_PER_BLOCK: usize = 7;
pub const OB_CHANNELS: usize = 12;
pub const OB_BLOCK: usize = 32 + OB_FRAMES_PER_BLOCK * OB_CHANNELS * 4; // 368

/// What a configuration descriptor asks of a host (`UsbHost::device_stack`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConfigInfo {
    /// Vendor (Overbridge) interfaces with alternate settings: (interface, the
    /// alternate with the largest packet), as Overwitch takes alt 3 on 1 and 2.
    pub alts: Vec<(u8, u8)>,
    /// That alternate's interrupt packet sizes: endpoint 0x83 IN, 0x03 OUT.
    pub ob_in_packet: usize,
    pub ob_out_packet: usize,
}

/// Walk a configuration descriptor (USB 2.0, 9.6): interfaces, their alternate
/// settings and endpoints; anything else is skipped by its length.
pub fn parse_config(d: &[u8]) -> ConfigInfo {
    let mut info = ConfigInfo::default();
    let (mut iface, mut alt, mut class) = (0u8, 0u8, 0u8);
    let mut o = 0;
    while o + 2 <= d.len() && d[o] >= 2 && o + d[o] as usize <= d.len() {
        let x = &d[o..o + d[o] as usize];
        match x[1] {
            4 if x.len() >= 9 => (iface, alt, class) = (x[2], x[3], x[5]),
            5 if x.len() >= 7 && class == 0xFF && alt != 0 && x[3] & 3 == 3 => {
                let mps = u16::from_le_bytes([x[4], x[5]]) as usize & 0x7FF;
                let slot = if x[2] & 0x80 != 0 { &mut info.ob_in_packet } else { &mut info.ob_out_packet };
                // A later alternate wins a tie: alt 3 of the Digitakt's OUT is 88 bytes, as alt 2.
                let e = info.alts.iter_mut().find(|e| e.0 == iface);
                match e {
                    Some(e) if mps >= *slot => {
                        e.1 = alt;
                        *slot = mps;
                    }
                    Some(_) => {}
                    None => {
                        info.alts.push((iface, alt));
                        *slot = mps;
                    }
                }
            }
            _ => {}
        }
        o += d[o] as usize;
    }
    info
}

/// MIDI bytes to USB-MIDI event packets (USB MIDI 1.0, table 4-1), cable 0: the
/// parser's state across calls, for a stream cut anywhere (running status, a
/// SysEx in pieces).
#[derive(Default)]
pub struct MidiPacker {
    status: u8,
    pend: Vec<u8>,
    sysex: bool,
}

impl MidiPacker {
    fn packet(cin: u8, b: &[u8], out: &mut VecDeque<u8>) {
        out.push_back(cin);
        out.extend((0..3).map(|k| b.get(k).copied().unwrap_or(0)));
    }

    pub fn feed(&mut self, bytes: &[u8], out: &mut VecDeque<u8>) {
        for &b in bytes {
            if b >= 0xF8 {
                Self::packet(0xF, &[b], out); // realtime, anywhere
            } else if self.sysex {
                self.pend.push(b);
                if b == 0xF7 {
                    Self::packet([5, 6, 7][self.pend.len() - 1], &self.pend, out);
                    self.pend.clear();
                    self.sysex = false;
                } else if self.pend.len() == 3 {
                    Self::packet(4, &self.pend, out);
                    self.pend.clear();
                }
            } else if b == 0xF0 {
                self.sysex = true;
                self.status = 0;
                self.pend = vec![b];
            } else if b == 0xF6 {
                self.status = 0;
                Self::packet(5, &[b], out);
            } else if b >= 0x80 {
                self.status = b;
                self.pend.clear();
            } else if self.status != 0 {
                if self.pend.is_empty() {
                    self.pend.push(self.status);
                }
                self.pend.push(b);
                let s = self.status;
                let need = if matches!(s, 0xC0..=0xDF | 0xF1 | 0xF3) { 2 } else { 3 };
                if self.pend.len() == need {
                    let cin = if s >= 0xF0 { need as u8 } else { s >> 4 };
                    Self::packet(cin, &self.pend, out);
                    self.pend.clear();
                    if s >= 0xF0 {
                        self.status = 0; // no running status for system common
                    }
                }
            }
        }
    }
}

/// The interrupt line: an enabled USBSTS bit, or an enabled OTGSC status bit.
pub fn line(bus: &Bus) -> bool {
    let sts = bus.peek32(USBSTS) & bus.peek32(USBINTR);
    let otg = bus.peek32(OTGSC);
    sts != 0 || ((otg >> 16) & (otg >> 24) & 0x7F) != 0
}

fn raise(bus: &mut Bus, sts: u32) {
    let v = bus.peek32(USBSTS) | sts;
    bus.poke32(USBSTS, v);
    bus.io.usb_irq = line(bus);
    bus.io.update_irq();
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    /// Waiting for the firmware to start the controller (USBCMD.RS) in device mode.
    Idle,
    /// VBUS raised; waiting for RS.
    Vbus,
    /// Bus reset signalled at `t`.
    Reset,
    /// Port reported; control requests from here.
    Enumerate,
    /// Configured: the audio stream runs.
    Stream,
}

#[derive(Clone, Copy, Debug)]
enum Stage {
    /// Setup packet posted, waiting for the data stage's dTD (or the status).
    Data,
    /// Data done, waiting for the status stage's dTD.
    Status,
}

struct Ctl {
    setup: [u8; 8],
    out: Vec<u8>,
    stage: Stage,
    got: Vec<u8>,
    t0: u64,
}

pub struct UsbHost {
    phase: Phase,
    /// Microframes since the host was attached, and the phase's start.
    uf: u64,
    t: u64,
    queue: VecDeque<[u8; 8]>,
    ctl: Option<Ctl>,
    log: bool,
    /// Descriptors read during enumeration (device, configuration).
    pub device_desc: Vec<u8>,
    pub config_desc: Vec<u8>,
    /// Rytm -> host frames received (12 channels), for the caller to take.
    pub from_rytm: VecDeque<[i32; OB_CHANNELS]>,
    /// Frames the host may still send (Overwitch sends as much as it receives).
    credit: usize,
    out_counter: u16,
    /// IN bytes the endpoint may still move: a high-speed interrupt endpoint with
    /// a 368-byte packet and bInterval 1 moves one packet per 125 us microframe.
    in_budget: usize,
    /// Read the MIDI streaming interface's bulk IN endpoint (0x81), as a host
    /// with the port open does. Only used when the firmware's own USB MIDI
    /// sender runs (`Machine::usb_midi_real`); otherwise the emulator takes
    /// the sender's calls and nothing is queued there.
    pub midi_in: bool,
    /// Clocks [from, to) during which the host stops reading MIDI IN but
    /// keeps the bus running (an application with the port closed, a stalled
    /// host): SOFs and the audio stream go on, the sender's requests stay
    /// queued.
    pub midi_sleep: Option<(u64, u64)>,
    /// Clocks [from, to) during which the host suspends the bus (a sleeping
    /// laptop): no SOF (FRINDEX stops), no transfers; PORTSC1.SUSP and
    /// USBSTS.SLI set on entry, a port change on resume.
    pub suspend: Option<(u64, u64)>,
    suspended: bool,
    /// Clocks [from, to) during which the host is gone (a USB host that
    /// browns out and restarts: VBUS off, no SOF, no transfers, whatever the
    /// Rytm had queued left where it is); at `to` it comes back as a fresh
    /// host: VBUS, bus reset and a new enumeration. Several windows allowed.
    pub gone: Vec<(u64, u64)>,
    is_gone: bool,
    /// MIDI bytes read from 0x81, for the caller to take.
    pub midi_out: Vec<u8>,
    /// The controller's general-purpose timers 0 and 1 (GPTIMERnLD at 0x80 /
    /// 0x88, GPTIMERnCTRL at 0x84 / 0x8C: bit 31 run, 30 reset, 24 repeat;
    /// 1 us counts; USBSTS bit 24 + n at zero): microseconds left, None when
    /// not loaded. OS 1.73 runs timer 0 at 10 ms (load 9999, 0x81000000;
    /// 0x40004f84) and its USB interrupt calls the callbacks registered with
    /// 0x40004b6e on each expiry - among them 0x40002566, which turns the USB
    /// MIDI sender on (0x413db254 = 1) once MIDI IN NAKs, and off, flushing
    /// endpoint 1 IN, when its oldest request has waited over 400
    /// microframes [measured]. Ticked only with `midi_in`, so the default
    /// mode runs as before.
    gpt: [Option<i64>; 2],
    /// A serial (CDC ACM) device instead of Overbridge: what the factory-test
    /// mode enumerates as (boot flag 0x40: 1935:ffff, data interface 1 with
    /// bulk endpoints 0x02 OUT and 0x82 IN; its console reads lines on 0x02,
    /// docs/RYTM-PORT.md) [measured, OS 1.74]. The host writes `cdc_to_device`
    /// and collects `cdc_from_device`; no audio stream runs.
    pub cdc: bool,
    /// Read the configuration descriptor and follow the firmware on and off
    /// the bus (module comment). Set by `Machine::usb_microframes`.
    pub device_stack: bool,
    /// The host -> device packet of the Overbridge stream and its channels:
    /// the Rytm's 368 and 12 unless the descriptor says otherwise.
    out_block: usize,
    out_channels: usize,
    /// MIDI to the device: USB-MIDI packets for bulk OUT 0x01 (`midi_send`).
    midi_to_device: VecDeque<u8>,
    packer: MidiPacker,
    /// RS seen set since the last bus reset (`device_stack`).
    was_running: bool,
    /// Read MIDI IN only every this many microframes, one dTD a time (a host
    /// that drains slower than the device sends); 0: as before, up to 8 a
    /// microframe. `--usb-host-midi-every N`.
    pub midi_every: u64,
    /// Answer every SysEx the device sends with one of this many bytes, a
    /// peer that acknowledges each message (`--usb-host-ack N`, `device_stack`).
    pub ack: Option<usize>,
    pub cdc_to_device: VecDeque<u8>,
    pub cdc_from_device: Vec<u8>,
    pub stats: UsbStats,
}

#[derive(Default, Debug, Clone)]
pub struct UsbStats {
    pub microframes: u64,
    pub in_transfers: u64,
    pub out_transfers: u64,
    pub in_frames: u64,
    pub out_frames: u64,
    pub bad_blocks: u64,
    /// MIDI IN (0x81) dTDs completed, and microframes the host did not read it.
    pub midi_dtds: u64,
    pub midi_asleep: u64,
    /// OUT frames sent as silence because the Overbridge link had none queued (oblink.rs).
    pub out_underruns: u64,
    /// `device_stack`: USB-MIDI packets into bulk OUT, and bytes read from the
    /// Overbridge vendor interface's interrupt IN 0x82.
    pub midi_packets_out: u64,
    pub vendor_in_bytes: u64,
    /// Host restarts (`gone` windows left) and enumerations completed.
    pub restarts: u64,
    pub enumerations: u64,
}

impl Default for UsbHost {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbHost {
    pub fn new() -> UsbHost {
        UsbHost {
            phase: Phase::Idle,
            uf: 0,
            t: 0,
            queue: VecDeque::new(),
            ctl: None,
            log: std::env::var_os("DTEMU_USB_LOG").is_some(),
            device_desc: Vec::new(),
            config_desc: Vec::new(),
            from_rytm: VecDeque::new(),
            credit: 0,
            out_counter: 0,
            in_budget: 0,
            midi_in: false,
            midi_sleep: None,
            suspend: None,
            suspended: false,
            gone: Vec::new(),
            is_gone: false,
            midi_out: Vec::new(),
            gpt: [None, None],
            cdc: false,
            device_stack: false,
            out_block: OB_BLOCK,
            out_channels: OB_CHANNELS,
            midi_to_device: VecDeque::new(),
            packer: MidiPacker::default(),
            was_running: false,
            midi_every: 0,
            ack: None,
            cdc_to_device: VecDeque::new(),
            cdc_from_device: Vec::new(),
            stats: UsbStats::default(),
        }
    }

    pub fn streaming(&self) -> bool {
        self.phase == Phase::Stream
    }

    /// MIDI to the device, on its bulk OUT endpoint as a USB host sends it
    /// (`device_stack` only; the Rytm's goes through the OS queue stand-in).
    pub fn midi_send(&mut self, bytes: &[u8]) {
        self.packer.feed(bytes, &mut self.midi_to_device);
    }

    fn say(&self, s: &str) {
        if self.log {
            eprintln!("usb host: {s}");
        }
    }

    /// One 125 us microframe. `to_rytm` supplies host -> Rytm frames.
    pub fn microframe(&mut self, bus: &mut Bus, to_rytm: &mut VecDeque<[i32; OB_CHANNELS]>) {
        self.stats.microframes += 1;
        self.uf += 1;
        let now = self.uf;
        let ms = 8; // microframes
        let cmd = bus.peek32(USBCMD);
        let running = cmd & 1 != 0;
        if self.midi_in {
            self.gptimers(bus);
        }
        let t = bus.io.now;
        let gone = self.gone.iter().any(|&(a, b)| (a..b).contains(&t));
        if gone != self.is_gone {
            self.is_gone = gone;
            if gone {
                // VBUS off (B session ends) and the port detached: the device sees the session change.
                let otg = (bus.peek32(OTGSC) & !(OTG_BSV | OTG_AVV | OTG_ASV)) | OTG_BSE | OTG_BSVIS;
                bus.poke32(OTGSC, otg);
                let p = bus.peek32(PORTSC1) & !(1 | (1 << 2) | (1 << 7));
                bus.poke32(PORTSC1, p);
                raise(bus, STS_PCI);
                self.say("host gone (VBUS off)");
            } else {
                // A fresh host: the Idle phase raises VBUS, then the bus reset and a new enumeration.
                self.phase = Phase::Idle;
                self.queue.clear();
                self.ctl = None;
                self.credit = 0;
                self.in_budget = 0;
                self.device_desc.clear();
                self.config_desc.clear();
                self.stats.restarts += 1;
                self.say("host back");
            }
        }
        if gone {
            self.stats.midi_asleep += 1;
            return;
        }
        let asleep = self.suspend.is_some_and(|(a, b)| (a..b).contains(&t));
        if asleep != self.suspended {
            self.suspended = asleep;
            let p = bus.peek32(PORTSC1);
            if asleep {
                bus.poke32(PORTSC1, p | (1 << 7));
                raise(bus, 1 << 8);
                self.say("suspend");
            } else {
                bus.poke32(PORTSC1, p & !(1 << 7));
                raise(bus, STS_PCI);
                self.say("resume");
            }
        }
        if asleep {
            self.stats.midi_asleep += 1;
            return;
        }
        if self.device_stack {
            // RS off is the firmware's pull-up gone (a USB CONFIG change, 0x4000601a on the Digitakt): a real host
            // sees a disconnect, then a new device when RS comes back, and enumerates it again.
            if running {
                self.was_running = true;
            } else if self.was_running {
                self.was_running = false;
                self.phase = Phase::Vbus;
                self.t = now;
                self.queue.clear();
                self.ctl = None;
                self.credit = 0;
                self.in_budget = 0;
                self.device_desc.clear();
                self.config_desc.clear();
                self.say("device left the bus");
            }
        }
        match self.phase {
            Phase::Idle => {
                // Cable in: VBUS valid (B session) and its interrupt.
                let otg = bus.peek32(OTGSC) | OTG_BSV | OTG_AVV | OTG_ASV | OTG_BSE | OTG_BSVIS;
                bus.poke32(OTGSC, otg);
                bus.io.usb_irq = line(bus);
                bus.io.update_irq();
                self.phase = Phase::Vbus;
                self.t = now;
                self.say("VBUS on");
            }
            Phase::Vbus => {
                if running && now > self.t + 50 * ms {
                    let p = bus.peek32(PORTSC1) | (1 << 8);
                    bus.poke32(PORTSC1, p);
                    raise(bus, STS_URI);
                    self.phase = Phase::Reset;
                    self.t = now;
                    self.say("bus reset");
                }
            }
            Phase::Reset => {
                if now > self.t + 10 * ms && bus.peek32(USBSTS) & STS_URI == 0 {
                    // Reset done: connected, enabled, high speed.
                    let p = (bus.peek32(PORTSC1) & !(1 << 8) & !(3 << 26)) | 1 | (1 << 2) | (2 << 26);
                    bus.poke32(PORTSC1, p);
                    raise(bus, STS_PCI);
                    self.phase = Phase::Enumerate;
                    self.t = now;
                    for s in [
                        [0x80, 6, 0, 1, 0, 0, 64, 0],     // GET_DESCRIPTOR device (64)
                        [0x00, 5, 7, 0, 0, 0, 0, 0],      // SET_ADDRESS 7
                        [0x80, 6, 0, 1, 0, 0, 18, 0],     // GET_DESCRIPTOR device
                        [0x80, 6, 0, 2, 0, 0, 9, 0],      // GET_DESCRIPTOR configuration (9)
                    ] {
                        self.queue.push_back(s);
                    }
                    self.say("port enabled, high speed");
                }
            }
            Phase::Enumerate | Phase::Stream => {
                if !running {
                    return;
                }
                // Start of (micro)frame: FRINDEX counts microframes.
                let fr = (bus.peek32(FRINDEX) + 1) & 0x3FFF;
                bus.poke32(FRINDEX, fr);
                raise(bus, STS_SRI);
                self.control(bus, now);
                if self.phase == Phase::Stream && self.cdc {
                    self.serial(bus);
                } else if self.phase == Phase::Stream {
                    self.stream(bus, to_rytm);
                    if self.midi_in {
                        self.midi(bus);
                    }
                    if self.device_stack {
                        self.midi_out(bus);
                        let (b, _) = self.run_dtds(bus, 2, true, &[]);
                        self.stats.vendor_in_bytes += b.len() as u64;
                    }
                }
            }
        }
    }

    fn qh(bus: &Bus, ep: u32, dir_in: bool) -> u32 {
        (bus.peek32(EPLISTADDR) & !0x7FF) + 64 * (2 * ep + dir_in as u32)
    }

    /// The next dTD the controller would run: the overlay's next pointer, or,
    /// when that terminates, the current dTD's own next pointer read again from
    /// memory (how a dTD appended while the endpoint stays primed is found).
    fn next_dtd(bus: &Bus, qh: u32) -> u32 {
        let next = bus.peek32(qh + 8);
        if next & 1 == 0 {
            return next;
        }
        let cur = bus.peek32(qh + 4) & !0x1F;
        if cur == 0 {
            return 1;
        }
        bus.peek32(cur)
    }

    /// Run the active dTDs of one endpoint direction. For IN (device -> host)
    /// returns the bytes sent; for OUT, `data` is what the host sends (as much as
    /// fits). -> (bytes, any dTD completed)
    fn run_dtds(&mut self, bus: &mut Bus, ep: u32, dir_in: bool, data: &[u8]) -> (Vec<u8>, bool) {
        let bit = if dir_in { 1u32 << (16 + ep) } else { 1u32 << ep };
        if bus.peek32(EPSR) & bit == 0 {
            return (Vec::new(), false);
        }
        let qh = Self::qh(bus, ep, dir_in);
        let mut out = Vec::new();
        let mut sent = 0usize;
        let mut done = false;
        let mut next = Self::next_dtd(bus, qh);
        let mut guard = 0;
        while next & 1 == 0 && guard < 64 {
            guard += 1;
            let td = next & !0x1F;
            let tok = bus.peek32(td + 4);
            if tok & 0x80 == 0 {
                // An inactive dTD ends the list: the controller un-primes the
                // endpoint, and the firmware's next add primes it again (its
                // dTDs are a reused ring, so following `next` would go around).
                next = 1;
                break;
            }
            let total = ((tok >> 16) & 0x7FFF) as usize;
            let p0 = bus.peek32(td + 8);
            let addr = |k: usize| -> u32 {
                let first = 4096 - (p0 as usize & 0xFFF);
                if k < first {
                    p0 + k as u32
                } else {
                    let page = 1 + (k - first) / 4096;
                    (bus.peek32(td + 8 + 4 * page as u32) & !0xFFF) + ((k - first) % 4096) as u32
                }
            };
            let n;
            if dir_in {
                n = total;
                let addrs: Vec<u32> = (0..n).map(addr).collect();
                for a in addrs {
                    out.push(bus.peek8(a));
                }
            } else {
                n = total.min(data.len() - sent);
                let addrs: Vec<u32> = (0..n).map(addr).collect();
                for (k, a) in addrs.into_iter().enumerate() {
                    bus.poke8(a, data[sent + k]);
                }
                sent += n;
            }
            let left = (total - n) as u32;
            bus.poke32(td + 4, (tok & !0x7FFF_0000 & !0xFF) | (left << 16));
            // The (1 ms) frame it completed in, in buffer pointer 1's low 11 bits.
            // The firmware reads it (0x400052b2); its stream sync waits for a
            // completion in the frame after the first one and works back to the
            // microframe by counting packets (0x4000590a, OUT; 0x4000580c, IN)
            // to find that microframe's SOF timestamp (0x42f67a4c[uframe & 31]).
            let p1 = bus.peek32(td + 12);
            bus.poke32(td + 12, (p1 & !0x7FF) | ((bus.peek32(FRINDEX) >> 3) & 0x7FF));
            bus.poke32(qh + 4, td);
            let nx = bus.peek32(td);
            bus.poke32(qh + 8, nx);
            bus.poke32(qh + 12, bus.peek32(td + 4));
            if tok & 0x8000 != 0 {
                let c = bus.peek32(EPCOMPLETE) | bit;
                bus.poke32(EPCOMPLETE, c);
                done = true;
            }
            next = nx;
            // One dTD per call: the stream moves one packet per microframe (the
            // caller paces multi-packet dTDs), and the firmware's stream sync
            // depends on completions spreading over microframes as on a real bus.
            if ep != 0 {
                break;
            }
        }
        // The controller fetches the next dTD at once: none, or one not yet
        // active, un-primes the endpoint (the firmware then primes again when it
        // adds one: 0x40003dfc).
        if next & 1 != 0 || bus.peek32((next & !0x1F) + 4) & 0x80 == 0 {
            let s = bus.peek32(EPSR) & !bit;
            bus.poke32(EPSR, s);
        }
        if done {
            raise(bus, STS_UI);
        }
        (out, done)
    }

    fn control(&mut self, bus: &mut Bus, now: u64) {
        if self.ctl.is_none() {
            let Some(setup) = self.queue.pop_front() else {
                if self.phase == Phase::Enumerate {
                    self.phase = Phase::Stream;
                    self.stats.enumerations += 1;
                    self.say("configured: streaming");
                }
                return;
            };
            // Post the setup packet.
            // The setup buffer holds the packet as two 32-bit words in USB (little-
            // endian) order: the firmware reads each and byte-reverses it
            // (0x400051ec) [measured].
            let qh = Self::qh(bus, 0, false);
            bus.poke32(qh + 0x28, u32::from_le_bytes([setup[0], setup[1], setup[2], setup[3]]));
            bus.poke32(qh + 0x2C, u32::from_le_bytes([setup[4], setup[5], setup[6], setup[7]]));
            let s = bus.peek32(EPSETUPSR) | 1;
            bus.poke32(EPSETUPSR, s);
            raise(bus, STS_UI);
            self.say(&format!("setup {:02x?}", setup));
            self.ctl = Some(Ctl { setup, out: Vec::new(), stage: Stage::Data, got: Vec::new(), t0: now });
            return;
        }
        let mut c = self.ctl.take().unwrap();
        // A stalled endpoint 0 (EPCR0 TXS/RXS) refuses the request.
        let epcr0 = bus.peek32(R + 0x1C0);
        if epcr0 & 0x0001_0001 != 0 {
            self.say(&format!("setup {:02x?}: stalled", c.setup));
            bus.poke32(R + 0x1C0, epcr0 & !0x0001_0001);
            return;
        }
        let dev_to_host = c.setup[0] & 0x80 != 0;
        let wlen = u16::from_le_bytes([c.setup[6], c.setup[7]]) as usize;
        match c.stage {
            Stage::Data => {
                if wlen == 0 {
                    c.stage = Stage::Status;
                } else if dev_to_host {
                    let (b, done) = self.run_dtds(bus, 0, true, &[]);
                    c.got.extend(b);
                    if done || c.got.len() >= wlen {
                        c.got.truncate(wlen);
                        c.stage = Stage::Status;
                    }
                } else {
                    let data = c.out.clone();
                    let (_, done) = self.run_dtds(bus, 0, false, &data);
                    if done {
                        c.stage = Stage::Status;
                    }
                }
                if !matches!(c.stage, Stage::Status) {
                    if now > c.t0 + 16_000 {
                        self.say(&format!("setup {:02x?}: no data stage, dropped", c.setup));
                        return;
                    }
                    self.ctl = Some(c);
                    return;
                }
            }
            Stage::Status => {}
        }
        // Status stage: the opposite direction, zero length.
        let (_, done) = if dev_to_host && wlen > 0 {
            self.run_dtds(bus, 0, false, &[])
        } else {
            self.run_dtds(bus, 0, true, &[])
        };
        if !done {
            if now > c.t0 + 16_000 {
                self.say(&format!("setup {:02x?}: no status stage, dropped", c.setup));
                return;
            }
            self.ctl = Some(c);
            return;
        }
        self.finished(&c);
    }

    fn finished(&mut self, c: &Ctl) {
        self.say(&format!("setup {:02x?} done, {} bytes in", c.setup, c.got.len()));
        let (req, dtype) = (c.setup[1], c.setup[3]);
        if c.setup[0] == 0x80 && req == 6 && dtype == 1 {
            self.device_desc = c.got.clone();
        }
        if c.setup[0] == 0x80 && req == 6 && dtype == 2 {
            if c.got.len() >= 4 && c.got.len() == 9 {
                let total = u16::from_le_bytes([c.got[2], c.got[3]]);
                let t = total.to_le_bytes();
                self.queue.push_front([0x80, 6, 0, 2, 0, 0, t[0], t[1]]);
            } else {
                self.config_desc = c.got.clone();
                if self.device_stack {
                    let info = parse_config(&c.got);
                    if info.ob_out_packet > 32 {
                        self.out_block = info.ob_out_packet;
                        self.out_channels = (info.ob_out_packet - 32) / (4 * OB_FRAMES_PER_BLOCK);
                    }
                    self.say(&format!("configuration: {info:?}"));
                    self.queue.push_back([0x00, 9, 1, 0, 0, 0, 0, 0]);
                    for (i, a) in info.alts {
                        self.queue.push_back([0x01, 11, a, 0, i, 0, 0, 0]);
                    }
                    return;
                }
                for s in [
                    [0x00, 9, 1, 0, 0, 0, 0, 0],  // SET_CONFIGURATION 1
                    [0x01, 11, 3, 0, 1, 0, 0, 0], // SET_INTERFACE 1, alt 3 (audio in)
                    [0x01, 11, 3, 0, 2, 0, 0, 0], // SET_INTERFACE 2, alt 3 (audio out)
                ] {
                    self.queue.push_back(s);
                }
            }
        }
    }

    /// One microframe (125 us) of the general-purpose timers (`gpt`).
    fn gptimers(&mut self, bus: &mut Bus) {
        for k in 0..2u32 {
            let (ld, ctrl) = (R + 0x80 + 8 * k, R + 0x84 + 8 * k);
            let c = bus.peek32(ctrl);
            if c & (1 << 30) != 0 {
                self.gpt[k as usize] = None;
                bus.poke32(ctrl, c & !(1 << 30));
            }
            if c & (1 << 31) == 0 {
                continue;
            }
            let left = self.gpt[k as usize].unwrap_or(bus.peek32(ld) as i64 & 0xFF_FFFF) - 125;
            if left < 0 {
                raise(bus, 1 << (24 + k));
                if c & (1 << 24) != 0 {
                    self.gpt[k as usize] = Some(left + 1 + (bus.peek32(ld) as i64 & 0xFF_FFFF));
                } else {
                    self.gpt[k as usize] = None;
                    bus.poke32(ctrl, c & !(1 << 31));
                }
            } else {
                self.gpt[k as usize] = Some(left);
            }
        }
    }

    /// MIDI IN: up to 8 of the firmware's queued dTDs on endpoint 1 IN a
    /// microframe (bulk; a real host's polling is about that eager), their
    /// USB-MIDI event packets decoded into `midi_out`. Each dTD carries IOC,
    /// so its completion interrupt hands the request back to the sender's pool
    /// (0x40003b24 -> 0x40003938, OS 1.73) [measured]. While asleep nothing
    /// is read and the requests stay queued.
    fn midi(&mut self, bus: &mut Bus) {
        let now = bus.io.now;
        if self.midi_sleep.is_some_and(|(a, b)| (a..b).contains(&now)) {
            self.stats.midi_asleep += 1;
            return;
        }
        // Polling an IN endpoint with nothing primed NAKs: EPNAK's bit 17. The
        // firmware takes that as "a host reads MIDI" (0x40002566 sets
        // 0x413db254 = 1, the sender's go) [measured, OS 1.73].
        if bus.peek32(EPSR) & (1 << 17) == 0 {
            let v = bus.peek32(EPNAK) | (1 << 17);
            bus.poke32(EPNAK, v);
        }
        let reads = match self.midi_every {
            0 => 8,
            n if self.uf % n == 0 => 1,
            _ => 0,
        };
        for _ in 0..reads {
            let (b, done) = self.run_dtds(bus, 1, true, &[]);
            if !done && b.is_empty() {
                break;
            }
            self.stats.midi_dtds += 1;
            for p in b.chunks_exact(4) {
                // CIN -> bytes in the packet (USB MIDI 1.0, table 4-1).
                let n = match p[0] & 0x0F {
                    0x5 | 0xF => 1,
                    0x2 | 0x6 | 0xC | 0xD => 2,
                    0x3 | 0x4 | 0x7 | 0x8 | 0x9 | 0xA | 0xB | 0xE => 3,
                    _ => 0,
                };
                self.midi_out.extend_from_slice(&p[1..1 + n]);
            }
        }
    }

    /// The serial device's data endpoints: read 0x82, and send what is waiting
    /// on 0x02, one high-speed bulk packet (512 bytes) per microframe each way.
    fn serial(&mut self, bus: &mut Bus) {
        let (b, _) = self.run_dtds(bus, 2, true, &[]);
        self.cdc_from_device.extend_from_slice(&b);
        if self.cdc_to_device.is_empty() || bus.peek32(EPSR) & (1 << 2) == 0 {
            return;
        }
        let td = Self::next_dtd(bus, Self::qh(bus, 2, false));
        if td & 1 != 0 {
            return;
        }
        let tok = bus.peek32((td & !0x1F) + 4);
        if tok & 0x80 == 0 {
            return;
        }
        let total = ((tok >> 16) & 0x7FFF) as usize;
        let n = total.min(512).min(self.cdc_to_device.len());
        let data: Vec<u8> = self.cdc_to_device.drain(..n).collect();
        self.run_dtds(bus, 2, false, &data);
    }

    /// Bulk OUT 0x01: the waiting USB-MIDI packets, one high-speed packet (512
    /// bytes) a microframe, into the dTD the firmware keeps primed.
    fn midi_out(&mut self, bus: &mut Bus) {
        if self.midi_to_device.is_empty() || bus.peek32(EPSR) & 2 == 0 {
            return;
        }
        let td = Self::next_dtd(bus, Self::qh(bus, 1, false));
        if td & 1 != 0 {
            return;
        }
        let tok = bus.peek32((td & !0x1F) + 4);
        if tok & 0x80 == 0 {
            return;
        }
        let total = ((tok >> 16) & 0x7FFF) as usize;
        let n = total.min(512).min(self.midi_to_device.len()) & !3;
        let data: Vec<u8> = self.midi_to_device.drain(..n).collect();
        self.run_dtds(bus, 1, false, &data);
        self.stats.midi_packets_out += n as u64 / 4;
    }

    /// The audio stream: take what the Rytm sends on 0x83, answer on 0x03 with as
    /// many frames.
    fn stream(&mut self, bus: &mut Bus, to_rytm: &mut VecDeque<[i32; OB_CHANNELS]>) {
        // One packet per microframe: a dTD of n packets completes in its n-th.
        let bit = 1u32 << 19;
        if bus.peek32(EPSR) & bit != 0 {
            let qh = Self::qh(bus, 3, true);
            let td = Self::next_dtd(bus, qh);
            if td & 1 == 0 {
                let total = ((bus.peek32((td & !0x1F) + 4) >> 16) & 0x7FFF) as usize;
                self.in_budget += OB_BLOCK;
                if total > self.in_budget {
                    return self.stream_out(bus, to_rytm);
                }
                self.in_budget = 0;
            }
        }
        let (b, done) = self.run_dtds(bus, 3, true, &[]);
        if done || !b.is_empty() {
            self.stats.in_transfers += 1;
            if self.log && self.stats.in_transfers <= 4 {
                eprintln!("usb host: IN {} bytes: {:02x?}", b.len(), &b[..b.len().min(80)]);
            }
            for blk in b.chunks(OB_BLOCK) {
                // Rytm -> host blocks start 0x0700 (host -> Rytm: 0x07ff) [measured].
                if blk.len() < OB_BLOCK || blk[0] != 0x07 {
                    self.stats.bad_blocks += 1;
                    continue;
                }
                for f in 0..OB_FRAMES_PER_BLOCK {
                    let mut fr = [0i32; OB_CHANNELS];
                    for (c, x) in fr.iter_mut().enumerate() {
                        let o = 32 + 4 * (f * OB_CHANNELS + c);
                        *x = i32::from_be_bytes([blk[o], blk[o + 1], blk[o + 2], blk[o + 3]]);
                    }
                    self.from_rytm.push_back(fr);
                    self.stats.in_frames += 1;
                    self.credit += 1;
                }
            }
            while self.from_rytm.len() > 48_000 {
                self.from_rytm.pop_front();
            }
        }
        self.stream_out(bus, to_rytm);
    }

    /// OUT: one transfer of whole blocks, when the Rytm has a buffer primed and
    /// we owe it frames.
    fn stream_out(&mut self, bus: &mut Bus, to_rytm: &mut VecDeque<[i32; OB_CHANNELS]>) {
        let bit = 1u32 << 3;
        if bus.peek32(EPSR) & bit == 0 || self.credit < OB_FRAMES_PER_BLOCK {
            return;
        }
        let qh = Self::qh(bus, 3, false);
        let td = Self::next_dtd(bus, qh);
        if td & 1 != 0 {
            let s = bus.peek32(EPSR) & !bit;
            bus.poke32(EPSR, s);
            return;
        }
        let tok = bus.peek32((td & !0x1F) + 4);
        if tok & 0x80 == 0 {
            let s = bus.peek32(EPSR) & !bit;
            bus.poke32(EPSR, s);
            return;
        }
        let total = ((tok >> 16) & 0x7FFF) as usize;
        // One packet (one block) per microframe.
        let blocks = (total / self.out_block).min(self.credit / OB_FRAMES_PER_BLOCK).clamp(1, 1);
        if self.log && self.stats.out_transfers < 4 {
            eprintln!("usb host: OUT dTD {total} bytes, sending {blocks} blocks");
        }
        let mut data = vec![0u8; blocks * self.out_block];
        let out_channels = self.out_channels;
        for blk in data.chunks_mut(self.out_block) {
            blk[0] = 0x07;
            blk[1] = 0xFF;
            blk[2..4].copy_from_slice(&self.out_counter.to_be_bytes());
            self.out_counter = self.out_counter.wrapping_add(OB_FRAMES_PER_BLOCK as u16);
            for f in 0..OB_FRAMES_PER_BLOCK {
                let fr = match to_rytm.pop_front() {
                    Some(f) => f,
                    None => {
                        self.stats.out_underruns += 1;
                        [0; OB_CHANNELS]
                    }
                };
                for (c, x) in fr.iter().take(out_channels).enumerate() {
                    let o = 32 + 4 * (f * out_channels + c);
                    blk[o..o + 4].copy_from_slice(&x.to_be_bytes());
                }
            }
        }
        self.credit -= blocks * OB_FRAMES_PER_BLOCK;
        // The Rytm's OUT dTDs carry no IOC: its audio code polls them.
        self.run_dtds(bus, 3, false, &data);
        self.stats.out_transfers += 1;
        self.stats.out_frames += (blocks * OB_FRAMES_PER_BLOCK) as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packets(chunks: &[&[u8]]) -> Vec<u8> {
        let (mut p, mut out) = (MidiPacker::default(), VecDeque::new());
        for c in chunks {
            p.feed(c, &mut out);
        }
        out.into_iter().collect()
    }

    #[test]
    fn midi_packets_channel_running_realtime() {
        assert_eq!(packets(&[&[0x90, 60, 100, 62, 101]]), [9, 0x90, 60, 100, 9, 0x90, 62, 101]);
        assert_eq!(packets(&[&[0xC3, 5, 6]]), [0xC, 0xC3, 5, 0, 0xC, 0xC3, 6, 0]);
        // A clock inside a note, the note split across calls.
        assert_eq!(packets(&[&[0x80, 60], &[0xF8, 0]]), [0xF, 0xF8, 0, 0, 8, 0x80, 60, 0]);
        assert_eq!(packets(&[&[0xF2, 1, 2, 0xF6]]), [3, 0xF2, 1, 2, 5, 0xF6, 0, 0]);
    }

    #[test]
    fn midi_packets_sysex_each_end() {
        assert_eq!(packets(&[&[0xF0, 0xF7]]), [6, 0xF0, 0xF7, 0]);
        assert_eq!(packets(&[&[0xF0, 1, 0xF7]]), [7, 0xF0, 1, 0xF7]);
        assert_eq!(packets(&[&[0xF0, 1, 2, 0xF7]]), [4, 0xF0, 1, 2, 5, 0xF7, 0, 0]);
        assert_eq!(packets(&[&[0xF0, 1], &[2, 3, 0xF7]]), [4, 0xF0, 1, 2, 6, 3, 0xF7, 0]);
    }

    /// The Digitakt's OVERBRIDGE configuration, its shape as the firmware
    /// gives it: IN alt 3 packs 12 channels, OUT alt 3 two, MIDI is not touched.
    #[test]
    fn config_takes_the_largest_vendor_alternate() {
        let ifd = |n, a, class| vec![9, 4, n, a, 1, class, 0, 0, 0];
        let ep = |e, attr, mps: u16| vec![7, 5, e, attr, mps as u8, (mps >> 8) as u8, 1];
        let mut d = vec![9, 2, 0, 0, 6, 1, 0, 0xC0, 0];
        for (a, mps) in [(1, 32), (2, 88), (3, 368)] {
            d.extend(ifd(1, a, 0xFF));
            d.extend(ep(0x83, 3, mps));
        }
        for (a, mps) in [(1, 32), (2, 88), (3, 88)] {
            d.extend(ifd(2, a, 0xFF));
            d.extend(ep(0x03, 3, mps));
        }
        d.extend(ifd(3, 0, 0xFF));
        d.extend(ep(0x02, 2, 512));
        d.extend(ep(0x82, 3, 64));
        d.extend(ifd(5, 0, 1));
        d.extend(ep(0x01, 2, 512));
        let i = parse_config(&d);
        assert_eq!(i.alts, [(1, 3), (2, 3)]);
        assert_eq!((i.ob_in_packet, i.ob_out_packet), (368, 88));
    }

    #[test]
    fn config_without_vendor_interfaces_asks_for_nothing() {
        let d = [9, 4, 1, 0, 2, 1, 3, 0, 0, 7, 5, 0x01, 2, 0, 2, 0, 7, 5, 0x81, 2, 0, 2, 0];
        assert_eq!(parse_config(&d), ConfigInfo::default());
    }
}
