//! The hardware-style windows: the Analog Four MKII and Analog Rytm MKII (light
//! grey panel, dark keys), the two MKI units (charcoal panel, light keys,
//! red secondary labels, single-colour LED dots) and the Syntakt (dark panel,
//! dark keys, red secondary labels) and the Digitakt (dark panel, dark keys, orange
//! secondary labels), each laid out after its front panel.
//!
//! Every control is a `Place` (its rectangle is the hit area); how a key is drawn
//! (key, trig, pad, piano key, round or pill button) and its labels follow from
//! its name. Static artwork (pots that are not modelled, unmapped LEDs, lines)
//! is drawn by `decor` and is not clickable.

use super::{Canvas, Control, Place, View, A4_ENC_LEVEL};
use crate::panel;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dev {
    A4,
    Rytm,
    A4Mk1,
    RytmMk1,
    Syntakt,
    Digitakt,
}

/// The device a window shows in hardware style: each one's own panel, the Digitakt
/// when no other is. The Syntakt is known by its panel tables
/// (`panel::use_syntakt_panel`).
pub fn dev_of(rytm: bool, a4: bool, mk1: bool) -> Option<Dev> {
    if panel::syntakt_panel() {
        Some(Dev::Syntakt)
    } else if mk1 && panel::rytmmk1_panel() {
        Some(Dev::RytmMk1)
    } else if mk1 {
        Some(Dev::A4Mk1)
    } else if a4 {
        Some(Dev::A4)
    } else if rytm {
        Some(Dev::Rytm)
    } else {
        Some(Dev::Digitakt)
    }
}

impl Dev {
    pub fn mk1(self) -> bool {
        matches!(self, Dev::A4Mk1 | Dev::RytmMk1)
    }
    fn title(self) -> &'static str {
        match self {
            Dev::A4 => "ANALOG FOUR MKII",
            Dev::Rytm => "ANALOG RYTM MKII",
            Dev::A4Mk1 => "ANALOG FOUR MKI",
            Dev::RytmMk1 => "ANALOG RYTM MKI",
            Dev::Syntakt => "SYNTAKT",
            Dev::Digitakt => "DIGITAKT",
        }
    }
}

/// The window size for a device.
pub fn size(dev: Dev) -> (usize, usize) {
    match dev {
        Dev::Syntakt => (1180, 880),
        Dev::Digitakt => (1036, 880),
        d if d.mk1() => (1180, 650),
        _ => (1180, 710),
    }
}

struct Theme {
    panel: u32,
    edge: u32,
    key: u32,
    key_text: u32,
    key_dim: u32,
    label: u32,
    sub: u32,
    knob: u32,
    led_off: u32,
    shadow: u32,
}

fn theme(dev: Dev) -> Theme {
    match dev {
        Dev::A4 | Dev::Rytm => Theme {
            panel: if dev == Dev::A4 { 0xA9ABAE } else { 0xB3B5B8 },
            edge: 0x6C6E72,
            key: 0x2B2D30,
            key_text: 0xE8E8E8,
            key_dim: 0x6E7176,
            label: 0x25272A,
            sub: if dev == Dev::A4 { 0x17704D } else { 0x2C5C9E },
            knob: 0x18191B,
            led_off: 0x4C4E52,
            shadow: 0x7E8084,
        },
        Dev::A4Mk1 | Dev::RytmMk1 => Theme {
            panel: 0x1E1E20,
            edge: 0x111113,
            key: 0xD8D9DB,
            key_text: 0x202022,
            key_dim: 0x202022,
            label: 0xF0F0F0,
            sub: 0xE5333F,
            knob: 0xA4A7AC,
            led_off: 0x48484B,
            shadow: 0x0A0A0B,
        },
        Dev::Syntakt => Theme {
            panel: 0x24272C,
            edge: 0x0E1012,
            key: 0x2C2F34,
            key_text: 0xB4B8BE,
            key_dim: 0x7A7E84,
            label: 0xE8EAEC,
            sub: 0xE8484E,
            knob: 0x1A1C1F,
            led_off: 0x3C3F44,
            shadow: 0x101114,
        },
        Dev::Digitakt => Theme {
            panel: 0x2A2B2E,
            edge: 0x0E1012,
            key: 0x35373B,
            key_text: 0xC4C7CC,
            key_dim: 0xA4A8AE,
            label: 0xE8EAEC,
            sub: 0xF08A24,
            knob: 0x1A1C1F,
            led_off: 0x45474C,
            shadow: 0x121316,
        },
    }
}

const HEADER: u32 = 0x161618;
const HEAD_TEXT: u32 = 0xF0F0F0;
const HEAD_DIM: u32 = 0xA0A4AA;
const YELLOW: u32 = 0xFFD050;
const OLED_ON: u32 = 0xEAF2FF;
const OLED_OFF: u32 = 0x020304;

fn rgb(c: (u8, u8, u8)) -> u32 {
    (c.0 as u32) << 16 | (c.1 as u32) << 8 | c.2 as u32
}

/// `a`/256 of `b` over `base`.
fn mix(base: u32, b: u32, a: u32) -> u32 {
    let ch = |s: u32| {
        let (x, y) = ((base >> s) & 0xFF, (b >> s) & 0xFF);
        ((x * (256 - a) + y * a) >> 8) << s
    };
    ch(16) | ch(8) | ch(0)
}

/// An LED colour made legible as text: scaled up so its brightest channel is full.
fn bright(c: (u8, u8, u8)) -> u32 {
    let m = c.0.max(c.1).max(c.2).max(1) as u32;
    let s = |x: u8| (x as u32 * 255 / m).min(255);
    s(c.0) << 16 | s(c.1) << 8 | s(c.2)
}

fn tw(s: &str, scale: i32) -> i32 {
    s.chars().count() as i32 * 6 * scale - scale
}

/// Centred text, lines split on '|', 10 px apart; -> the y under the last line.
fn lines(cv: &mut Canvas, cx: i32, y: i32, s: &str, c: u32) -> i32 {
    let mut y = y;
    for l in s.split('|') {
        cv.text_centered(cx, y, l, 1, c);
        y += 10;
    }
    y
}

// ---------------------------------------------------------------- shapes

impl Canvas {
    /// A filled rectangle with corners of radius `r`.
    pub fn round_fill(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, c: u32) {
        let r = r.min(w / 2).min(h / 2).max(0);
        for yy in y.max(0)..(y + h).min(self.h as i32) {
            for xx in x.max(0)..(x + w).min(self.w as i32) {
                if inside_round(xx, yy, x, y, w, h, r) {
                    self.px[yy as usize * self.w + xx as usize] = c;
                }
            }
        }
    }

    /// `round_fill` shaded from `top` to `bottom`.
    #[allow(clippy::too_many_arguments)]
    pub fn round_grad(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, top: u32, bottom: u32) {
        let r = r.min(w / 2).min(h / 2).max(0);
        for yy in y.max(0)..(y + h).min(self.h as i32) {
            let c = mix(top, bottom, ((yy - y) * 256 / h.max(1)) as u32);
            for xx in x.max(0)..(x + w).min(self.w as i32) {
                if inside_round(xx, yy, x, y, w, h, r) {
                    self.px[yy as usize * self.w + xx as usize] = c;
                }
            }
        }
    }

    /// The outline of `round_fill`, `t` pixels thick.
    #[allow(clippy::too_many_arguments)]
    pub fn round_frame(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, t: i32, c: u32) {
        let r = r.min(w / 2).min(h / 2).max(0);
        let ri = (r - t).max(0);
        for yy in y.max(0)..(y + h).min(self.h as i32) {
            for xx in x.max(0)..(x + w).min(self.w as i32) {
                if inside_round(xx, yy, x, y, w, h, r) && !inside_round(xx, yy, x + t, y + t, w - 2 * t, h - 2 * t, ri) {
                    self.px[yy as usize * self.w + xx as usize] = c;
                }
            }
        }
    }

    /// A line `t` pixels thick.
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, t: i32, c: u32) {
        let n = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
        for i in 0..=n {
            let x = x0 + (x1 - x0) * i / n;
            let y = y0 + (y1 - y0) * i / n;
            self.fill(x - t / 2, y - t / 2, t, t, c);
        }
    }
}

fn inside_round(px: i32, py: i32, x: i32, y: i32, w: i32, h: i32, r: i32) -> bool {
    if w <= 0 || h <= 0 || px < x || py < y || px >= x + w || py >= y + h {
        return false;
    }
    let r = r.min((w - 1) / 2).min((h - 1) / 2);
    if r <= 0 {
        return true;
    }
    // Pixel centres against the corner circles' centres, in half pixels.
    let cx = px.clamp(x + r, x + w - 1 - r);
    let cy = py.clamp(y + r, y + h - 1 - r);
    let (dx, dy) = (px - cx, py - cy);
    dx * dx + dy * dy <= r * r + r / 2
}

// ---------------------------------------------------------------- layouts

fn key(name: &str, x: i32, y: i32, w: i32, h: i32) -> Place {
    Place { c: Control::Key(panel::rytm_code(name)), x, y, w, h }
}

/// A key by its centre.
fn keyc(name: &str, cx: i32, cy: i32, w: i32, h: i32) -> Place {
    key(name, cx - w / 2, cy - h / 2, w, h)
}

/// A knob by its centre and radius; the place also covers its label.
fn knob(enc: u8, push: &str, cx: i32, cy: i32, r: i32) -> Place {
    Place { c: Control::Knob(enc, Some(panel::rytm_code(push))), x: cx - r, y: cy - r, w: 2 * r, h: 2 * r + 14 }
}

pub fn layout(dev: Dev) -> Vec<Place> {
    match dev {
        Dev::A4 => layout_a4(),
        Dev::Rytm => layout_rytm(),
        Dev::A4Mk1 => layout_a4mk1(),
        Dev::RytmMk1 => layout_rytmmk1(),
        Dev::Syntakt => layout_syntakt(),
        Dev::Digitakt => layout_digitakt(),
    }
}

/// Coordinates of the Digitakt panel drawing (manual OS 1.54, section 3.1, at 2.2x;
/// the panel is 282..1080 by 320..974) to the window.
fn dx(px: i32) -> i32 {
    18 + (px - 282) * 5 / 4
}
fn dy(py: i32) -> i32 {
    38 + (py - 328) * 5 / 4
}

