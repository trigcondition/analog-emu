//! The front-panel microcontroller at the far end of UART8.
//!
//! A separate MCU scans the keys and encoders and drives the OLED and the
//! key LEDs; the ColdFire talks to it over UART8. This models it from the
//! stream, as the MCU sees it (reference project, emu/panelleds.py, read
//! from both the ColdFire's builders and the MCU's own parser):
//!
//! ```text
//!   0x            1  ignored
//!   1p cc d0..d7 10  OLED tile: page p, columns cc..cc+7
//!   2g ss         2  LED selectors for LEDs 4g..4g+3, 2 bits each
//!   Bs id vv      3  slot s (0..3) of LED id := palette index vv
//!   B4 id v0..v3  6  all four slots
//!   B5 i r g b    5  palette entry i := RGB, 0..31 each
//!   B6 ..         3  accepted, unused
//!   B7 x          2  OLED contrast
//!   B8            1  end of OLED frame
//!   anything else 2
//! ```
//!
//! Queries the MCU answers:
//!
//! ```text
//!   60 01        every button group: [0x20 | group, mask] x 6
//!   70 00/71 00  the UI card: 70, type 4 (Digitakt), UI fw 1.2.0, tested
//!   74 00        the serial number: 70 then 9 bytes
//! ```
//!
//! and what it sends on its own: `[0x20 | channel, mask]` for a button
//! group's state (a bitmask, the firmware derives edges), `[0x30 | encoder,
//! delta]` for an encoder turn (signed).

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use serde_big_array::BigArray;

pub const GROUPS: u8 = 6;
pub const CARD: [u8; 4] = [0x04, 0x02, 0x00, 0x01];
/// The Analog Rytm MKII's UI card: type 7, the two UI firmware versions the OS
/// carries (5 and 5, at 0x402c8758 / 0x402cb480 in 1.73), tested [measured: with
/// type 7 and other versions the OS starts re-flashing the panel MCU over its
/// bootloader protocol (0x4013fe9c), which the model does not speak, and boot
/// stalls about 30 s on its timeouts; with type 4 it says WRONG UI CARD and
/// sends no LED updates after boot].
pub const CARD_RYTM: [u8; 4] = [0x07, 0x05, 0x05, 0x01];
/// The Analog Four MKII's UI card: type 6 (the OS says "WRONG UI CARD" for any
/// other, 0x401195f4) [measured]. The OS carries its own panel firmware, version
/// 10 (*0x4024d9a0; the image follows at 0x4024d99c), and re-flashes the panel
/// over its polled bootloader protocol (0x40118518: `80 42`, then 5-byte frames)
/// when the card's first version byte is lower [measured]; the second version
/// byte is copied from the first [inferred].
pub const CARD_A4: [u8; 4] = [0x06, 0x0A, 0x0A, 0x01];

/// The Syntakt's UI card: type 0x0B (`0x40080434`; "WRONG UI CARD" otherwise)
/// [measured]; versions as the Digitakt's [inferred].
pub const CARD_SYNTAKT: [u8; 4] = [0x0B, 0x02, 0x00, 0x01];

fn default_card() -> [u8; 4] {
    CARD
}
pub const SERIAL: &[u8; 12] = b"EMULATED0000";
pub const LEDS: usize = 128;
/// LEDs a view collects.
pub const ALL_LEDS: usize = LEDS;
pub const LED_GROUPS: usize = 32;
pub const PALETTE: usize = 127;

/// Control code -> (wire channel, bit), measured on the device firmware
/// (reference devices/digitakt.toml `[panel.exceptions]`).
pub const WIRE: [(u8, u8, u8); 48] = [
    (24, 0, 0), (25, 0, 1), (26, 0, 2), (27, 0, 3), (28, 0, 4), (29, 0, 5), (30, 0, 6), (31, 0, 7),
    (32, 1, 0), (33, 1, 1), (34, 1, 2), (35, 1, 3), (36, 1, 4), (37, 1, 5), (38, 1, 6), (39, 1, 7),
    (2, 2, 0), (1, 2, 1), (19, 2, 2), (20, 2, 3), (0, 2, 4), (21, 2, 5), (22, 2, 6), (23, 2, 7),
    (6, 3, 0), (7, 3, 1), (8, 3, 2), (13, 3, 3), (16, 3, 4), (15, 3, 5), (17, 3, 6), (18, 3, 7),
    (4, 4, 0), (3, 4, 1), (5, 4, 2), (9, 4, 3), (10, 4, 4), (11, 4, 5), (12, 4, 6), (14, 4, 7),
    (46, 5, 0), (45, 5, 1), (44, 5, 2), (41, 5, 3), (40, 5, 4), (43, 5, 5), (42, 5, 6), (47, 5, 7),
];

/// Measured key names (reference devices/digitakt.toml `[panel.labels]`),
/// by control code.
pub fn key_name(code: u8) -> Option<&'static str> {
    if code >= RYTM_CODE_BASE {
        return device_keys().get((code - RYTM_CODE_BASE) as usize).map(|k| k.0);
    }
    Some(match code {
        1 => "FUNC",
        2 => "TRK",
        3 => "PTN",
        4 => "BANK",
        5 => "SONG",
        6 => "GLOBAL",
        7 => "SAMPLE",
        8 => "TEMPO",
        9 => "RECORD",
        10 => "PLAY",
        11 => "STOP",
        12 => "YES",
        13 => "NO",
        14 => "UP",
        15 => "DOWN",
        16 => "LEFT",
        17 => "RIGHT",
        18 => "PAGE",
        19 => "TRIG",
        20 => "SRC",
        21 => "FLTR",
        22 => "AMP",
        23 => "LFO",
        24..=39 => ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16"][(code - 24) as usize],
        40..=47 => ["A", "B", "C", "D", "E", "F", "G", "H"][(code - 40) as usize],
        _ => return None,
    })
}

/// The LED that lights key `code`, if it has one (reference `[panel.leds]`).
pub fn led_for_key(code: u8) -> Option<usize> {
    if code >= RYTM_CODE_BASE {
        if syntakt_panel() {
            let name = key_name(code)?;
            return SYNTAKT_LEDS.iter().find(|k| k.0 == name).map(|k| k.1);
        }
        if rytmmk1_panel() {
            let name = key_name(code)?;
            return RYTMMK1_LEDS.iter().find(|k| k.0 == name).map(|k| k.1);
        }
        if a4mk1_panel() {
            let name = key_name(code)?;
            return A4MK1_LEDS.iter().chain(A4MK1_MORE_LEDS.iter()).find(|k| k.0 == name).map(|k| k.1);
        }
        if a4_panel() {
            let name = key_name(code)?;
            return A4_LEDS.iter().chain(A4_ROW_LEDS.iter()).find(|k| k.0 == name).map(|k| k.1);
        }
        return key_name(code).and_then(rytm_led);
    }
    const MAP: [(u8, u8); 39] = [
        (0, 24), (1, 25), (2, 26), (3, 27), (4, 28), (5, 29), (6, 30), (7, 31), (8, 32), (9, 33), (10, 34), (11, 35),
        (12, 36), (13, 37), (14, 38), (15, 39), (16, 4), (17, 3), (18, 19), (19, 20), (20, 21), (21, 22), (22, 23),
        (23, 18), (24, 11), (25, 8), (26, 12), (27, 13), (28, 14), (29, 16), (30, 15), (31, 17), (32, 1), (33, 5),
        (34, 6), (35, 7), (36, 9), (37, 10), (38, 2),
    ];
    MAP.iter().find(|(_, k)| *k == code).map(|(l, _)| *l as usize)
}

/// The LED under TRK: lit red in chromatic mode (FUNC + TRK) [measured, OS 1.53].
pub const DIGITAKT_TRK_LED: usize = 39;
/// The four pattern-page LEDs, page 1 first.
pub const PAGE_LEDS: [usize; 4] = [43, 42, 41, 40];

#[derive(Clone, Serialize, Deserialize)]
pub struct PanelMcu {
    /// Bytes of a message still arriving.
    partial: Vec<u8>,
    /// Keys held, per wire channel.
    pub held: [u8; 16],
    /// Button groups this panel has: what the `60 01` query reports (Digitakt
    /// 6, Analog Rytm MKII 11).
    pub groups: u8,
    /// The UI card's identity, answered to `70 00`/`71 00`: type, UI firmware
    /// version, tested flag. Digitakt `CARD` (type 4); the Rytm OS wants type 7,
    /// or its self-test says "WRONG UI CARD" (0x40139d5a, 0x4013bbde).
    #[serde(default = "default_card")]
    pub card: [u8; 4],
    /// The A4's `0xAg vv` rows: the slot selectors of LEDs 64 + 4g .. 64 + 4g + 3
    /// (the A4's counterpart of the Rytm's `0x3g`) [measured, docs/A4-PORT.md].
    #[serde(default)]
    pub led_rows: [Option<u8>; 16],
    /// The OLED as the MCU holds it: byte = page + 8 * column, bit n = row
    /// 8 * (7 - page) + n (page 0 is the bottom band).
    #[serde(with = "BigArray")]
    pub oled: [u8; 1024],
    /// Frames completed (B8 seen).
    pub oled_frames: u64,
    pub contrast: u8,
    /// Per LED, per slot: a palette index (0xFF = undefined).
    pub slots: Vec<[u8; 4]>,
    /// Per selector group: the last selector byte, once one was sent.
    pub selectors: Vec<Option<u8>>,
    /// RGB, 0..31 each; None until defined.
    pub palette: Vec<Option<(u8, u8, u8)>>,
    pub queries: u64,
    pub unknown: u64,
    /// The last messages the model does not understand, for diagnosis.
    #[serde(skip)]
    pub unknown_log: VecDeque<Vec<u8>>,
}

impl Default for PanelMcu {
    fn default() -> Self {
        PanelMcu {
            partial: Vec::new(),
            held: [0; 16],
            groups: GROUPS,
            card: CARD,
            oled: [0; 1024],
            oled_frames: 0,
            contrast: 0,
            slots: vec![[0xFF; 4]; 256],
            selectors: vec![None; LED_GROUPS],
            palette: vec![None; PALETTE + 1],
            queries: 0,
            unknown: 0,
            unknown_log: VecDeque::new(),
            led_rows: [None; 16],
        }
    }
}

fn msg_len(h: u8) -> usize {
    match h >> 4 {
        0x0 => 1,
        0x1 => 10,
        0xB => match h {
            0xB0..=0xB3 | 0xB6 => 3,
            0xB4 => 6,
            0xB5 => 5,
            0xB7 => 2,
            0xB8 => 1,
            _ => 2,
        },
        _ => 2,
    }
}

impl PanelMcu {
    /// A byte from the ColdFire. Replies are appended to `reply`.
    pub fn from_host(&mut self, b: u8, reply: &mut Vec<u8>) {
        self.partial.push(b);
        let n = msg_len(self.partial[0]);
        if self.partial.len() < n {
            return;
        }
        let m = std::mem::take(&mut self.partial);
        self.message(&m, reply);
    }

    fn message(&mut self, m: &[u8], reply: &mut Vec<u8>) {
        let h = m[0];
        match h >> 4 {
            0x0 => {}
            0x1 => {
                let page = (h & 7) as usize;
                let col = m[1] as usize;
                for (k, &d) in m[2..10].iter().enumerate() {
                    let c = col + k;
                    if c < 128 {
                        self.oled[page + 8 * c] = d;
                    }
                }
            }
            // LED selectors: 0x2g for groups 0..15, 0x3g for groups 16..31 (the
            // Rytm's second bank, LEDs 64..127; the Digitakt never sends it).
            0x2 | 0x3 => {
                let g = (h & 0xF) as usize + if h >> 4 == 0x3 { 16 } else { 0 };
                if g < self.selectors.len() {
                    self.selectors[g] = Some(m[1]);
                }
            }
            0x6 if m[1] == 0x01 && h == 0x60 => {
                self.queries += 1;
                for g in 0..self.groups {
                    reply.push(0x20 | g);
                    reply.push(self.held[g as usize]);
                }
            }
            0x7 if m[1] == 0x00 && (h == 0x70 || h == 0x71) => {
                self.queries += 1;
                reply.push(0x70);
                reply.extend_from_slice(&self.card);
            }
            0x7 if m[1] == 0x00 && h == 0x74 => {
                self.queries += 1;
                reply.push(0x70);
                reply.extend_from_slice(&SERIAL[..9]);
            }
            0xA => self.led_rows[(h & 0xF) as usize] = Some(m[1]),
            0xB => match h {
                0xB0..=0xB3 => self.slots[m[1] as usize][(h & 3) as usize] = m[2],
                0xB4 => self.slots[m[1] as usize].copy_from_slice(&m[2..6]),
                0xB5 => {
                    let i = m[1] as usize;
                    if i < self.palette.len() {
                        self.palette[i] = Some((m[2], m[3], m[4]));
                    }
                }
                0xB7 => self.contrast = m[1],
                0xB8 => self.oled_frames += 1,
                _ => {}
            },
            _ => {
                self.unknown += 1;
                if self.unknown_log.len() >= 64 {
                    self.unknown_log.pop_front();
                }
                self.unknown_log.push_back(m.to_vec());
            }
        }
    }

    /// The colour LED `led` shows, as 8-bit RGB, if defined. On the A4, LEDs
    /// 64..127 take their slot from the `0xAg` rows instead of `0x3g` selectors
    /// (docs/A4-PORT.md section 9).
    pub fn led_rgb(&self, led: usize) -> Option<(u8, u8, u8)> {
        let s = self.selectors.get(led >> 2).copied().flatten();
        let s = s.or_else(|| if led >= 64 { self.led_rows.get((led - 64) >> 2).copied().flatten() } else { None })?;
        let slot = (s >> ((led & 3) * 2)) & 3;
        let idx = self.slots[led][slot as usize];
        let (r, g, b) = (*self.palette.get(idx as usize)?)?;
        let c = |v: u8| (v.min(31) as u16 * 255 / 31) as u8;
        Some((c(r), c(g), c(b)))
    }