/// The Digitakt's control code of a key, by name.
fn dt_code(name: &str) -> u8 {
    (1..48).find(|&c| panel::key_name(c) == Some(name)).unwrap_or_else(|| panic!("no key {name}"))
}

/// The Digitakt, after the manual's panel drawing. Its keys are the upstream
/// control codes (`panel::WIRE`), not `device_keys`.
fn layout_digitakt() -> Vec<Place> {
    let k = |name: &str, px: i32, py: i32, w: i32, h: i32| Place { c: Control::Key(dt_code(name)), x: dx(px) - w / 2, y: dy(py) - h / 2, w, h };
    let mut v = Vec::new();
    for (n, py) in [("FUNC", 655), ("TRK", 714), ("PTN", 824), ("BANK", 908)] {
        v.push(k(n, 356, py, 80, 50));
    }
    for (n, px) in [("SONG", 448), ("GLOBAL", 508), ("SAMPLE", 568), ("TEMPO", 627)] {
        v.push(k(n, px, 655, 50, 50));
    }
    for (n, px) in [("RECORD", 460), ("PLAY", 538), ("STOP", 616)] {
        v.push(k(n, px, 714, 80, 50));
    }
    for (n, px) in [("TRIG", 735), ("SRC", 805), ("FLTR", 876), ("AMP", 947), ("LFO", 1018)] {
        v.push(k(n, px, 615, 52, 50));
    }
    v.push(k("YES", 735, 684, 52, 50));
    v.push(k("NO", 735, 744, 52, 50));
    v.push(k("UP", 854, 684, 52, 50));
    for (n, px) in [("LEFT", 795), ("DOWN", 854), ("RIGHT", 913)] {
        v.push(k(n, px, 744, 52, 50));
    }
    v.push(k("PAGE", 1007, 744, 80, 50));
    for i in 0..16 {
        v.push(k(&(i + 1).to_string(), 460 + (i % 8) * 78, if i < 8 { 825 } else { 908 }, 80, 76));
    }
    // LEVEL/DATA has no push: the manual presses only the DATA ENTRY knobs, and on
    // OS 1.53 the wire's group 2 bit 4 does nothing; FUNC + turn is the Sound Browser.
    let kn = |enc: u8, push: Option<u8>, px: i32, py: i32| Place { c: Control::Knob(enc, push), x: dx(px) - 30, y: dy(py) - 30, w: 60, h: 74 };
    v.push(kn(8, None, 347, 534));
    for i in 0..8u8 {
        let n = ((b'A' + i) as char).to_string();
        v.push(kn(i, Some(dt_code(&n)), [737, 830, 923, 1016][(i % 4) as usize], if i < 4 { 441 } else { 533 }));
    }
    v
}

/// Photo coordinates of the Syntakt panel (a front-on photo) to the window.
fn sx(px: i32) -> i32 {
    14 + (px - 45) * 62 / 100
}
fn sy(py: i32) -> i32 {
    40 + (py - 110) * 56 / 100
}

/// The Syntakt, after a photo of the panel.
fn layout_syntakt() -> Vec<Place> {
    let mut v = Vec::new();
    for (n, px) in [("TRIG", 1095), ("SYN", 1260), ("FLTR", 1425), ("AMP", 1593), ("LFO", 1760)] {
        v.push(keyc(n, sx(px), sy(727), 62, 46));
    }
    v.push(keyc("FUNC", sx(205), sy(817), 80, 46));
    for (n, px) in [("MENU", 420), ("GLOBAL", 562), ("FX", 700), ("TEMPO", 838)] {
        v.push(keyc(n, sx(px), sy(817), 58, 46));
    }
    v.push(keyc("TRK", sx(205), sy(957), 80, 46));
    for (n, px) in [("RECORD", 447), ("PLAY", 630), ("STOP", 815)] {
        v.push(keyc(n, sx(px), sy(955), 80, 50));
    }
    v.push(keyc("YES", sx(1093), sy(887), 58, 46));
    v.push(keyc("UP", sx(1373), sy(887), 58, 46));
    for (n, px) in [("NO", 1093), ("LEFT", 1233), ("DOWN", 1373), ("RIGHT", 1512)] {
        v.push(keyc(n, sx(px), sy(1027), 58, 46));
    }
    v.push(keyc("PAGE", sx(1732), sy(1030), 80, 46));
    v.push(keyc("PTN", sx(205), sy(1213), 80, 46));
    v.push(keyc("BANK", sx(205), sy(1410), 80, 46));
    for i in 0..16 {
        v.push(keyc(&format!("T{}", i + 1), sx(449 + (i % 8) * 1835 / 10), sy(if i < 8 { 1210 } else { 1405 }), 90, 80));
    }
    v.push(knob(8, "PUSH_LEVEL", sx(178), sy(530), 26));
    for i in 0..8u8 {
        let n = format!("PUSH_{}", (b'A' + i) as char);
        let px = [1098, 1318, 1537, 1757][(i % 4) as usize];
        v.push(knob(i, &n, sx(px), sy(if i < 4 { 315 } else { 535 }), 28));
    }
    v
}

/// The Analog Four MKII, after a community drawing of the panel.
fn layout_a4() -> Vec<Place> {
    let mut v = vec![knob(A4_ENC_LEVEL, "PUSH_LEVEL", 298, 98, 27)];
    v.push(key("GLOBAL", 60, 166, 56, 40));
    v.push(key("QPER", 168, 166, 56, 40));
    v.push(key("TEMPO", 276, 166, 56, 40));
    v.push(key("FUNC", 60, 266, 58, 50));
    for (i, n) in ["KIT", "SND", "MIX"].iter().enumerate() {
        v.push(key(n, 164 + i as i32 * 60, 266, 50, 38));
    }
    v.push(key("PERF", 62, 344, 52, 38));
    for (i, n) in ["TRK", "PTN", "SONG"].iter().enumerate() {
        v.push(key(n, 164 + i as i32 * 60, 344, 50, 38));
    }
    for i in 0..8 {
        let n = format!("BANK_{}", (b'A' + i as u8) as char);
        v.push(key(&n, 58 + (i % 4) * 58, 445 + (i / 4) * 58, 52, 40));
    }
    for i in 0..10u8 {
        let n = format!("PUSH_{}", (b'A' + i) as char);
        v.push(knob(i, &n, 665 + (i % 5) as i32 * 95, 103 + (i / 5) as i32 * 95, 26));
    }
    for (i, n) in ["TRACK1", "TRACK2", "TRACK3", "TRACK4", "FX", "CV"].iter().enumerate() {
        v.push(key(n, 1088, 90 + i as i32 * 52, 54, 34));
    }
    v.push(key("YES", 406, 295, 48, 38));
    v.push(key("NO", 406, 345, 48, 40));
    v.push(key("UP", 494, 295, 48, 38));
    v.push(key("LEFT", 460, 345, 48, 40));
    v.push(key("DOWN", 513, 345, 48, 40));
    v.push(key("RIGHT", 566, 345, 48, 40));
    v.push(key("SONGMODE", 406, 422, 50, 38));
    v.push(key("CHAIN", 464, 422, 50, 38));
    v.push(key("RECORD", 406, 492, 54, 48));
    v.push(key("PLAY", 468, 492, 54, 48));
    v.push(key("STOP", 530, 492, 54, 48));
    for (i, n) in ["ARP", "NOTE", "OSC1", "OSC2", "FILTERS", "AMP", "ENV", "LFO"].iter().enumerate() {
        v.push(key(n, 638 + i as i32 * 56, 295, 50, 38));
    }
    v.push(key("OCT_DOWN", 710, 356, 52, 38));
    v.push(key("OCT_UP", 937, 356, 52, 38));
    v.push(key("TRNS", 995, 356, 52, 38));
    // The keyboard: white keys first, the black keys over them (hit-tested first:
    // the window takes the first place hit, so they go before the white keys).
    for (slot, n) in [(0, "C#1"), (1, "D#1"), (3, "F#1"), (4, "G#1"), (5, "A#1")] {
        v.push(key(&format!("KB_{n}"), 638 + (slot + 1) * 50 - 16, 410, 28, 74));
    }
    for (i, n) in ["C1", "D1", "E1", "F1", "G1", "A1", "B1", "C2"].iter().enumerate() {
        v.push(key(&format!("KB_{n}"), 638 + i as i32 * 50, 410, 47, 126));
    }
    v.push(key("FILL", 1088, 515, 54, 40));
    v.push(key("PAGE", 1088, 597, 54, 40));
    for i in 0..16 {
        v.push(key(&format!("T{}", i + 1), 40 + i * 64, 595, 56, 48));
    }
    v
}

/// Photo coordinates of the Rytm MKII panel to the window.
fn rx(px: i32) -> i32 {
    12 + (px - 35) * 613 / 1000
}
fn ry(py: i32) -> i32 {
    45 + (py - 105) * 62 / 100
}