    /// A single-colour LED of an MKI panel (`0x2g bits`): LED = 8 group + bit.
    pub fn led_mono(&self, led: usize) -> bool {
        self.selectors.get(led >> 3).copied().flatten().is_some_and(|v| v >> (led & 7) & 1 != 0)
    }

    /// Set a key's state. -> the wire message to send, if it changed.
    pub fn key(&mut self, code: u8, down: bool) -> Option<[u8; 2]> {
        let &(_, ch, bit) = WIRE.iter().find(|w| w.0 == code)?;
        self.wire(ch, bit, down)
    }

    /// Press or release the key at wire group `ch`, bit `bit`. -> the message.
    pub fn wire(&mut self, ch: u8, bit: u8, down: bool) -> Option<[u8; 2]> {
        if ch >= self.groups || bit > 7 {
            return None;
        }
        let old = self.held[ch as usize];
        let new = if down { old | (1 << bit) } else { old & !(1 << bit) };
        self.held[ch as usize] = new;
        (new != old).then_some([0x20 | ch, new])
    }

    /// Everything held, released. -> the wire messages.
    pub fn release_all(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        for ch in 0..16u8 {
            if self.held[ch as usize] != 0 {
                self.held[ch as usize] = 0;
                out.extend_from_slice(&[0x20 | ch, 0]);
            }
        }
        out
    }

    /// Turn encoder `ch` (0..8: A..H, then LEVEL/DATA) by `delta` counts.
    pub fn encoder(ch: u8, delta: i8) -> [u8; 2] {
        [0x30 | (ch & 0xF), delta as u8]
    }
}


/// Analog Rytm MKII OS 1.73 front panel: (name, wire group, bit, OS key id).
/// The slot table is the OS's own (`*0x40285610`, 11 groups of 8, read from a
/// running machine); names pair its key ids with the OS button-name table
/// (`0x40235610`, ids at `0x40235798`). PUSH_* (ids 0x70..0x78, nine encoder
/// pushes: A..H and TRACK LEVEL) are named by count from the manual [inferred].
pub const RYTM_KEYS: [(&str, u8, u8, u8); 78] = [
    ("BANK_A", 0, 0, 0xc0),
    ("BANK_B", 0, 1, 0xc1),
    ("BANK_C", 0, 2, 0xc2),
    ("BANK_D", 0, 3, 0xc3),
    ("CHAIN", 0, 4, 0x82),
    ("SONG", 0, 5, 0x81),
    ("FX", 0, 6, 0x10),
    ("FILL", 0, 7, 0xb2),
    ("BANK_E", 1, 0, 0xc4),
    ("BANK_F", 1, 1, 0xc5),
    ("BANK_G", 1, 2, 0xc6),
    ("BANK_H", 1, 3, 0xc7),
    ("RECORD", 1, 4, 0x50),
    ("PLAY", 1, 5, 0x51),
    ("STOP", 1, 6, 0x52),
    ("PAGE", 1, 7, 0x84),
    ("T9", 2, 0, 0x08),
    ("T10", 2, 1, 0x09),
    ("T11", 2, 2, 0x0a),
    ("T12", 2, 3, 0x0b),
    ("T13", 2, 4, 0x0c),
    ("T14", 2, 5, 0x0d),
    ("T15", 2, 6, 0x0e),
    ("T16", 2, 7, 0x0f),
    ("T1", 3, 0, 0x00),
    ("T2", 3, 1, 0x01),
    ("T3", 3, 2, 0x02),
    ("T4", 3, 3, 0x03),
    ("T5", 3, 4, 0x04),
    ("T6", 3, 5, 0x05),
    ("T7", 3, 6, 0x06),
    ("T8", 3, 7, 0x07),
    ("NO", 4, 0, 0x45),
    ("LEFT", 4, 1, 0x40),
    ("DOWN", 4, 2, 0x43),
    ("RIGHT", 4, 3, 0x41),
    ("TEMPO", 4, 4, 0x83),
    ("FIX", 4, 5, 0xb3),
    ("PERF", 4, 6, 0x14),
    ("SCNE", 4, 7, 0x13),
    ("YES", 5, 0, 0x44),
    ("UP", 5, 1, 0x42),
    ("TRIG", 5, 2, 0x30),
    ("SRC", 5, 3, 0x31),
    ("SMPL", 5, 4, 0x32),
    ("FLTR", 5, 5, 0x33),
    ("AMP", 5, 6, 0x34),
    ("LFO", 5, 7, 0x35),
    ("MUTE", 6, 0, 0x11),
    ("PLAYMODE", 6, 1, 0xb5),
    ("QPER", 6, 2, 0xb1),
    ("SAMPLER", 6, 3, 0xb4),
    ("GLOBAL", 6, 4, 0xb0),
    ("FUNC", 6, 5, 0x80),
    ("TRK", 6, 6, 0x86),
    ("RTRG", 6, 7, 0x85),
    ("PUSH_A", 7, 0, 0x70),
    ("PUSH_B", 7, 1, 0x71),
    ("PUSH_C", 7, 2, 0x72),
    ("PUSH_D", 7, 3, 0x73),
    ("PUSH_E", 7, 4, 0x74),
    ("PUSH_F", 7, 5, 0x75),
    ("PUSH_G", 7, 6, 0x76),
    ("PUSH_H", 7, 7, 0x77),
    ("PUSH_LEVEL", 8, 0, 0x78),
    ("CHRO", 8, 1, 0x12),
    ("BD", 9, 0, 0x90),
    ("SD", 9, 1, 0x91),
    ("RS", 9, 2, 0x92),
    ("CP", 9, 3, 0x93),
    ("BT", 9, 4, 0x94),
    ("LT", 9, 5, 0x95),
    ("MT", 9, 6, 0x96),
    ("HT", 9, 7, 0x97),
    ("CH", 10, 0, 0x98),
    ("OH", 10, 1, 0x99),
    ("CY", 10, 2, 0x9a),
    ("CB", 10, 3, 0x9b),
];

/// Analog Four MKII OS 1.56 front panel: (name, wire group, bit, OS key id).
/// The slot table is the OS's own (0x40212158: 12 groups of 8, slot = 8 group +
/// bit, read where the panel parser indexes it, 0x40064582; 0x86 marks an empty
/// slot) [measured]. Names [inferred]: the ids the Rytm shares (trigs, arrows,
/// YES/NO, transport, FUNC, CHAIN, SONG MODE, TEMPO, PAGE, GLOBAL, QPER, FILL,
/// BANK, encoder pushes) keep the Rytm's names; 0x10..0x1c are the 13-key
/// keyboard (white and black keys sit in separate wire groups), 0x20..0x25 the
/// track keys, 0x33..0x38 the PARAMETER keys, the rest after the manual's panel
/// drawing and the wiring's neighbours; see docs/A4-PORT.md for which were
/// checked on screen.
pub const A4_KEYS: [(&str, u8, u8, u8); 89] = [
    ("T1", 0, 0, 0x00),
    ("T2", 0, 1, 0x01),
    ("T3", 0, 2, 0x02),
    ("T4", 0, 3, 0x03),
    ("T5", 0, 4, 0x04),
    ("T6", 0, 5, 0x05),
    ("T7", 0, 6, 0x06),
    ("T8", 0, 7, 0x07),
    ("T9", 1, 0, 0x08),
    ("T10", 1, 1, 0x09),
    ("T11", 1, 2, 0x0a),
    ("T12", 1, 3, 0x0b),
    ("T13", 1, 4, 0x0c),
    ("T14", 1, 5, 0x0d),
    ("T15", 1, 6, 0x0e),
    ("T16", 1, 7, 0x0f),
    ("BANK_E", 2, 0, 0xc4),
    ("BANK_F", 2, 1, 0xc5),
    ("BANK_G", 2, 2, 0xc6),
    ("BANK_H", 2, 3, 0xc7),
    ("RECORD", 2, 4, 0x50),
    ("PLAY", 2, 5, 0x51),
    ("STOP", 2, 6, 0x52),
    ("KB_C1", 2, 7, 0x10),
    ("BANK_A", 3, 0, 0xc0),
    ("BANK_B", 3, 1, 0xc1),
    ("BANK_C", 3, 2, 0xc2),
    ("BANK_D", 3, 3, 0xc3),
    ("CHAIN", 3, 4, 0x82),
    ("SONGMODE", 3, 5, 0x81),
    ("KB_C#1", 3, 6, 0x11),
    ("KB_D#1", 3, 7, 0x13),
    ("KB_D1", 4, 0, 0x12),
    ("KB_E1", 4, 1, 0x14),
    ("KB_F1", 4, 2, 0x15),
    ("KB_G1", 4, 3, 0x17),
    ("KB_A1", 4, 4, 0x19),
    ("KB_B1", 4, 5, 0x1b),
    ("KB_C2", 4, 6, 0x1c),
    ("PAGE", 4, 7, 0x84),
    ("PERF", 5, 0, 0x30),
    ("TRK", 5, 1, 0x92),
    ("PTN", 5, 2, 0x93),
    ("SONG", 5, 3, 0x94),
    ("NO", 5, 4, 0x45),
    ("LEFT", 5, 5, 0x40),
    ("DOWN", 5, 6, 0x43),
    ("RIGHT", 5, 7, 0x41),
    ("KB_F#1", 6, 0, 0x16),
    ("KB_G#1", 6, 1, 0x18),
    ("KB_A#1", 6, 2, 0x1a),
    ("OCT_UP", 6, 3, 0xa4),
    ("TRNS", 6, 4, 0x85),
    ("FX", 6, 5, 0x24),
    ("CV", 6, 6, 0x25),
    ("FILL", 6, 7, 0xb2),
    ("FUNC", 7, 0, 0x80),
    ("KIT", 7, 1, 0x90),
    ("SND", 7, 2, 0x91),
    ("MIX", 7, 3, 0xb3),
    ("YES", 7, 4, 0x44),
    ("UP", 7, 5, 0x42),
    ("ARP", 7, 6, 0x31),
    ("NOTE", 7, 7, 0x32),
    ("OCT_DOWN", 8, 0, 0xa5),
    ("OSC1", 8, 1, 0x33),
    ("OSC2", 8, 2, 0x34),
    ("FILTERS", 8, 3, 0x35),
    ("AMP", 8, 4, 0x36),
    ("ENV", 8, 5, 0x37),
    ("LFO", 8, 6, 0x38),
    ("TRACK4", 8, 7, 0x23),
    ("GLOBAL", 9, 0, 0xb0),
    ("QPER", 9, 1, 0xb1),
    ("TEMPO", 9, 2, 0x83),
    ("PUSH_F", 9, 3, 0x75),
    ("PUSH_G", 9, 4, 0x76),
    ("PUSH_H", 9, 5, 0x77),
    ("PUSH_I", 9, 6, 0x78),
    ("PUSH_J", 9, 7, 0x79),
    ("PUSH_A", 10, 0, 0x70),
    ("PUSH_B", 10, 1, 0x71),
    ("PUSH_C", 10, 2, 0x72),
    ("PUSH_D", 10, 3, 0x73),
    ("PUSH_E", 10, 4, 0x74),
    ("TRACK3", 10, 5, 0x22),
    ("TRACK2", 10, 6, 0x21),
    ("TRACK1", 10, 7, 0x20),
    ("PUSH_LEVEL", 11, 0, 0x7a),
];

/// The A4 keys' LEDs. One LED space of 128, as the Rytm's: LEDs 0..63 take their
/// slot from the `0x2g` selectors, 64..127 from the `0xAg` rows; the colour is
/// the slot's palette entry (`Bs id vv`, `B5`). Measured by pressing each key in
/// the emulator and diffing the panel state (2026-10-07), and from the OS's own
/// LED id -> LED table at 0x4180eb88 (2026-10-12; docs/A4-PORT.md section 9):
/// trigs 0..15, BANK E..H 17..20, RECORD 21, PLAY 22, STOP 23, the keyboard (white
/// keys 24..31, black keys 39..43), BANK A..D 33..36, SONG MODE 38, CV 44, PERF
/// 45, TRK 46, PTN 47, SONG 48, FX 56, KIT 58, MIX 60, ARP 63; NOTE, OSC1, OSC2,
/// FILTERS, AMP, ENV, LFO 64..70; the track keys T4 71, T3 75, T2 76, T1 77; the
/// seven OCTAVE LEDs 78..84 (A4_OCTAVE_LEDS); FILL 32, CHAIN 37, TRNS 55, SND 59,
/// GLOBAL 72; the PATTERN PAGE LEDs 88..91 and PATTERN MODE LEDs 92..95.
pub const A4_LEDS: [(&str, usize); 41] = [
    ("T1", 0), ("T2", 1), ("T3", 2), ("T4", 3), ("T5", 4), ("T6", 5), ("T7", 6), ("T8", 7),
    ("T9", 8), ("T10", 9), ("T11", 10), ("T12", 11), ("T13", 12), ("T14", 13), ("T15", 14), ("T16", 15),
    ("BANK_E", 17), ("BANK_F", 18), ("BANK_G", 19), ("BANK_H", 20),
    ("RECORD", 21), ("PLAY", 22), ("STOP", 23),
    ("BANK_A", 33), ("BANK_B", 34), ("BANK_C", 35), ("BANK_D", 36),
    ("SONGMODE", 38), ("CV", 44), ("PERF", 45), ("TRK", 46), ("PTN", 47), ("SONG", 48),
    ("FX", 56), ("KIT", 58), ("MIX", 60), ("ARP", 63),
    ("NOTE", 64), ("OSC1", 65), ("OSC2", 66), ("FILTERS", 67),
];
/// More A4 key LEDs (kept apart only to keep the table above short).
pub const A4_ROW_LEDS: [(&str, usize); 25] = [
    ("AMP", 68), ("ENV", 69), ("LFO", 70),
    ("TRACK4", 71), ("TRACK3", 75), ("TRACK2", 76), ("TRACK1", 77),
    ("KB_C1", 24), ("KB_D1", 25), ("KB_E1", 26), ("KB_F1", 27), ("KB_G1", 28), ("KB_A1", 29), ("KB_B1", 30), ("KB_C2", 31),
    ("KB_C#1", 39), ("KB_D#1", 40), ("KB_F#1", 41), ("KB_G#1", 42), ("KB_A#1", 43),
    ("FILL", 32), ("CHAIN", 37), ("TRNS", 55), ("SND", 59), ("GLOBAL", 72),
];
/// The A4's OCTAVE LEDs, low to high (the OS's LED ids 64..70); the keyboard's
/// octave lights one of them [measured, docs/A4-PORT.md section 9].
pub const A4_OCTAVE_LEDS: [usize; 7] = [78, 79, 80, 81, 82, 83, 84];
/// The A4's PATTERN PAGE LEDs (pages 1..4: dim = in the pattern, bright = the
/// active page) [measured], and the PATTERN MODE LEDs in panel order SEQUENTIAL,
/// DIRECT START, DIRECT JUMP, TEMP JUMP (FUNC + BANK A..D lights 95, 94, 93, 92;
/// with TEMP JUMP the previous mode's LED stays dim) [measured].
pub const A4_PAGE_LEDS: [usize; 4] = [88, 89, 90, 91];
pub const A4_MODE_LEDS: [usize; 4] = [95, 94, 93, 92];
/// The window's short labels for the PATTERN MODE LEDs (A4 and Rytm).
pub const MODE_LABELS: [&str; 4] = ["SEQ", "D.ST", "D.JMP", "T.JMP"];