/// The Analog Rytm MKII, after a photo of the panel.
fn layout_rytm() -> Vec<Place> {
    let mut v = Vec::new();
    v.push(keyc("SAMPLER", rx(308), ry(190), 42, 32));
    v.push(keyc("QPER", rx(485), ry(190), 48, 30));
    v.push(knob(8, "PUSH_LEVEL", rx(838), ry(188), 21));
    for (n, px) in [("GLOBAL", 165), ("PLAYMODE", 306), ("MUTE", 395), ("CHRO", 483), ("SCNE", 572), ("PERF", 660), ("FIX", 749), ("TEMPO", 838)] {
        v.push(keyc(n, rx(px), ry(297), 36, 32));
    }
    v.push(keyc("FUNC", rx(167), ry(473), 52, 52));
    v.push(keyc("TRK", rx(167), ry(657), 52, 34));
    v.push(keyc("RTRG", rx(167), ry(824), 52, 34));
    let pads = [["CH", "OH", "CY", "CB"], ["BT", "LT", "MT", "HT"], ["BD", "SD", "RS", "CP"]];
    for (r, row) in pads.iter().enumerate() {
        for (c, n) in row.iter().enumerate() {
            v.push(keyc(n, rx(345 + c as i32 * 152), ry(452 + r as i32 * 167), 78, 78));
        }
    }
    for i in 0..8u8 {
        let n = format!("PUSH_{}", (b'A' + i) as char);
        let px = [1400, 1530, 1662, 1795][(i % 4) as usize];
        v.push(knob(i, &n, rx(px), ry(if i < 4 { 215 } else { 355 }), 22));
    }
    v.push(keyc("YES", rx(1013), ry(500), 36, 32));
    v.push(keyc("UP", rx(1170), ry(500), 36, 32));
    v.push(keyc("NO", rx(1013), ry(587), 36, 32));
    v.push(keyc("LEFT", rx(1092), ry(587), 36, 32));
    v.push(keyc("DOWN", rx(1170), ry(587), 36, 32));
    v.push(keyc("RIGHT", rx(1249), ry(587), 36, 32));
    for (n, px) in [("TRIG", 1399), ("SRC", 1478), ("SMPL", 1556), ("FLTR", 1635), ("AMP", 1714), ("LFO", 1792)] {
        v.push(keyc(n, rx(px), ry(500), 36, 32));
    }
    v.push(keyc("FX", rx(1790), ry(617), 36, 32));
    v.push(keyc("CHAIN", rx(1487), ry(678), 36, 32));
    v.push(keyc("SONG", rx(1589), ry(678), 36, 32));
    for i in 0..8 {
        let n = format!("BANK_{}", (b'A' + i as u8) as char);
        v.push(keyc(&n, 579 + (i % 4) * 70, ry(if i < 4 { 745 } else { 825 }), 58, 30));
    }
    v.push(keyc("RECORD", rx(1437), ry(808), 52, 52));
    v.push(keyc("PLAY", rx(1539), ry(808), 52, 52));
    v.push(keyc("STOP", rx(1641), ry(808), 52, 52));
    v.push(keyc("FILL", rx(1790), ry(825), 36, 32));
    v.push(keyc("PAGE", rx(1793), ry(982), 46, 32));
    for i in 0..16 {
        v.push(keyc(&format!("T{}", i + 1), rx(167 + i * 1016 / 10), ry(965), 52, 52));
    }
    v
}

/// Photo coordinates of the A4 MKI panel (a front-on photo) to the window.
fn ax(px: i32) -> i32 {
    12 + (px - 47) * 611 / 1000
}
fn ay(py: i32) -> i32 {
    40 + (py - 130) * 635 / 1000
}

/// The Analog Four MKI, after a photo of the panel.
fn layout_a4mk1() -> Vec<Place> {
    let mut v = vec![knob(A4_ENC_LEVEL, "PUSH_LEVEL", ax(505), ay(293), 21)];
    v.push(keyc("SONGMODE", ax(260), ay(472), 52, 22));
    v.push(keyc("CHAIN", ax(388), ay(472), 52, 22));
    v.push(keyc("TEMPO", ax(508), ay(472), 28, 28));
    v.push(keyc("FUNC", ax(135), ay(541), 52, 22));
    v.push(keyc("BANKGROUP", ax(135), ay(662), 52, 22));
    for (i, n) in ["BANK_A", "BANK_B", "BANK_C", "BANK_D"].iter().enumerate() {
        v.push(keyc(n, ax(241 + i as i32 * 89), ay(758), 28, 28));
    }
    v.push(keyc("YES", ax(666), ay(472), 28, 28));
    v.push(keyc("NO", ax(666), ay(562), 28, 28));
    v.push(keyc("UP", ax(878), ay(472), 28, 28));
    v.push(keyc("LEFT", ax(789), ay(562), 28, 28));
    v.push(keyc("DOWN", ax(878), ay(562), 28, 28));
    v.push(keyc("RIGHT", ax(967), ay(562), 28, 28));
    v.push(keyc("RECORD", ax(711), ay(758), 52, 21));
    v.push(keyc("PLAY", ax(816), ay(758), 52, 21));
    v.push(keyc("STOP", ax(922), ay(758), 52, 21));
    for i in 0..10u8 {
        let n = format!("PUSH_{}", (b'A' + i) as char);
        let px = [1126, 1271, 1416, 1561, 1707][(i % 5) as usize];
        v.push(knob(i, &n, ax(px), ay(if i < 5 { 218 } else { 365 }), 21));
    }
    for (i, n) in ["PERF", "ARP", "NOTE", "OSC1", "OSC2", "FILTERS", "AMP", "ENV", "LFO"].iter().enumerate() {
        v.push(keyc(n, ax(1103 + i as i32 * 78), ay(516), 28, 28));
    }
    for (i, n) in ["TRACK1", "TRACK2", "TRACK3", "TRACK4", "FX", "CV"].iter().enumerate() {
        v.push(keyc(n, ax(1862), ay(222 + i as i32 * 89), 52, 21));
    }
    v.push(keyc("TRNS", ax(1862), ay(758), 52, 21));
    // The keyboard: the five black keys over the eight white ones.
    for (n, px) in [("C#1", 1148), ("D#1", 1237), ("F#1", 1415), ("G#1", 1504), ("A#1", 1593)] {
        v.push(keyc(&format!("KB_{n}"), ax(px), ay(686), 30, 30));
    }
    for (i, n) in ["C1", "D1", "E1", "F1", "G1", "A1", "B1", "C2"].iter().enumerate() {
        v.push(keyc(&format!("KB_{n}"), ax(1104 + i as i32 * 89), ay(786), 28, 28));
    }
    v.push(keyc("PAGE", ax(1828), ay(930), 52, 21));
    for i in 0..16 {
        v.push(keyc(&format!("T{}", i + 1), ax(135 + i * 1057 / 10), ay(930), 50, 50));
    }
    v
}

/// Photo coordinates of the Rytm MKI panel to the window.
fn bx(px: i32) -> i32 {
    12 + (px - 57) * 635 / 1000
}
fn by(py: i32) -> i32 {
    40 + (py - 140) * 64 / 100
}

/// The Analog Rytm MKI, after a photo of the panel.
fn layout_rytmmk1() -> Vec<Place> {
    let mut v = Vec::new();
    for (i, n) in ["FX", "MUTE", "CHRO", "SCNE", "PERF"].iter().enumerate() {
        v.push(keyc(n, bx(308 + i as i32 * 75), by(247), 30, 30));
    }
    v.push(knob(8, "PUSH_LEVEL", bx(778), by(245), 23));
    v.push(keyc("TEMPO", bx(778), by(388), 28, 28));
    v.push(keyc("TRK", bx(130), by(408), 54, 22));
    v.push(keyc("FUNC", bx(130), by(560), 54, 22));
    v.push(keyc("RTRG", bx(130), by(708), 54, 22));
    let pads = [["CH", "OH", "CY", "CB"], ["BT", "LT", "MT", "HT"], ["BD", "SD", "RS", "CP"]];
    for (r, row) in pads.iter().enumerate() {
        for (c, n) in row.iter().enumerate() {
            v.push(keyc(n, bx(266 + c as i32 * 129), by(410 + r as i32 * 149), 64, 64));
        }
    }
    for i in 0..8u8 {
        let n = format!("PUSH_{}", (b'A' + i) as char);
        let px = [1378, 1518, 1658, 1798][(i % 4) as usize];
        v.push(knob(i, &n, bx(px), by(if i < 4 { 245 } else { 385 }), 24));
    }
    for (i, n) in ["TRIG", "SYNTH", "SMPL", "FLTR", "AMP", "LFO"].iter().enumerate() {
        v.push(keyc(n, bx(1395 + i as i32 * 75), by(525), 28, 28));
    }
    v.push(keyc("YES", bx(932), by(483), 28, 28));
    v.push(keyc("NO", bx(932), by(570), 28, 28));
    v.push(keyc("UP", bx(1135), by(483), 28, 28));
    v.push(keyc("LEFT", bx(1050), by(570), 28, 28));
    v.push(keyc("DOWN", bx(1135), by(570), 28, 28));
    v.push(keyc("RIGHT", bx(1220), by(570), 28, 28));
    v.push(keyc("BANKGROUP", bx(800), by(668), 54, 22));
    for (i, n) in ["BANK_A", "BANK_B", "BANK_C", "BANK_D"].iter().enumerate() {
        v.push(keyc(n, bx(902 + i as i32 * 85), by(760), 28, 28));
    }
    v.push(keyc("RECORD", bx(1326), by(710), 54, 22));
    v.push(keyc("PLAY", bx(1428), by(710), 54, 22));
    v.push(keyc("STOP", bx(1530), by(710), 54, 22));
    v.push(keyc("SONG", bx(1678), by(710), 54, 22));
    v.push(keyc("CHAIN", bx(1782), by(710), 54, 22));
    v.push(keyc("PAGE", bx(1772), by(903), 54, 22));
    for i in 0..16 {
        v.push(keyc(&format!("T{}", i + 1), bx(143 + i * 1017 / 10), by(900), 50, 50));
    }
    v
}

// ---------------------------------------------------------------- labels

/// What is printed on (MKII) or under (MKI) a key.
fn face_label(dev: Dev, name: &str) -> String {
    if dev.mk1() {
        let s = match (dev, name) {
            (_, "SONGMODE") | (Dev::RytmMk1, "SONG") => "SONG",
            (_, "CHAIN") => "CHAIN",
            (_, "TEMPO") => "TEMPO",
            (Dev::A4Mk1, "FUNC") | (Dev::RytmMk1, "FUNC") => "FUNCTION",
            (_, "BANKGROUP") => "BANK GROUP",
            (_, "YES") => "YES/SAVE",
            (_, "NO") => "NO/RELOAD",
            (Dev::RytmMk1, "UP") => "RETRIG+",
            (Dev::RytmMk1, "DOWN") => "RETRIG-",
            (Dev::RytmMk1, "LEFT") => "UTIME-",
            (Dev::RytmMk1, "RIGHT") => "UTIME+",
            (_, "PERF") => "PERF",
            (_, "ARP") => "ARP",
            (_, "NOTE") => "NOTE",
            (_, "OSC1") => "OSC 1|EXT IN|CV A",
            (_, "OSC2") => "OSC 2|CV B",
            (_, "FILTERS") => "FILTERS|CHORUS|CV C",
            (Dev::A4Mk1, "AMP") => "AMP|DELAY|CV D",
            (_, "ENV") => "ENV|REVERB|ENV",
            (Dev::A4Mk1, "LFO") => "LFO|LFO|LFO",
            (_, "TRACK1") => "TRK 1",
            (_, "TRACK2") => "TRK 2",
            (_, "TRACK3") => "TRK 3",
            (_, "TRACK4") => "TRK 4",
            (Dev::A4Mk1, "FX") => "FX",
            (_, "CV") => "CV",
            (_, "TRNS") => "TRANSPOSE",
            (_, "PAGE") => "PAGE",
            (Dev::RytmMk1, "FX") => "FX",
            (_, "MUTE") => "MUTE",
            (_, "CHRO") => "CHROMATIC",
            (_, "SCNE") => "SCENE",
            (_, "TRK") => "TRACK",
            (_, "RTRG") => "RETRIG",
            (_, "TRIG") => "TRIG",
            (_, "SYNTH") => "SYNTH",
            (_, "SMPL") => "SAMPLE",
            (_, "FLTR") => "FILTER",
            (Dev::RytmMk1, "AMP") => "AMP",
            (Dev::RytmMk1, "LFO") => "LFO",
            _ => "",
        };
        return s.to_string();
    }
    if dev == Dev::Digitakt {
        return name.to_string();
    }
    let s = match name {
        "SAMPLER" => "SMPLNG",
        "PLAYMODE" => "PLAY",
        "SONGMODE" => "SONG M",
        "FILTERS" => "FILTER",
        n if n.starts_with("KB_") => "",
        _ => return panel::key_label(panel::rytm_code(name)).unwrap_or("?").to_string(),
    };
    s.to_string()
}

/// The secondary function printed under a key.
fn sub_label(dev: Dev, name: &str) -> &'static str {
    match dev {
        Dev::A4 => match name {
            "GLOBAL" => "SAVE PROJECT",
            "QPER" => "MUTE",
            "TEMPO" => "TAP TEMPO",
            "KIT" => "SAVE KIT",
            "SND" => "SETTINGS",
            "MIX" => "POLY CONFIG",
            "PERF" => "SETUP",
            "TRK" => "METRONOME",
            "PTN" => "SWING",
            "SONG" => "SAVE SONG",
            "BANK_E" => "TRIG MUTE",
            "BANK_F" => "ACCENT",
            "BANK_G" => "NOTE SLD",
            "BANK_H" => "PARAM SLD",
            "YES" => "SAVE",
            "NO" => "RELOAD",
            "LEFT" => "TIME-",
            "RIGHT" => "TIME+",
            "SONGMODE" => "EDIT SONG",
            "CHAIN" => "NEW CHAIN",
            "RECORD" => "COPY",
            "PLAY" => "CLEAR",
            "STOP" => "PASTE",
            "ARP" | "NOTE" => "SETUP",
            "OSC1" => "EXT/CV A",
            "OSC2" => "CV B",
            "FILTERS" => "CHOR/C",
            "AMP" => "DELAY/D",
            "ENV" => "REVERB",
            "LFO" => "LFO",
            "OCT_DOWN" | "OCT_UP" => "OCTAVE",
            "TRNS" => "LOCK",
            "TRACK1" | "TRACK2" | "TRACK3" | "TRACK4" | "FX" | "CV" => "MUTE",
            "FILL" => "CUE FILL",
            "PAGE" => "SCALE",
            _ => "",
        },
        Dev::Rytm => match name {
            "SAMPLER" => "DIRECT",
            "QPER" => "MUTE",
            "GLOBAL" => "SAVE PROJECT",
            "PLAYMODE" => "KIT",
            "MUTE" => "SOUND",
            "CHRO" => "TRACK",
            "SCNE" => "PATTERN",
            "PERF" => "SONG",
            "FIX" => "SETUP",
            "TEMPO" => "TAP TEMPO",
            "TRK" => "SAVE KIT",
            "RTRG" => "METRONOME",
            "YES" => "SAVE",
            "NO" => "RELOAD",
            "UP" => "RETRIG+",
            "LEFT" => "UTIME-",
            "DOWN" => "RETRIG-",
            "RIGHT" => "UTIME+",
            "TRIG" => "QUANTIZE",
            "SRC" => "DELAY|ASSIGN",
            "SMPL" => "REVERB|SAMPLES",
            "FLTR" => "DIST|SETTINGS",
            "AMP" => "COMP",
            "LFO" => "LFO",
            "FX" => "MIDI CONFIG",
            "CHAIN" => "NEW CHAIN",
            "SONG" => "EDIT SONG",
            "BANK_E" => "TRIG MUTE",
            "BANK_F" => "ACCENT",
            "BANK_G" => "SWING",
            "BANK_H" => "SLIDE",
            "RECORD" => "COPY",
            "PLAY" => "CLEAR",
            "STOP" => "PASTE",
            "FILL" => "CUE FILL",
            "PAGE" => "SCALE",
            _ => "",
        },
        Dev::A4Mk1 => match name {
            "SONGMODE" => "EDIT",
            "CHAIN" => "NEW",
            "TEMPO" => "TAP",
            "BANKGROUP" => "DIRECT CHANGE",
            "BANK_A" => "TRIG MUTE",
            "BANK_B" => "ACCENT",
            "BANK_C" => "NOTE|SLIDE",
            "BANK_D" => "PARAMETER|SLIDE",
            "RECORD" => "COPY",
            "PLAY" => "CLEAR",
            "STOP" => "PASTE",
            "PERF" | "ARP" | "NOTE" => "SETUP",
            "TRACK1" | "TRACK2" | "TRACK3" | "TRACK4" | "FX" | "CV" => "MUTE",
            "TRNS" => "LOCK",
            "PAGE" => "SCALE",
            "KB_C1" => "KIT",
            "KB_D1" => "SOUND",
            "KB_E1" => "TRACK",
            "KB_F1" => "PATTERN",
            "KB_G1" => "SONG",
            "KB_A1" => "CLICK",
            "KB_B1" => "SWING",
            "KB_C2" => "GLOBAL",
            _ => "",
        },
        Dev::Syntakt => match name {
            "TRIG" => "QUANTIZE",
            "SYN" => "MACHINE",
            "FLTR" => "DELAY",
            "AMP" => "REVERB",
            "LFO" => "MIXER",
            "MENU" => "SOUND",
            "GLOBAL" => "SAVE PROJ",
            "FX" => "ROUTING",
            "TEMPO" => "TAP TEMPO",
            "TRK" => "KEYBOARD",
            "RECORD" => "COPY",
            "PLAY" => "CLEAR",
            "STOP" => "PASTE",
            "YES" => "SAVE PTN",
            "NO" => "RELOAD PTN",
            "UP" => "RTRG/OCT+",
            "DOWN" => "RTRG/OCT-",
            "LEFT" => "UTIME-",
            "RIGHT" => "UTIME+",
            "PAGE" => "FILL/SCALE",
            "PTN" => "METRONOME",
            "BANK" => "MUTE MODE",
            _ => "",
        },
        Dev::Digitakt => match name {
            "SONG" => "IMP/EXP",
            "GLOBAL" => "SAVE PROJ",
            "SAMPLE" => "DIRECT",
            "TEMPO" => "TAP TEMPO",
            "TRK" => "CHROMATIC",
            "RECORD" => "COPY",
            "PLAY" => "CLEAR",
            "STOP" => "PASTE",
            "PTN" => "METRONOME",
            "BANK" => "MUTE MODE",
            "YES" => "SAVE PTN",
            "NO" => "RELOAD PTN",
            "TRIG" => "QUANTIZE",
            "SRC" => "ASSIGN",
            "FLTR" => "DELAY",
            "AMP" => "REVERB",
            "LFO" => "MASTER",
            "UP" => "RTRG/OCT+",
            "DOWN" => "RTRG/OCT-",
            "LEFT" => "UTIME-",
            "RIGHT" => "UTIME+",
            "PAGE" => "FILL/SCALE",
            _ => "",
        },
        Dev::RytmMk1 => match name {
            "FX" => "KIT",
            "MUTE" => "SOUND",
            "CHRO" => "TRACK",
            "SCNE" => "PATTERN",
            "PERF" => "SONG",
            "TEMPO" => "TAP",
            "TRK" => "GLOBAL",
            "RTRG" => "CLICK",
            "TRIG" => "SETUP",
            "SYNTH" => "DELAY",
            "SMPL" => "REVERB",
            "FLTR" => "DIST",
            "AMP" => "COMP",
            "LFO" => "LFO",
            "BANKGROUP" => "DIRECT CHANGE",
            "BANK_A" => "TRIG MUTE",
            "BANK_B" => "ACCENT",
            "BANK_C" => "SWING",
            "BANK_D" => "SLIDE",
            "RECORD" => "COPY",
            "PLAY" => "CLEAR",
            "STOP" => "PASTE",
            "SONG" => "EDIT",
            "CHAIN" => "NEW",
            "PAGE" => "SCALE",
            _ => "",
        },
    }
}

/// The Rytm pads' numbers.
fn pad_number(name: &str) -> Option<usize> {
    ["BD", "SD", "RS", "CP", "BT", "LT", "MT", "HT", "CH", "OH", "CY", "CB"].iter().position(|&p| p == name).map(|i| i + 1)
}

fn trig_number(name: &str) -> Option<i32> {
    name.strip_prefix('T').and_then(|n| n.parse::<i32>().ok())
}

fn knob_label(dev: Dev, enc: u8) -> &'static str {
    match (dev, enc) {
        (Dev::A4, A4_ENC_LEVEL) | (Dev::Rytm, 8) => "TRACK LEVEL|SOUND BROWSER",
        (Dev::A4Mk1, A4_ENC_LEVEL) | (Dev::RytmMk1, 8) => "LEVEL",
        (Dev::Syntakt, 8) | (Dev::Digitakt, 8) => "LEVEL/DATA",
        _ => ["A", "B", "C", "D", "E", "F", "G", "H", "I", "J"].get(enc as usize).copied().unwrap_or("?"),
    }
}

// ---------------------------------------------------------------- drawing

/// An LED's colour, None when off or too dim to see.
fn led(view: &View, led: usize) -> Option<(u8, u8, u8)> {
    view.leds.get(led).copied().flatten().filter(|&c| level(c) >= 40)
}

/// An LED's brightness: its brightest channel.
fn level(c: (u8, u8, u8)) -> u32 {
    c.0.max(c.1).max(c.2) as u32
}

/// An LED dot: lit in its colour with a halo, or dark.
fn dot(cv: &mut Canvas, th: &Theme, x: i32, y: i32, c: Option<(u8, u8, u8)>) {
    match c {
        Some(c) => {
            cv.disc(x, y, 6, mix(th.panel, rgb(c), 90));
            cv.disc(x, y, 4, rgb(c));
            cv.disc(x - 1, y - 1, 1, mix(rgb(c), 0xFFFFFF, 140));
        }
        None => {
            cv.disc(x, y, 4, mix(th.led_off, 0x000000, 60));
            cv.disc(x, y, 3, th.led_off);
            cv.put(x - 1, y - 1, mix(th.led_off, 0xFFFFFF, 80));
        }
    }
}