/// Which panel tables are in use (set once, when an OS is loaded): 0 the Rytm
/// MKII's, 1 the A4 MKII's, 2 the A4 MKI's, 3 the Rytm MKI's.
static PANEL_KIND: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn use_a4_panel() {
    PANEL_KIND.store(1, std::sync::atomic::Ordering::Relaxed);
}

/// The A4 MKII's panel tables are in use.
pub fn a4_panel() -> bool {
    PANEL_KIND.load(std::sync::atomic::Ordering::Relaxed) == 1
}

pub fn use_a4mk1_panel() {
    PANEL_KIND.store(2, std::sync::atomic::Ordering::Relaxed);
}

/// The A4 MKI's panel tables are in use.
pub fn a4mk1_panel() -> bool {
    PANEL_KIND.load(std::sync::atomic::Ordering::Relaxed) == 2
}

pub fn use_rytmmk1_panel() {
    PANEL_KIND.store(3, std::sync::atomic::Ordering::Relaxed);
}

/// The Rytm MKI's panel tables are in use.
pub fn rytmmk1_panel() -> bool {
    PANEL_KIND.load(std::sync::atomic::Ordering::Relaxed) == 3
}

pub fn use_syntakt_panel() {
    PANEL_KIND.store(4, std::sync::atomic::Ordering::Relaxed);
}

/// The Syntakt's panel tables are in use.
pub fn syntakt_panel() -> bool {
    PANEL_KIND.load(std::sync::atomic::Ordering::Relaxed) == 4
}

/// The key table of the loaded device: the A4's or the Rytm's.
fn device_keys() -> &'static [(&'static str, u8, u8, u8)] {
    match PANEL_KIND.load(std::sync::atomic::Ordering::Relaxed) {
        1 => &A4_KEYS,
        2 => &A4MK1_KEYS,
        3 => &RYTMMK1_KEYS,
        4 => &SYNTAKT_KEYS,
        _ => &RYTM_KEYS,
    }
}

/// Syntakt OS 1.42 front panel: (name, wire group, bit, OS key id). The panel
/// MCU firmware is the Digitakt's, byte for byte, and the OS's button-name
/// table (`0x401D7400`, 48 entries) has the Digitakt's order with SAMPLE -> FX
/// and SRC -> SYN [measured, docs/SYNTAKT-PORT.md]; so the wiring is the
/// Digitakt's (`WIRE`) and the id is the name table's index [inferred from the
/// Digitakt's]. MENU is the `...` key (the OS's "PATTERN MENU"; FUNC: Sound),
/// PUSH_LEVEL the Level/Data push (Sound Browser), PUSH_A..H the knobs'
/// pushes. Checked on screen: see docs/SYNTAKT-PORT.md section 10.
pub const SYNTAKT_KEYS: [(&str, u8, u8, u8); 48] = [
    ("T1", 0, 0, 0x00), ("T2", 0, 1, 0x01), ("T3", 0, 2, 0x02), ("T4", 0, 3, 0x03),
    ("T5", 0, 4, 0x04), ("T6", 0, 5, 0x05), ("T7", 0, 6, 0x06), ("T8", 0, 7, 0x07),
    ("T9", 1, 0, 0x08), ("T10", 1, 1, 0x09), ("T11", 1, 2, 0x0a), ("T12", 1, 3, 0x0b),
    ("T13", 1, 4, 0x0c), ("T14", 1, 5, 0x0d), ("T15", 1, 6, 0x0e), ("T16", 1, 7, 0x0f),
    ("TRK", 2, 0, 0x12), ("FUNC", 2, 1, 0x13), ("TRIG", 2, 2, 0x1b), ("SYN", 2, 3, 0x1c),
    ("PUSH_LEVEL", 2, 4, 0x2f), ("FLTR", 2, 5, 0x1d), ("AMP", 2, 6, 0x1e), ("LFO", 2, 7, 0x1f),
    ("GLOBAL", 3, 0, 0x15), ("FX", 3, 1, 0x16), ("TEMPO", 3, 2, 0x17), ("NO", 3, 3, 0x21),
    ("LEFT", 3, 4, 0x22), ("DOWN", 3, 5, 0x24), ("RIGHT", 3, 6, 0x25), ("PAGE", 3, 7, 0x26),
    ("BANK", 4, 0, 0x10), ("PTN", 4, 1, 0x11), ("MENU", 4, 2, 0x14), ("RECORD", 4, 3, 0x18),
    ("PLAY", 4, 4, 0x19), ("STOP", 4, 5, 0x1a), ("YES", 4, 6, 0x20), ("UP", 4, 7, 0x23),
    ("PUSH_G", 5, 0, 0x2d), ("PUSH_F", 5, 1, 0x2c), ("PUSH_E", 5, 2, 0x2b), ("PUSH_B", 5, 3, 0x28),
    ("PUSH_A", 5, 4, 0x27), ("PUSH_D", 5, 5, 0x2a), ("PUSH_C", 5, 6, 0x29), ("PUSH_H", 5, 7, 0x2e),
];