/// A knob: shadow and body. Only a pot (`pot`) has a position mark; the
/// encoders are endless and have none.
#[allow(clippy::too_many_arguments)]
fn draw_knob(cv: &mut Canvas, dev: Dev, th: &Theme, cx: i32, cy: i32, r: i32, pressed: bool, pot: bool) {
    cv.disc(cx + 2, cy + 3, r, th.shadow);
    if dev.mk1() {
        // A grey cap lit from the top left: rim, body, a soft highlight.
        let body = if pressed { mix(th.knob, 0x000000, 50) } else { th.knob };
        cv.disc(cx, cy, r, mix(body, 0x000000, 70));
        cv.disc(cx - 1, cy - 1, r - 2, body);
        for k in 1..=4 {
            cv.disc(cx - r / 4 - k / 2, cy - r / 4 - k / 2, r * (6 - k) / 8, mix(body, 0xFFFFFF, 10 * k as u32));
        }
        if pot {
            cv.disc(cx - r * 4 / 10, cy - r * 5 / 10, 2, 0xFAFAFA);
        }
    } else {
        let body = if pressed { 0x3A3C40 } else { th.knob };
        cv.disc(cx, cy, r, body);
        cv.ring(cx, cy, r, mix(body, 0xFFFFFF, 24));
        if pot {
            cv.disc(cx - r * 4 / 10, cy - r * 4 / 10, 2, 0xF0F0F0);
        }
    }
}

/// The OLED (MKII, 128x64) or the backlit LCD (MKI, 122x32) at scale 2.
fn draw_display(cv: &mut Canvas, dev: Dev, view: &View) {
    match dev {
        Dev::A4 | Dev::Rytm => {
            let (bx0, by0, bw, bh) = if dev == Dev::A4 { (345, 58, 290, 212) } else { (544, 72, 280, 192) };
            cv.round_fill(bx0 + 2, by0 + 3, bw, bh, 8, 0x6A6C70);
            cv.round_fill(bx0, by0, bw, bh, 8, 0x050506);
            cv.round_frame(bx0, by0, bw, bh, 8, 1, 0x3A3C40);
            let (tag, name) = if dev == Dev::A4 {
                ("4 VOICE ANALOG TABLETOP SYNTHESIZER", "ANALOG FOUR MKII")
            } else {
                ("8 VOICE ANALOG DRUM COMPUTER & SAMPLER", "ANALOG RYTM MKII")
            };
            let cx = bx0 + bw / 2;
            cv.text_centered(cx, by0 + 10, tag, 1, 0xD0D0D0);
            let (ox, oy) = (cx - 128, by0 + 30);
            for y in 0..64usize {
                for x in 0..128usize {
                    let on = (view.oled[(7 - y / 8) + 8 * x] >> (y % 8)) & 1 != 0;
                    cv.fill(ox + x as i32 * 2, oy + y as i32 * 2, 2, 2, if on { OLED_ON } else { OLED_OFF });
                }
            }
            cv.text_centered(cx, oy + 128 + (bh - 30 - 128 - 14) / 2, name, 2, 0xF0F0F0);
        }
        Dev::Syntakt => {
            // The window: the tag line, the OLED, the name under it.
            let (bx0, by0, bw, bh) = (sx(318), sy(215), sx(965) - sx(318), sy(665) - sy(215));
            cv.round_fill(bx0 + 2, by0 + 3, bw, bh, 6, 0x08090A);
            cv.round_fill(bx0, by0, bw, bh, 6, 0x0A0C10);
            cv.round_frame(bx0, by0, bw, bh, 6, 1, 0x3A3E44);
            cv.text(sx(372), sy(247) - 3, "12 TRACK DRUM COMPUTER & SYNTHESIZER", 1, 0xE0E2E4);
            let (ox, oy) = (bx0 + (bw - 256) / 2, sy(285));
            for y in 0..64usize {
                for x in 0..128usize {
                    let on = (view.oled[(7 - y / 8) + 8 * x] >> (y % 8)) & 1 != 0;
                    cv.fill(ox + x as i32 * 2, oy + y as i32 * 2, 2, 2, if on { OLED_ON } else { OLED_OFF });
                }
            }
            cv.text(sx(372), sy(617) - 6, "SYNTAKT", 2, 0xF4F4F4);
        }
        Dev::Digitakt => {
            // The screen's outline and the OLED.
            let (bx0, by0, bw, bh) = (dx(405), dy(402), dx(679) - dx(405), dy(590) - dy(402));
            cv.round_fill(bx0 + 2, by0 + 3, bw, bh, 4, 0x08090A);
            cv.round_fill(bx0, by0, bw, bh, 4, 0x0A0C10);
            cv.round_frame(bx0, by0, bw, bh, 4, 1, 0x4A4E54);
            let (ox, oy) = (bx0 + (bw - 256) / 2, dy(432));
            for y in 0..64usize {
                for x in 0..128usize {
                    let on = (view.oled[(7 - y / 8) + 8 * x] >> (y % 8)) & 1 != 0;
                    cv.fill(ox + x as i32 * 2, oy + y as i32 * 2, 2, 2, if on { OLED_ON } else { OLED_OFF });
                }
            }
        }
        Dev::A4Mk1 | Dev::RytmMk1 => {
            // A black window; the backlit glass with a margin round the
            // pixels and a soft glow.
            let (wx, wy, ww, wh) = if dev == Dev::A4Mk1 { (ax(612), ay(205), 276, 128) } else { (bx(872) - 6, by(200), 272, 142) };
            let (on, off) = if dev == Dev::A4Mk1 { (0x30343A, 0xEEF2F4) } else { (0x262C3C, 0xD2DAE6) };
            cv.round_fill(wx + 2, wy + 3, ww, wh, 4, 0x0C0C0D);
            cv.round_fill(wx, wy, ww, wh, 4, 0x050505);
            cv.round_frame(wx, wy, ww, wh, 4, 1, 0x48484C);
            let (gw, gh) = (122 * 2 + 8, 32 * 2 + 8);
            let gx = wx + (ww - gw) / 2;
            let gy = if dev == Dev::A4Mk1 { wy + (wh - gh) / 2 } else { wy + 20 };
            for (i, a) in [(4, 30), (2, 60)] {
                cv.fill(gx - i, gy - i, gw + 2 * i, gh + 2 * i, mix(0x050505, off, a));
            }
            cv.fill(gx, gy, gw, gh, off);
            cv.fill(gx, gy, gw, 2, mix(off, 0xFFFFFF, 120));
            for y in 0..32usize {
                for x in 0..122usize {
                    let lit = (view.oled[(3 - y / 8) + 8 * x] >> (y % 8)) & 1 != 0;
                    if lit {
                        cv.fill(gx + 4 + x as i32 * 2, gy + 4 + y as i32 * 2, 2, 2, on);
                    }
                }
            }
            if dev == Dev::RytmMk1 {
                cv.text(gx, gy + gh + 22, "8 VOICE DRUM COMPUTER", 1, 0xE0E0E0);
            }
        }
    }
}

/// The static artwork: pots that are not modelled, panel text, lines and the
/// LEDs no table maps.
fn decor(cv: &mut Canvas, dev: Dev, th: &Theme) {
    match dev {
        Dev::A4 => {
            cv.text(20, 46, "ANALOG FOUR MKII", 1, th.label);
            for (cx, l) in [(90, "MAIN VOLUME"), (197, "QUICK PERF")] {
                draw_knob(cv, dev, th, cx, 100, 22, false, true);
                cv.text_centered(cx, 130, l, 1, th.label);
            }
        }
        Dev::Rytm => {
            cv.text(20, 46, "ANALOG RYTM MKII", 1, th.label);
            for (px, r, l) in [(160, 21, "MAIN VOLUME"), (613, 19, "QUICK PERF AMOUNT")] {
                draw_knob(cv, dev, th, rx(px), ry(188), r, false, true);
                cv.text_centered(rx(px), ry(188) + 27, l, 1, th.label);
            }
            // The line joining PLAY .. PERF.
            cv.fill(rx(306), ry(297) - 1, rx(660) - rx(306), 2, th.edge);
        }
        Dev::A4Mk1 => {
            cv.text(ax(100), ay(150), "ANALOG FOUR", 2, 0xF0F0F0);
            cv.text(ax(215), ay(212), "4 VOICE ANALOG SYNTHESIZER", 1, 0xD8D8D8);
            draw_knob(cv, dev, th, ax(183), ay(293), 21, false, true);
            lines(cv, ax(183), ay(293) + 27, "MASTER|VOLUME", th.label);
            // MODE LEDs over SONG / CHAIN, TEMPO's LED, the bank-group LEDs.
            for (px, py) in [(508, 413), (541, 647), (541, 681), (135, 758)] {
                dot(cv, th, ax(px), ay(py), None);
            }
            cv.text_centered(ax(325), ay(408), "MODE", 1, th.label);
            cv.fill(ax(268), ay(412), ax(298) - ax(268), 1, th.label);
            cv.fill(ax(352), ay(412), ax(381) - ax(352), 1, th.label);
            cv.fill(ax(135) - tw("FUNCTION", 1) / 2 - 7, ay(541) + 17, 3, 7, th.sub);
            for (i, (a, e)) in [("A", "E"), ("B", "F"), ("C", "G"), ("D", "H")].iter().enumerate() {
                let x = ax(241 + i as i32 * 89);
                cv.text_centered(x, ay(642), a, 1, th.label);
                cv.text_centered(x, ay(676), e, 1, th.label);
            }
            cv.fill(ax(190), ay(663), ax(552) - ax(190), 1, th.label);
            // The red line over the transport, the dash to REC's LED.
            cv.fill(ax(676), ay(712), ax(966) - ax(676), 3, th.sub);
            cv.fill(ax(640), ay(758), ax(662) - ax(640), 1, th.label);
            // The dash joining OSC1 and OSC2.
            cv.fill(ax(1360), ay(516), ax(1392) - ax(1360), 2, th.label);
            // OCT LEDs, a column left of the keyboard.
            for py in [658, 691, 724, 758, 791] {
                dot(cv, th, ax(1031), ay(py), None);
            }
            cv.text_centered(ax(1031), ay(822), "OCT", 1, th.label);
            // The keyboard's bracket lines, the dotted lines from the black
            // keys, and its unmapped LEDs.
            for px in [1060, 1326, 1683, 1771] {
                cv.fill(ax(px), ay(660), 1, ay(785) - ay(660), th.label);
            }
            cv.fill(ax(1060), ay(785), ax(1771) - ax(1060), 1, th.label);
            for px in [1148, 1237, 1415, 1504, 1593] {
                dot(cv, th, ax(px), ay(636), None);
                let mut y = ay(686) + 17;
                while y < ay(785) {
                    cv.fill(ax(px), y, 1, 2, th.label);
                    y += 4;
                }
            }
            for i in 0..8 {
                dot(cv, th, ax(1104 + i * 89), ay(737), None);
            }
            trig_line(cv, th, |i| ax(135 + i * 1057 / 10), ay(995) - 3);
        }
        Dev::Syntakt => {
            // Main Volume: an analog pot (with its mark), not a panel input.
            draw_knob(cv, dev, th, sx(178), sy(300), 26, false, true);
            let w = tw("MAIN VOLUME", 1);
            cv.text(sx(178) - w / 2, sy(300) + 34, "MAIN", 1, th.label);
            cv.text(sx(178) - w / 2 + tw("MAIN ", 1), sy(300) + 34, "VOLUME", 1, th.key_text);
            // The Level/Data push's second function, under its name.
            cv.text_centered(sx(178), sy(530) + 26 + 17, "SOUND BROWSER", 1, th.sub);
            // MODIFIER / SETUP beside UP: the up/down arrows' FUNC labels.
            chevron(cv, sx(1462), sy(875), 4, 0, th.sub);
            chevron(cv, sx(1462), sy(897), 4, 1, th.sub);
            lines(cv, sx(1530), sy(866), "MODIFIER|SETUP", th.sub);
            // The trig rows' group labels and their dotted brackets.
            let pair = |cv: &mut Canvas, cx: i32, y: i32, a: &str, b: &str| {
                let (wa, wb) = (tw(a, 1), tw(b, 1));
                let x = cx - (wa + wb) / 2;
                cv.text(x, y, a, 1, th.label);
                cv.text(x + wa, y, b, 1, th.sub);
            };
            let trig_x = |i: i32| sx(449 + i * 1835 / 10);
            let row_y = [sy(1210) + 40 + 7, sy(1405) + 40 + 7];
            pair(cv, (trig_x(3) + trig_x(4)) / 2, row_y[0], "DIGITAL", "/MUTE");
            pair(cv, (trig_x(1) + trig_x(2)) / 2, row_y[1], "ANALOG", "/MUTE");
            for (i, (m, f)) in [("M1", "/RETRIG"), ("M2", "/VELOCITY"), ("M3", "/MOD A"), ("M4", "/MOD B")].iter().enumerate() {
                pair(cv, trig_x(4 + i as i32), row_y[1], m, f);
            }
            let dotted = |cv: &mut Canvas, x0: i32, x1: i32, y: i32| {
                let mut x = x0;
                while x < x1 {
                    cv.fill(x, y + 3, 2, 1, th.label);
                    x += 4;
                }
            };
            // Dotted brackets under the first and last key of each group:
            // 1..8 (Digital), 9..12 (Analog).
            for (r, first, last) in [(0, 0, 7), (1, 0, 3)] {
                let y = row_y[r];
                cv.fill(trig_x(first) - 24, y, 1, 5, th.label);
                dotted(cv, trig_x(first) - 22, trig_x(first) + 22, y + 1);
                dotted(cv, trig_x(last) - 22, trig_x(last) + 22, y + 1);
                cv.fill(trig_x(last) + 23, y, 1, 5, th.label);
            }
        }
        Dev::Digitakt => {
            // The connectors along the top edge.
            for (px, s) in [(382, "LEFT"), (436, "RIGHT"), (491, "INPUT L"), (546, "INPUT R"), (612, "MIDI IN"), (690, "MIDI OUT|SYNC A"), (764, "MIDI THRU|SYNC B"), (846, "USB"), (917, "DC IN"), (998, "POWER")] {
                lines(cv, dx(px), dy(335), s, th.key_dim);
            }
            // Master Volume: an analog pot (with its mark), not a panel input.
            draw_knob(cv, dev, th, dx(347), dy(441), 30, false, true);
            cv.text_centered(dx(347), dy(441) + 37, "MASTER VOLUME", 1, th.label);
            // The Level/Data push's second function, under its name.
            cv.text_centered(dx(347), dy(534) + 30 + 17, "SOUND BROWSER", 1, th.sub);
            // Quick Mute: the bracket right of the trig rows.
            let x = dx(1050);
            for (y0, y1) in [(dy(825), dy(848)), (dy(888), dy(908))] {
                cv.fill(x + 16, y0.min(y1), 1, (y1 - y0).abs(), th.label);
            }
            for y in [dy(825), dy(908)] {
                cv.fill(x, y, 16, 1, th.label);
            }
            lines(cv, dx(1061), dy(858), "QUICK|MUTE", th.sub);
        }
        Dev::RytmMk1 => {
            cv.text(bx(880), by(160), "ANALOG RYTM", 2, 0xF0F0F0);
            draw_knob(cv, dev, th, bx(200), by(185), 24, false, true);
            lines(cv, bx(200), by(185) + 30, "MASTER|VOLUME", th.label);
            for (px, py) in [(782, 332), (802, 760), (1188, 686)] {
                dot(cv, th, bx(px), by(py), None);
            }
            cv.fill(bx(130) - tw("FUNCTION", 1) / 2 - 7, by(560) + 17, 3, 7, th.sub);
            for (i, (a, e)) in [("A", "E"), ("B", "F"), ("C", "G"), ("D", "H")].iter().enumerate() {
                let x = bx(900 + i as i32 * 85);
                cv.text_centered(x, by(647), a, 1, th.label);
                cv.text_centered(x, by(681), e, 1, th.label);
            }
            cv.fill(bx(850), by(668), bx(1195) - bx(850), 1, th.label);
            cv.text_centered(bx(1730), by(646), "MODE", 1, th.label);
            cv.fill(bx(1690), by(653), bx(1708) - bx(1690), 1, th.label);
            cv.fill(bx(1752), by(653), bx(1770) - bx(1752), 1, th.label);
            // The dividers between paired pads.
            for (px, py) in [(330, 410), (588, 410), (588, 560), (588, 708)] {
                for k in 0..3 {
                    cv.fill(bx(px) - 3 + k * 3, by(py) - 1, 2, 3, 0xBDBDBD);
                }
            }
            trig_line(cv, th, |i| bx(143 + i * 1017 / 10), by(968) - 3);
        }
    }
}

/// The MKI's trig numbers on a line, 1, 5, 9 and 13 ringed.
fn trig_line(cv: &mut Canvas, th: &Theme, x: impl Fn(i32) -> i32, y: i32) {
    for i in 0..16 {
        let cx = x(i);
        let n = (i + 1).to_string();
        let w = tw(&n, 1);
        if i % 4 == 0 {
            // Bar starts: the number in a white disc.
            cv.disc(cx, y + 3, 7, th.label);
            cv.text_centered(cx, y, &n, 1, th.panel);
        } else {
            cv.text_centered(cx, y, &n, 1, th.label);
        }
        let (a, b) = (cx + w / 2 + if i % 4 == 0 { 8 } else { 4 }, x(i + 1) - 10);
        if b > a {
            cv.fill(a, y + 3, b - a, 1, th.label);
        }
    }
}