/// The Syntakt keys' LEDs (RGB, the Digitakt's protocol: `0x2g` selectors,
/// `Bs id vv` slots, `B5` palette).
pub const SYNTAKT_LEDS: [(&str, usize); 38] = [
    ("T1", 0), ("T2", 1), ("T3", 2), ("T4", 3), ("T5", 4), ("T6", 5), ("T7", 6), ("T8", 7),
    ("T9", 8), ("T10", 9), ("T11", 10), ("T12", 11), ("T13", 12), ("T14", 13), ("T15", 14), ("T16", 15),
    ("BANK", 16), ("PTN", 17), ("TRIG", 18), ("SYN", 19), ("FLTR", 20), ("AMP", 21), ("LFO", 22), ("PAGE", 23),
    ("STOP", 24), ("TEMPO", 25), ("YES", 26), ("NO", 27), ("UP", 28), ("LEFT", 29), ("DOWN", 30), ("RIGHT", 31),
    ("FUNC", 32), ("MENU", 33), ("GLOBAL", 34), ("FX", 35), ("RECORD", 36), ("PLAY", 37),
];
/// The LED under TRK: lit red in keyboard mode (FUNC + TRK) [measured].
pub const SYNTAKT_TRK_LED: usize = 39;
/// The Syntakt's pattern page LEDs, 1:4 .. 4:4.
pub const SYNTAKT_PAGE_LEDS: [usize; 4] = [43, 42, 41, 40];

/// Analog Four MKI OS 1.56 front panel: (name, wire group, bit, OS key id).
/// The slot table is the OS's own, `0x401ebaf8` (11 groups of 8, slot = 8 group
/// plus bit; the parser `0x40062b1c` indexes it) [measured]; the OS takes it when
/// boot-flag bit 20 is clear, which is the Analog Four (bit 20 set boots as the
/// Analog Keys: "analog KEYS" logo, `0x400f1110`) [measured]. The A4 MKI sends
/// its keys as the MKII does, `0x2g mask` [measured]. Names [measured on
/// screen or MIDI out, 2026-10-12, unless noted]: the page keys (OSC1 etc. open
/// their pages), BANK_A..D (0x61..0x64: "BANK A : CHOOSE PTN"), the keyboard
/// (group 6, all id 0x86, plays C..C: notes 60, 62 .. 72 on MIDI out; the black
/// keys are ids 0x90..0x94: 61, 63, 66, 68, 70), the transport and the track
/// keys (their LEDs follow). [inferred] from the ids the A4 MKII shares and the
/// panel photo: FUNC, SONGMODE, CHAIN, TEMPO, TRNS, PAGE, the encoder pushes,
/// BANKGROUP (0x60, the one key left on the panel; it shows the track mute row).
/// Group 9 bits 0..3 and 7 and group 10 (ids 0xa0..0xa5) are the Analog Keys'
/// keys, not on the A4's panel [inferred].
pub const A4MK1_KEYS: [(&str, u8, u8, u8); 75] = [
    ("T1", 0, 0, 0x00), ("T2", 0, 1, 0x01), ("T3", 0, 2, 0x02), ("T4", 0, 3, 0x03),
    ("T5", 0, 4, 0x04), ("T6", 0, 5, 0x05), ("T7", 0, 6, 0x06), ("T8", 0, 7, 0x07),
    ("T9", 1, 0, 0x08), ("T10", 1, 1, 0x09), ("T11", 1, 2, 0x0a), ("T12", 1, 3, 0x0b),
    ("T13", 1, 4, 0x0c), ("T14", 1, 5, 0x0d), ("T15", 1, 6, 0x0e), ("T16", 1, 7, 0x0f),
    ("PAGE", 2, 0, 0x84), ("NOTE", 2, 1, 0x32), ("OSC1", 2, 2, 0x33), ("OSC2", 2, 3, 0x34),
    ("FILTERS", 2, 4, 0x35), ("AMP", 2, 5, 0x36), ("ENV", 2, 6, 0x37), ("LFO", 2, 7, 0x38),
    ("TRACK1", 3, 0, 0x20), ("TRACK2", 3, 1, 0x21), ("TRACK3", 3, 2, 0x22), ("TRACK4", 3, 3, 0x23),
    ("FX", 3, 4, 0x24), ("CV", 3, 5, 0x25), ("TEMPO", 3, 6, 0x83), ("TRNS", 3, 7, 0x85),
    ("YES", 4, 0, 0x44), ("NO", 4, 1, 0x45), ("LEFT", 4, 2, 0x40), ("DOWN", 4, 3, 0x43),
    ("RIGHT", 4, 4, 0x41), ("UP", 4, 5, 0x42), ("PERF", 4, 6, 0x30), ("ARP", 4, 7, 0x31),
    ("RECORD", 5, 0, 0x50), ("PLAY", 5, 1, 0x51), ("STOP", 5, 2, 0x52),
    ("KB_C#1", 5, 3, 0x90), ("KB_D#1", 5, 4, 0x91), ("KB_F#1", 5, 5, 0x92), ("KB_G#1", 5, 6, 0x93), ("KB_A#1", 5, 7, 0x94),
    ("KB_C1", 6, 0, 0x86), ("KB_D1", 6, 1, 0x86), ("KB_E1", 6, 2, 0x86), ("KB_F1", 6, 3, 0x86),
    ("KB_G1", 6, 4, 0x86), ("KB_A1", 6, 5, 0x86), ("KB_B1", 6, 6, 0x86), ("KB_C2", 6, 7, 0x86),
    ("BANKGROUP", 7, 0, 0x60), ("SONGMODE", 7, 1, 0x81), ("CHAIN", 7, 2, 0x82), ("FUNC", 7, 3, 0x80),
    ("BANK_A", 7, 4, 0x61), ("BANK_B", 7, 5, 0x62), ("BANK_C", 7, 6, 0x63), ("BANK_D", 7, 7, 0x64),
    ("PUSH_B", 8, 0, 0x71), ("PUSH_C", 8, 1, 0x72), ("PUSH_D", 8, 2, 0x73), ("PUSH_E", 8, 3, 0x74),
    ("PUSH_G", 8, 4, 0x76), ("PUSH_H", 8, 5, 0x77), ("PUSH_I", 8, 6, 0x78), ("PUSH_J", 8, 7, 0x79),
    ("PUSH_A", 9, 4, 0x70), ("PUSH_F", 9, 5, 0x75), ("PUSH_LEVEL", 9, 6, 0x7a),
];