/// LED strips: (LED ids, positions, labels, labels above).
type Strip = (&'static [usize], Vec<(i32, i32)>, &'static [&'static str], bool);

fn strips(dev: Dev) -> Vec<Strip> {
    match dev {
        Dev::A4 => vec![
            (&panel::A4_OCTAVE_LEDS, (0..7).map(|i| (786 + i * 22, 375)).collect(), &["-3", "-2", "-1", "+1", "+2", "+3", "+4"], false),
            (&panel::A4_PAGE_LEDS, (0..4).map(|i| (1059 + i * 21, 580)).collect(), &["1", "2", "3", "4"], false),
            (&panel::A4_MODE_LEDS, (0..4).map(|i| (84 + i * 58, 424)).collect(), &panel::MODE_LABELS, false),
        ],
        Dev::Rytm => vec![
            (&panel::RYTM_PAGE_LEDS, (0..4).map(|i| (1054 + i * 24, ry(930))).collect(), &["1:4", "2:4", "3:4", "4:4"], true),
            (&panel::RYTM_MODE_LEDS, (0..4).map(|i| (579 + i * 70, ry(675))).collect(), &["SEQUENTIAL", "DIRECT|START", "DIRECT|JUMP", "TEMP|JUMP"], false),
        ],
        Dev::A4Mk1 => vec![(&panel::A4MK1_PAGE_LEDS, (0..4).map(|i| (ax(1781) + i * 22, ay(870))).collect(), &["1:4", "2:4", "3:4", "4:4"], false)],
        Dev::Syntakt => vec![
            (&panel::SYNTAKT_PAGE_LEDS, [1655, 1707, 1760, 1813].iter().map(|&px| (sx(px), sy(922))).collect(), &["1:4", "2:4", "3:4", "4:4"], true),
            (&[panel::SYNTAKT_TRK_LED], vec![(sx(205), sy(1057))], &[], false),
        ],
        Dev::Digitakt => vec![
            (&panel::PAGE_LEDS, [973, 995, 1018, 1040].iter().map(|&px| (dx(px), dy(700))).collect(), &["1:4", "2:4", "3:4", "4:4"], true),
            (&[panel::DIGITAKT_TRK_LED], vec![(dx(356), dy(759))], &[], false),
        ],
        Dev::RytmMk1 => vec![
            (&panel::RYTMMK1_PAGE_LEDS, (0..4).map(|i| (bx(1725) + i * 22, by(843))).collect(), &["1:4", "2:4", "3:4", "4:4"], false),
            (&[29], vec![(bx(1188), by(652))], &[], false),
        ],
    }
}

/// Where an MKI key's LED dot sits, if it has one.
fn mk1_led_pos(dev: Dev, name: &str, p: &Place) -> Option<(i32, i32)> {
    let cx = p.x + p.w / 2;
    match (dev, name) {
        (Dev::RytmMk1, "BANKGROUP") => None,
        (Dev::RytmMk1, "SONG") => Some((bx(1678), by(653))),
        (Dev::RytmMk1, "CHAIN") => Some((bx(1782), by(653))),
        (Dev::RytmMk1, "RECORD") => Some((bx(1326), by(653))),
        (Dev::A4Mk1, "RECORD") => Some((ax(630), ay(758))),
        (Dev::A4Mk1, n) if n.starts_with("TRACK") || n == "FX" || n == "CV" => Some((ax(1793), p.y + p.h / 2)),
        (Dev::A4Mk1, "SONGMODE") => Some((ax(260), ay(412))),
        (Dev::A4Mk1, "CHAIN") => Some((ax(388), ay(412))),
        _ if trig_number(name).is_some() => Some((cx, p.y - 16)),
        _ => Some((cx, p.y - 15)),
    }
}

/// A chevron pointing `dir` (0 up, 1 down, 2 left, 3 right).
fn chevron(cv: &mut Canvas, cx: i32, cy: i32, s: i32, dir: u8, c: u32) {
    let (a, b, m) = match dir {
        0 => ((cx - s, cy + s / 2), (cx + s, cy + s / 2), (cx, cy - s / 2)),
        1 => ((cx - s, cy - s / 2), (cx + s, cy - s / 2), (cx, cy + s / 2)),
        2 => ((cx + s / 2, cy - s), (cx + s / 2, cy + s), (cx - s / 2, cy)),
        _ => ((cx - s / 2, cy - s), (cx - s / 2, cy + s), (cx + s / 2, cy)),
    };
    cv.line(a.0, a.1, m.0, m.1, 2, c);
    cv.line(b.0, b.1, m.0, m.1, 2, c);
}

/// The transport and arrow keys' symbols; -> whether `name` has one.
fn icon(cv: &mut Canvas, name: &str, cx: i32, cy: i32, s: i32, c: u32) -> bool {
    match name {
        "RECORD" => {
            cv.ring(cx, cy, s, c);
        }
        "PLAY" => {
            cv.line(cx - s / 2, cy - s, cx - s / 2, cy + s, 2, c);
            cv.line(cx - s / 2, cy - s, cx + s, cy, 2, c);
            cv.line(cx - s / 2, cy + s, cx + s, cy, 2, c);
        }
        "STOP" => {
            cv.frame(cx - s + 1, cy - s + 1, 2 * s - 1, 2 * s - 1, c);
            cv.frame(cx - s + 2, cy - s + 2, 2 * s - 3, 2 * s - 3, c);
        }
        "UP" => chevron(cv, cx, cy, s, 0, c),
        "DOWN" => chevron(cv, cx, cy, s, 1, c),
        "LEFT" => chevron(cv, cx, cy, s, 2, c),
        "RIGHT" => chevron(cv, cx, cy, s, 3, c),
        _ => return false,
    }
    true
}

/// The Syntakt's and Digitakt's symbol keys: `...`, the gear, the waveform, the
/// metronome. -> drawn.
fn syntakt_icon(cv: &mut Canvas, name: &str, cx: i32, cy: i32, c: u32) -> bool {
    match name {
        "MENU" => {
            for k in -1..=1 {
                cv.fill(cx + 7 * k - 2, cy - 2, 5, 4, c);
            }
        }
        "SONG" => {
            for k in -1..=1 {
                cv.fill(cx + 7 * k - 2, cy - 2, 5, 4, c);
            }
        }
        "SAMPLE" => {
            for (k, h) in [(-3, 3), (-2, 6), (-1, 9), (0, 4), (1, 8), (2, 5), (3, 2)] {
                cv.fill(cx + 3 * k, cy - h, 1, 2 * h, c);
            }
        }
        "GLOBAL" => {
            cv.ring(cx, cy, 6, c);
            cv.ring(cx, cy, 2, c);
            for (dx, dy) in [(0, -8), (0, 8), (-8, 0), (8, 0), (-6, -6), (6, -6), (-6, 6), (6, 6)] {
                cv.fill(cx + dx - 1, cy + dy - 1, 2, 2, c);
            }
        }
        "TEMPO" => {
            cv.line(cx - 7, cy + 7, cx - 3, cy - 7, 2, c);
            cv.line(cx + 7, cy + 7, cx + 3, cy - 7, 2, c);
            cv.line(cx - 3, cy - 7, cx + 3, cy - 7, 2, c);
            cv.line(cx - 7, cy + 7, cx + 7, cy + 7, 2, c);
            cv.line(cx, cy + 4, cx + 6, cy - 4, 1, c);
        }
        "FX" => {
            cv.text_centered(cx, cy - 7, "FX", 2, c);
            cv.fill(cx - 10, cy + 9, 20, 2, c);
        }
        _ => return false,
    }
    true
}

/// An MKII key (dark rounded key, trig key, pad or piano key).
#[allow(clippy::too_many_arguments)]
fn draw_key_mk2(cv: &mut Canvas, dev: Dev, th: &Theme, view: &View, p: &Place, name: &str, down: bool, latched: bool) {
    let code = if let Control::Key(c) = p.c { c } else { 0 };
    let lit = panel::led_for_key(code).and_then(|l| led(view, l));
    let has_led = panel::led_for_key(code).is_some();
    let (x, y, w, h) = (p.x, p.y, p.w, p.h);
    let cx = x + w / 2;
    if let Some(n) = pad_number(name).filter(|_| dev == Dev::Rytm) {
        let mut face = 0xE4E5E6;
        if let Some(c) = lit.filter(|&c| level(c) >= 120) {
            for (i, a) in [(6, 40), (4, 70), (2, 110)] {
                cv.round_fill(x - i, y - i, w + 2 * i, h + 2 * i, 8 + i, mix(th.panel, rgb(c), a));
            }
        } else {
            cv.round_fill(x + 1, y + 3, w, h, 7, th.shadow);
        }
        if let Some(c) = lit {
            // A pad lights through its face: the brighter, the more colour.
            face = mix(face, bright(c), level(c) * 200 / 255);
        }
        if down {
            face = mix(face, 0x000000, 40);
        }
        cv.round_fill(x, y, w, h, 7, face);
        cv.round_frame(x, y, w, h, 7, 1, mix(face, 0x000000, 50));
        if latched {
            cv.round_frame(x - 3, y - 3, w + 6, h + 6, 9, 2, YELLOW);
        }
        let lbl = format!("{n}  {name}");
        cv.text_centered(cx, y + h + 6, &lbl, 1, th.label);
        return;
    }
    if let Some(rest) = name.strip_prefix("KB_") {
        let black = rest.contains('#');
        let mut face = if black { 0x1C1D1F } else { 0xEDEEEF };
        if let Some(c) = lit {
            face = mix(face, rgb(c), if black { 150 } else { 110 });
        }
        if down {
            face = if black { mix(face, 0xFFFFFF, 60) } else { mix(face, 0x000000, 50) };
        }
        if black {
            cv.round_fill(x + 2, y + 2, w, h, 4, 0x55575A);
            cv.round_fill(x, y, w, h, 4, face);
            cv.round_fill(x + 5, y + 4, w - 10, h - 14, 3, mix(face, 0xFFFFFF, 30));
        } else {
            cv.round_fill(x, y, w, h, 5, 0x55575A);
            cv.round_fill(x + 1, y + 1, w - 2, h - 2, 4, face);
            cv.fill(x + 1, y + h - 8, w - 2, 1, mix(face, 0x000000, 30));
        }
        if latched {
            cv.round_frame(x, y, w, h, 5, 2, YELLOW);
        }
        return;
    }
    // The Digitakt's trigs go by number (`panel::key_name`).
    let trig = if dev == Dev::Digitakt { name.parse().ok() } else { trig_number(name) };
    let big = trig.is_some() || matches!(name, "RECORD" | "PLAY" | "STOP");
    let r = if big { 6 } else { 5 };
    let mut face = match (dev, name) {
        (Dev::Syntakt, "FUNC") => 0xC8464B,
        (Dev::Digitakt, _) => th.key,
        (Dev::A4, "FUNC") => 0x3B4A3D,
        (_, "FUNC") => 0x2B5A6E,
        _ => th.key,
    };
    // The Digitakt's keys are backlit.
    if let Some(c) = lit.filter(|_| dev == Dev::Digitakt) {
        face = mix(face, rgb(c), level(c) * 150 / 255);
    }
    if down {
        face = mix(face, 0xFFFFFF, 60);
    }
    cv.round_fill(x + 1, y + 2, w, h, r, th.shadow);
    if let Some(c) = lit {
        cv.round_frame(x - 3, y - 3, w + 6, h + 6, r + 3, 2, mix(th.panel, rgb(c), 90));
    }
    cv.round_fill(x, y, w, h, r, face);
    cv.fill(x + r, y + 1, w - 2 * r, 1, mix(face, 0xFFFFFF, 30));
    match lit {
        Some(c) => cv.round_frame(x, y, w, h, r, 2, rgb(c)),
        None => cv.round_frame(x, y, w, h, r, 1, 0x141517),
    }
    if latched {
        cv.round_frame(x - 4, y - 4, w + 8, h + 8, r + 4, 1, YELLOW);
    }
    let ink = match lit {
        // On a backlit Digitakt key the symbol stays pale.
        Some(c) if dev == Dev::Digitakt => mix(bright(c), 0xFFFFFF, 200),
        Some(c) => mix(th.key_dim, bright(c), (level(c) * 256 / 180).min(256)),
        None if dev == Dev::Syntakt && name == "FUNC" => 0xF2D0D0,
        None if dev == Dev::Digitakt && name == "FUNC" => th.sub,
        None if has_led => th.key_dim,
        None => th.key_text,
    };
    let cy = y + h / 2;
    if let Some(n) = trig {
        let s = n.to_string();
        cv.text_centered(cx, cy - 9, &s, 2, ink);
        cv.fill(cx - tw(&s, 2) / 2, cy + 8, tw(&s, 2), 2, ink);
        // The Digitakt frames each bar's first step.
        if dev == Dev::Digitakt && (n - 1) % 4 == 0 {
            cv.round_frame(cx - 23, cy - 21, 46, 42, 3, 1, ink);
        }
    } else {
        let drawn = (matches!(dev, Dev::Syntakt | Dev::Digitakt) && syntakt_icon(cv, name, cx, cy, ink)) || icon(cv, name, cx, cy, if big { 9 } else { 7 }, ink);
        if !drawn {
            let s = face_label(dev, name);
            cv.text_centered(cx, cy - 3, &s, 1, ink);
        }
    }
    // Under the key.
    let under = match trig {
        Some(_) if dev == Dev::Syntakt => "",
        Some(n) if dev == Dev::Digitakt => ["KICK", "SNARE", "TOM", "CLAP", "COWBELL", "CLOSED HAT", "OPEN HAT", "CYMBAL", "MIDI A", "MIDI B", "MIDI C", "MIDI D", "MIDI E", "MIDI F", "MIDI G", "MIDI H"][n as usize - 1],
        Some(n) if dev == Dev::A4 => ["A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P"][n as usize - 1],
        Some(n) => ["BD", "SD", "RS", "CP", "BT", "LT", "MT", "HT", "CH", "OH", "CY", "CB", "", "", "", ""][n as usize - 1],
        None => sub_label(dev, name),
    };
    let col = if trig.is_some() { th.label } else { th.sub };
    lines(cv, cx, y + h + 5, under, col);
}

/// An MKI key (light round or pill button, pad), its LED dot and its labels.
#[allow(clippy::too_many_arguments)]
fn draw_key_mk1(cv: &mut Canvas, dev: Dev, th: &Theme, view: &View, p: &Place, name: &str, down: bool, latched: bool) {
    let code = panel::rytm_code(name);
    let lit = panel::led_for_key(code).and_then(|l| led(view, l));
    let (x, y, w, h) = (p.x, p.y, p.w, p.h);
    let (cx, cy) = (x + w / 2, y + h / 2);
    if panel::led_for_key(code).is_some() {
        if let Some((lx, ly)) = mk1_led_pos(dev, name, p) {
            dot(cv, th, lx, ly, lit);
        }
    }
    if let Some(n) = pad_number(name).filter(|_| dev == Dev::RytmMk1) {
        // A backlit pad: light face, its LED glowing through.
        let mut face = 0xD5DEE6;
        if let Some(c) = lit {
            for (i, a) in [(5, 40), (3, 80)] {
                cv.round_fill(x - i, y - i, w + 2 * i, h + 2 * i, 5 + i, mix(th.panel, rgb(c), a));
            }
            face = mix(face, bright(c), level(c) * 170 / 255);
        } else {
            cv.round_fill(x + 2, y + 3, w, h, 4, th.shadow);
        }
        if down {
            face = mix(face, 0xFFFFFF, 160);
        }
        cv.round_fill(x, y, w, h, 4, mix(face, 0x000000, 50));
        cv.round_grad(x + 1, y + 1, w - 2, h - 2, 3, mix(face, 0xFFFFFF, 60), mix(face, 0x000000, 20));
        if latched {
            cv.round_frame(x - 3, y - 3, w + 6, h + 6, 6, 2, YELLOW);
        }
        // Name, number, and the white level bar.
        let lbl = format!("{name} {n}");
        let lw = tw(&lbl, 1);
        cv.text(x + 2, y + h + 6, &lbl, 1, th.label);
        cv.fill(x + 2 + lw + 4, y + h + 7, (w - lw - 8).max(8), 5, 0xECECEC);
        return;
    }
    let pill = w > h + 6;
    let trig = trig_number(name);
    const WHITE: u32 = 0xF1F1F1;
    const GREY: u32 = 0xA2A5AA;
    let mut face = match (dev, name) {
        (_, "RECORD") => 0xE2392E,
        (_, "STOP") => WHITE,
        (_, "YES") | (_, "NO") | (_, "FUNC") => WHITE,
        (Dev::A4Mk1, "PERF") | (Dev::A4Mk1, "ARP") | (Dev::A4Mk1, "NOTE") => WHITE,
        (Dev::A4Mk1, n) if n.starts_with("TRACK") || n == "FX" || n == "CV" => WHITE,
        (Dev::A4Mk1, n) if n.starts_with("KB_") && n.contains('#') => 0x161618,
        (Dev::A4Mk1, n) if n.starts_with("KB_") => WHITE,
        (Dev::RytmMk1, "FX") => 0x5C5E62,
        (Dev::RytmMk1, "MUTE") | (Dev::RytmMk1, "CHRO") | (Dev::RytmMk1, "SCNE") | (Dev::RytmMk1, "PERF") | (Dev::RytmMk1, "TRIG") => WHITE,
        _ => GREY,
    };
    if down {
        face = if face < 0x404040 { mix(face, 0xFFFFFF, 70) } else { mix(face, 0x000000, 70) };
    }
    // A rubber cap: drop shadow, dark edge, face shading from a lit top to a
    // darker bottom, a soft highlight.
    let r = if pill { h / 2 } else { w / 2 };
    cv.round_fill(x + 2, y + 3, w, h, r, th.shadow);
    cv.round_fill(x, y, w, h, r, mix(face, 0x000000, 90));
    cv.round_grad(x + 1, y + 1, w - 2, h - 2, r - 1, mix(face, 0xFFFFFF, 50), mix(face, 0x000000, 40));
    if pill {
        cv.round_fill(x + r / 2 + 1, y + 2, w - r - 2, h / 3, h / 6, mix(face, 0xFFFFFF, 70));
    } else {
        cv.disc(cx - r / 4, cy - r / 4, r / 3, mix(face, 0xFFFFFF, 60));
    }
    if latched {
        cv.round_frame(x - 3, y - 3, w + 6, h + 6, r + 3, 2, YELLOW);
    }
    let ink = if face < 0x606060 { 0xE0E0E0 } else { 0x3A3A3C };
    match name {
        "RECORD" => cv.disc(cx, cy, 2, 0x200000),
        "PLAY" => {
            for i in 0..5 {
                cv.fill(cx - 2 + i, cy - 4 + i, 1, 9 - 2 * i, 0xEEEEEE);
            }
        }
        "STOP" => cv.fill(cx - 2, cy - 2, 5, 5, 0x202020),
        _ => {
            icon(cv, name, cx, cy, 5, ink);
        }
    }
    // Labels under the key: the name in white, the secondary function in red.
    let prim = face_label(dev, name);
    let sub = sub_label(dev, name);
    let mut ly = y + h + 6;
    if dev == Dev::A4Mk1 && (name.starts_with("TRACK") || name == "FX" || name == "CV") {
        let (pw, sw) = (tw(&prim, 1), tw(sub, 1));
        let lx = cx - (pw + 6 + sw) / 2;
        cv.text(lx, ly, &prim, 1, th.label);
        cv.text(lx + pw + 6, ly, sub, 1, th.sub);
        return;
    }
    if trig.is_some() {
        return;
    }
    if !prim.is_empty() {
        ly = lines(cv, cx, ly, &prim, th.label);
    }
    if !sub.is_empty() {
        // The Rytm MKI prints the page keys' second functions in grey.
        let grey = dev == Dev::RytmMk1 && matches!(name, "SYNTH" | "SMPL" | "FLTR" | "AMP" | "LFO");
        lines(cv, cx, ly, sub, if grey { 0x9A9C9F } else { th.sub });
    }
}

/// Draw a hardware-style window.
pub fn draw(cv: &mut Canvas, dev: Dev, view: &View, places: &[Place], held: &[u8], latched: &[u8], selected: Option<u8>) {
    let th = theme(dev);
    let (cw, ch) = (cv.w as i32, cv.h as i32);
    cv.fill(0, 0, cw, ch, HEADER);
    let tw0 = cv.text(12, 11, dev.title(), 2, HEAD_TEXT);
    cv.text(12 + tw0 + 28, 15, &view.status, 1, HEAD_DIM);
    let bottom = ch - 28;
    cv.round_fill(10, 35, cw - 20, bottom - 35, 12, th.edge);
    cv.round_fill(12, 37, cw - 24, bottom - 39, 10, th.panel);
    decor(cv, dev, &th);
    draw_display(cv, dev, view);
    for (leds, pos, labels, above) in strips(dev) {
        for (i, (&l, &(x, y))) in leds.iter().zip(pos.iter()).enumerate() {
            dot(cv, &th, x, y, led(view, l));
            if let Some(s) = labels.get(i) {
                lines(cv, x, if above { y - 15 } else { y + 8 }, s, th.label);
            }
        }
    }
    // White piano keys under the black ones: draw in reverse place order.
    for p in places.iter().rev() {
        match p.c {
            Control::Key(code) => {
                let name = panel::key_name(code).unwrap_or("?");
                let (down, lat) = (held.contains(&code), latched.contains(&code));
                if dev.mk1() {
                    draw_key_mk1(cv, dev, &th, view, p, name, down, lat);
                } else {
                    draw_key_mk2(cv, dev, &th, view, p, name, down, lat);
                }
            }
            Control::Knob(enc, push) => {
                let r = p.w / 2;
                let (cx, cy) = (p.x + r, p.y + r);
                draw_knob(cv, dev, &th, cx, cy, r, push.is_some_and(|c| held.contains(&c)), false);
                if selected == Some(enc) {
                    cv.ring(cx, cy, r + 4, YELLOW);
                }
                lines(cv, cx, cy + r + 7, knob_label(dev, enc), th.label);
            }
        }
    }
    let pads = if matches!(dev, Dev::Rytm | Dev::RytmMk1) { "   PAD: CLICK HIGHER = HARDER" } else { "" };
    let hint = format!("CLICK: PRESS   SHIFT-CLICK: LATCH   DRAG/WHEEL A KNOB: TURN   CLICK A KNOB: PUSH   CTRL-DRAG: PUSH+TURN   ARROW KEYS: NUDGE IT   ESC: RELEASE ALL{pads}");
    cv.text(12, ch - 18, &hint, 1, HEAD_DIM);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_shapes_stay_inside() {
        let mut cv = Canvas::new(40, 40);
        cv.round_fill(5, 5, 20, 10, 5, 0xFFFFFF);
        assert_eq!(cv.px[5 * 40 + 5], super::super::BG, "corner cut");
        assert_eq!(cv.px[10 * 40 + 15], 0xFFFFFF);
        cv.round_frame(0, 20, 30, 20, 4, 2, 0x123456);
        assert_eq!(cv.px[30 * 40 + 15], super::super::BG, "frame is hollow");
        assert_eq!(cv.px[30 * 40], 0x123456);
    }
}