/// Analog Rytm MKI OS 1.74 front panel: (name, wire group, bit, OS key id).
/// The slot table is the OS's own (`*0x40249dd8` = 0x40249e38, 10 groups of 8,
/// 32-bit ids; parser 0x400803aa) [measured, read from a running machine]. The
/// ids are the Rytm MKII's (0x10 FX .. 0x14 PERF, 0x30..0x35 the page keys,
/// 0x80 FUNC, 0x81 SONG, 0x82 CHAIN, 0x83 TEMPO, 0x84 PAGE, 0x85 RTRG, 0x86 TRK,
/// pads 0x90..0x9b, pushes 0x70..0x78) and the A4 MKI's 0x60 BANK GROUP, 0x61..
/// 0x64 BANK A..D; names follow the panel photo (FX/Kit, Mute/Sound, Chromatic/
/// Track, Scene/Pattern, Perf/Song; Track/Global, Function, Retrig/Click; Trig,
/// Synth, Sample, Filter, Amp, LFO; Bank Group; A..D = Trig Mute, Accent,
/// Swing, Slide) [inferred: by id]. Id 0x87 fills the unused slots.
pub const RYTMMK1_KEYS: [(&str, u8, u8, u8); 69] = [
    ("T1", 0, 0, 0x00), ("T2", 0, 1, 0x01), ("T3", 0, 2, 0x02), ("T4", 0, 3, 0x03),
    ("T5", 0, 4, 0x04), ("T6", 0, 5, 0x05), ("T7", 0, 6, 0x06), ("T8", 0, 7, 0x07),
    ("T9", 1, 0, 0x08), ("T10", 1, 1, 0x09), ("T11", 1, 2, 0x0a), ("T12", 1, 3, 0x0b),
    ("T13", 1, 4, 0x0c), ("T14", 1, 5, 0x0d), ("T15", 1, 6, 0x0e), ("T16", 1, 7, 0x0f),
    ("PAGE", 2, 0, 0x84), ("TRIG", 2, 1, 0x30), ("SYNTH", 2, 2, 0x31), ("SMPL", 2, 3, 0x32),
    ("FLTR", 2, 4, 0x33), ("AMP", 2, 5, 0x34), ("LFO", 2, 6, 0x35), ("TEMPO", 2, 7, 0x83),
    ("MUTE", 3, 0, 0x11), ("FX", 3, 1, 0x10), ("TRK", 3, 2, 0x86), ("FUNC", 3, 3, 0x80),
    ("RTRG", 3, 4, 0x85), ("CHRO", 3, 5, 0x12), ("PERF", 3, 6, 0x14), ("SCNE", 3, 7, 0x13),
    ("YES", 4, 0, 0x44), ("NO", 4, 1, 0x45), ("LEFT", 4, 2, 0x40), ("DOWN", 4, 3, 0x43),
    ("RIGHT", 4, 4, 0x41), ("UP", 4, 5, 0x42), ("SONG", 4, 6, 0x81), ("CHAIN", 4, 7, 0x82),
    ("RECORD", 5, 0, 0x50), ("PLAY", 5, 1, 0x51), ("STOP", 5, 2, 0x52), ("BANKGROUP", 5, 3, 0x60),
    ("BANK_A", 5, 4, 0x61), ("BANK_B", 5, 5, 0x62), ("BANK_C", 5, 6, 0x63), ("BANK_D", 5, 7, 0x64),
    ("PUSH_E", 6, 2, 0x74), ("PUSH_D", 6, 3, 0x73), ("PUSH_C", 6, 4, 0x72), ("PUSH_B", 6, 5, 0x71),
    ("PUSH_A", 6, 6, 0x70), ("PUSH_LEVEL", 6, 7, 0x78),
    ("PUSH_H", 7, 5, 0x77), ("PUSH_G", 7, 6, 0x76), ("PUSH_F", 7, 7, 0x75),
    ("BD", 8, 0, 0x90), ("SD", 8, 1, 0x91), ("RS", 8, 2, 0x92), ("CP", 8, 3, 0x93),
    ("BT", 8, 4, 0x94), ("LT", 8, 5, 0x95), ("MT", 8, 6, 0x96), ("HT", 8, 7, 0x97),
    ("CH", 9, 0, 0x98), ("OH", 9, 1, 0x99), ("CY", 9, 2, 0x9a), ("CB", 9, 3, 0x9b),
];

/// The Rytm MKI's key LEDs: single colour, LED = 8 group + bit (the OS sends
/// `0x2g bits`, 12 groups) [measured, pressing keys and reading the groups
/// back, 2026-10-12]: trigs 0..15; RECORD 16, SONG 17, CHAIN 18; BANK A..D
/// 24..27 (the pattern's bank); the bank-group pair 28 / 29 (29 lit for A..D,
/// 28 while BANK GROUP is held) [inferred: E..H and A..D]; CHRO 32, MUTE 33,
/// SCNE 36, PERF 37, FX 39; FLTR 41, SMPL 43, SYNTH 45, TRIG 47, LFO 53, AMP 55.
/// Group 2 bit 4 (LED 20) is lit from boot: the pattern page LEDs 20..23
/// [inferred, as on the A4 MKI]. Groups 7..11 follow the pads (track select,
/// MUTE and CHRO modes light patterns there), not mapped; `0x3g vv` sends 16
/// more values (levels, not bits), not mapped either.
pub const RYTMMK1_LEDS: [(&str, usize); 35] = [
    ("T1", 0), ("T2", 1), ("T3", 2), ("T4", 3), ("T5", 4), ("T6", 5), ("T7", 6), ("T8", 7),
    ("T9", 8), ("T10", 9), ("T11", 10), ("T12", 11), ("T13", 12), ("T14", 13), ("T15", 14), ("T16", 15),
    ("RECORD", 16), ("SONG", 17), ("CHAIN", 18),
    ("BANK_A", 24), ("BANK_B", 25), ("BANK_C", 26), ("BANK_D", 27), ("BANKGROUP", 29),
    ("CHRO", 32), ("MUTE", 33), ("SCNE", 36), ("PERF", 37), ("FX", 39),
    ("FLTR", 41), ("SMPL", 43), ("SYNTH", 45), ("TRIG", 47), ("LFO", 53), ("AMP", 55),
];
/// The Rytm MKI's pattern page LEDs, 1:4 .. 4:4 [inferred].
pub const RYTMMK1_PAGE_LEDS: [usize; 4] = [20, 21, 22, 23];

/// The A4 MKI's LEDs: single colour, eight to a group, LED = 8 group + bit (the
/// OS sends `0x2g bits`, 14 groups) [measured, pressing keys and reading the
/// groups back, 2026-10-12]: trigs 0..15; the pattern page LEDs 20..23
/// [inferred]; REC 32; the four bank LEDs over BANK_A..D 40..43 (the pattern's
/// bank); CHAIN 45, SONG MODE 46; PERF 48, ARP 49, NOTE 50; FILTERS 56, AMP 58,
/// OSC2 60, OSC1 62; FX 64, CV 66, LFO 68, ENV 70; the track keys TRACK2 72,
/// TRACK1 74, TRACK4 76, TRACK3 78. Each page and track key has a second bit
/// beside its LED (bit + 1; the track keys' set while not selected): a second
/// LED or colour, not placed [inferred].
pub const A4MK1_LEDS: [(&str, usize); 37] = [
    ("T1", 0), ("T2", 1), ("T3", 2), ("T4", 3), ("T5", 4), ("T6", 5), ("T7", 6), ("T8", 7),
    ("T9", 8), ("T10", 9), ("T11", 10), ("T12", 11), ("T13", 12), ("T14", 13), ("T15", 14), ("T16", 15),
    ("RECORD", 32), ("BANK_A", 40), ("BANK_B", 41), ("BANK_C", 42), ("BANK_D", 43),
    ("CHAIN", 45), ("SONGMODE", 46), ("PERF", 48), ("ARP", 49), ("NOTE", 50),
    ("FILTERS", 56), ("AMP", 58), ("OSC2", 60), ("OSC1", 62), ("FX", 64), ("CV", 66), ("LFO", 68), ("ENV", 70),
    ("TRACK2", 72), ("TRACK1", 74), ("TRACK4", 76),
];
/// More A4 MKI LEDs (kept apart only to keep the table above short).
pub const A4MK1_MORE_LEDS: [(&str, usize); 1] = [("TRACK3", 78)];
/// The A4 MKI's pattern page LEDs, 1:4 .. 4:4 [inferred: group 2's high
/// nibble, lit as a 4-page pattern's would be].
pub const A4MK1_PAGE_LEDS: [usize; 4] = [20, 21, 22, 23];
/// LED groups the A4 MKI's OS drives.
pub const A4MK1_LED_GROUPS: usize = 14;

/// The Rytm keys' LEDs (LED id as `0x2g`/`0x3g` selectors and `Bs id vv` slots
/// address them). Measured by pressing each key on OS 1.73 and watching which
/// LED changed: a trig placed in GRID RECORD lights its key's LED red, a page or
/// mode key lights its own. [inferred]: PLAYMODE (goes dark when another mode is
/// chosen), STOP (dark while playing), TRK and RTRG (change while held). YES, NO,
/// the arrows, TEMPO, QPER and FUNC changed no LED; LED 44 and 54 are unassigned
/// and LED 51 lights green from boot for a reason not found (it is not QPER).
pub const RYTM_LEDS: [(&str, usize); 55] = [
    ("T1", 0), ("T2", 1), ("T3", 2), ("T4", 3), ("T5", 4), ("T6", 5), ("T7", 6), ("T8", 7),
    ("T9", 8), ("T10", 9), ("T11", 10), ("T12", 11), ("T13", 12), ("T14", 13), ("T15", 14), ("T16", 15),
    ("BANK_E", 17), ("BANK_F", 18), ("BANK_G", 19), ("BANK_H", 20),
    ("RECORD", 21), ("PLAY", 22), ("STOP", 23), ("FILL", 24),
    ("BANK_A", 25), ("BANK_B", 26), ("BANK_C", 27), ("BANK_D", 28),
    ("CHAIN", 29), ("SONG", 30), ("FX", 31),
    ("TRIG", 38), ("SRC", 39), ("SMPL", 40), ("FLTR", 41), ("AMP", 42), ("LFO", 43),
    ("FIX", 45), ("PERF", 46), ("SCNE", 47), ("CHRO", 48), ("MUTE", 49),
    ("PLAYMODE", 50), ("SAMPLER", 52), ("GLOBAL", 53),
    ("TRK", 55), ("RTRG", 56),
    ("CH", 72), ("OH", 73), ("CY", 74), ("CB", 75), ("BT", 76), ("LT", 77), ("MT", 78), ("HT", 79),
];
/// The BD..CP pads' LEDs (80..83), kept apart only to keep the table above short.
pub const RYTM_PAD_LEDS: [(&str, usize); 4] = [("BD", 80), ("SD", 81), ("RS", 82), ("CP", 83)];
/// The Rytm's PATTERN PAGE LEDs, pages 1..4: dim red for the pattern's pages,
/// bright for the page shown (GRID RECORD; PAGE steps it) or played [measured on
/// OS 1.73, docs/RYTM-PORT.md]. And its PATTERN MODE LEDs, SEQUENTIAL, DIRECT
/// START, DIRECT JUMP, TEMP JUMP (FUNC + BANK A..D) [measured].
pub const RYTM_PAGE_LEDS: [usize; 4] = [64, 65, 66, 67];
pub const RYTM_MODE_LEDS: [usize; 4] = [68, 69, 70, 71];

/// A Rytm key's LED, by name.
pub fn rytm_led(name: &str) -> Option<usize> {
    RYTM_LEDS.iter().chain(RYTM_PAD_LEDS.iter()).find(|k| k.0 == name).map(|k| k.1)
}

/// A key's face label: its name, except where the Rytm panel prints something
/// shorter (BANK_A -> A, T1 -> 1, PLAYMODE -> PLAY MODE, SAMPLER -> SAMPLING).
pub fn key_label(code: u8) -> Option<&'static str> {
    let name = key_name(code)?;
    if code < RYTM_CODE_BASE {
        return Some(name);
    }
    Some(match name {
        "SONGMODE" => "SONG M",
        "BANKGROUP" => "BANK GR",
        "OCT_DOWN" => "OCT-",
        "OCT_UP" => "OCT+",
        "TRACK1" => "T1",
        "TRACK2" => "T2",
        "TRACK3" => "T3",
        "TRACK4" => "T4",
        "FILTERS" => "FILTER",
        n if n.starts_with("KB_") => &n[3..],
        "PLAYMODE" => "MODE",
        "SAMPLER" => "SMPLNG",
        "RECORD" => "REC",
        n if n.starts_with("BANK_") => &n[5..],
        n if n.starts_with('T') && n[1..].parse::<u8>().is_ok() => &n[1..],
        n => n,
    })
}

/// Control codes from here on are Rytm keys: code = RYTM_CODE_BASE + index into
/// RYTM_KEYS. They share the window's key plumbing with the Digitakt's codes.
pub const RYTM_CODE_BASE: u8 = 100;

/// The control code of a Rytm key, by name.
pub fn rytm_code(name: &str) -> u8 {
    let i = device_keys().iter().position(|k| k.0 == name).unwrap_or_else(|| panic!("no key {name}"));
    RYTM_CODE_BASE + i as u8
}

/// A Rytm key by name (case-insensitive). -> (wire group, bit).
pub fn rytm_key(name: &str) -> Option<(u8, u8)> {
    device_keys().iter().find(|k| k.0.eq_ignore_ascii_case(name)).map(|k| (k.1, k.2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectors_and_palette() {
        let mut p = PanelMcu::default();
        let mut r = Vec::new();
        for &b in &[0xb8u8, 0x20, 0x01, 0x21, 0x02, 0xb5, 0x03, 0x1f, 0x00, 0x00, 0xb1, 0x00, 0x03] {
            p.from_host(b, &mut r);
        }
        assert_eq!(p.selectors[0], Some(0x01));
        assert_eq!(p.selectors[1], Some(0x02));
        assert_eq!(p.palette[3], Some((31, 0, 0)));
        // LED 0: selector 0 bits 1:0 = 1 -> slot 1 -> palette 3.
        assert_eq!(p.led_rgb(0), Some((255, 0, 0)));
    }
}
