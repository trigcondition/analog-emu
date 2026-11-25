//! Elektron OS `.syx` → ELE3 container → decompressed sections.
//!
//! Layer cake, outermost first:
//!
//! ```text
//!   1. MIDI SysEx  -- F0 00 20 3C <dev> 00 <cmd> ... F7 messages
//!   2. 8-in-7      -- each group of 7 data bytes is preceded by a byte
//!                     holding their high bits, MSB first
//!   3. preamble    -- 8 bytes; bytes 4..8 are a 32-bit content checksum
//!   4. ELE3        -- magic, then a section table of 16-byte entries
//! ```
//!
//! The MKI units (Analog Rytm MKI, Analog Four MKI / Analog Keys) use an older
//! container, ELE2: magic, a 12-byte build, a 4-byte version, MAIN OS's load
//! address, then MAIN OS itself as a packed `[len][sum]` stream. No section
//! table, no other sections. Its packed stream has no end marker: the depacker
//! runs out of input inside a code, which ends the image (docs/MK1-PORT.md).
//!
//! A section's payload is packed when its 8-byte header
//! `[u32 len][u32 byte sum]` is self-consistent; otherwise it is stored raw
//! (the updater, id 4, has a header with sum 0; the build stamp, id 5, has
//! none). Nothing is encrypted or signed.

use std::fmt;

const COUNT_OFF: usize = 0x1C;
const TABLE_OFF: usize = 0x20;
const ENTRY_SZ: usize = 16;

/// Section ids with a known role.
pub const SEC_BOOTSTRAP: u32 = 2;
pub const SEC_MAIN_OS: u32 = 3;
pub const SEC_UPDATER: u32 = 4;
pub const SEC_META: u32 = 5;

#[derive(Debug)]
pub enum Error {
    Syx(String),
    Container(String),
    Depack(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Syx(s) => write!(f, "sysex: {s}"),
            Error::Container(s) => write!(f, "container: {s}"),
            Error::Depack(s) => write!(f, "depack: {s}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Raw `.syx` bytes → the decoded byte stream (preamble + container).
pub fn decode_syx(d: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(d.len());
    let mut i = 0;
    while i < d.len() {
        if d[i] != 0xF0 {
            return Err(Error::Syx(format!("expected F0 at offset {i}")));
        }
        let j = d[i..]
            .iter()
            .position(|&b| b == 0xF7)
            .map(|p| i + p)
            .ok_or_else(|| Error::Syx(format!("unterminated message at {i}")))?;
        let body = &d[i + 1..j];
        i = j + 1;
        // 14-byte bodies are the start/end markers; data messages are 126:
        // a 9-byte header, 116 bytes of 8-in-7 payload, 1 checksum byte.
        if body.len() != 126 {
            continue;
        }
        let payload = &body[9..125];
        let mut k = 0;
        while k < payload.len() {
            let ms = payload[k];
            k += 1;
            for n in 0..7 {
                if k >= payload.len() {
                    break;
                }
                let hi = if (ms >> (6 - n)) & 1 != 0 { 0x80 } else { 0 };
                out.push(payload[k] | hi);
                k += 1;
            }
        }
    }
    Ok(out)
}

#[derive(Clone, Debug)]
pub struct SectionEntry {
    pub id: u32,
    pub offset: u32,
    pub comp_len: u32,
    /// Load address for code sections; for the bootstrap a version word.
    pub dest: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Storage {
    Packed,
    Raw,
}

#[derive(Clone)]
pub struct Section {
    pub entry: SectionEntry,
    pub storage: Storage,
    pub data: Vec<u8>,
}

impl Section {
    pub fn name(&self) -> &'static str {
        match self.entry.id {
            2 => "BOOTSTRAP",
            3 => "MAIN_OS",
            4 => "UPDATER",
            5 => "META",
            8 => "PANEL_MCU",
            _ => "SECTION",
        }
    }
}

pub struct Firmware {
    /// The ELE3 (or ELE2) container, starting at its magic.
    pub container: Vec<u8>,
    /// An ELE2 container (an MKI unit's OS file).
    pub ele2: bool,
    pub build: String,
    pub version: String,
    pub sections: Vec<Section>,
}

fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Decide how a section is stored and return its payload.
///
/// Packed only when the header is self-consistent: the declared length fits
/// and the bytes after it sum to the declared sum. A header with sum 0 is a
/// raw payload with a header to strip; anything else is raw as-is.
pub fn classify(stream: &[u8]) -> (Storage, &[u8]) {
    if stream.len() >= 8 {
        let ln = be32(stream, 0) as usize;
        let sm = be32(stream, 4);
        if ln.checked_add(8).is_some_and(|e| e <= stream.len()) {
            let sum = stream[8..8 + ln]
                .iter()
                .fold(0u32, |a, &b| a.wrapping_add(b as u32));
            if sum == sm {
                return (Storage::Packed, stream);
            }
        }
        if sm == 0 {
            return (Storage::Raw, &stream[8..]);
        }
    }
    (Storage::Raw, stream)
}

impl Firmware {
    pub fn from_syx_bytes(syx: &[u8]) -> Result<Firmware> {
        let dec = decode_syx(syx)?;
        let Some(off) = dec.windows(4).position(|w| w == b"ELE3") else {
            return match dec.windows(4).position(|w| w == b"ELE2") {
                Some(off) => Self::from_ele2(dec[off..].to_vec()),
                None => Err(Error::Container("no ELE3 or ELE2 magic".into())),
            };
        };
        Self::from_container(&dec[off..])
    }

    /// An ELE3 container from its magic, e.g. as it sits in the SPI flash
    /// (whatever follows its last section is kept, unread).
    pub fn from_container(c: &[u8]) -> Result<Firmware> {
        if !c.starts_with(b"ELE3") {
            return Err(Error::Container("no ELE3 magic".into()));
        }
        let c = c.to_vec();
        if c.len() < TABLE_OFF {
            return Err(Error::Container("truncated header".into()));
        }
        let n = be32(&c, COUNT_OFF) as usize;
        if TABLE_OFF + n * ENTRY_SZ > c.len() {
            return Err(Error::Container(format!("section table of {n} does not fit")));
        }
        let build = String::from_utf8_lossy(&c[0x08..0x14]).trim().to_string();
        let version = String::from_utf8_lossy(&c[0x14..0x18]).to_string();
        let mut sections = Vec::with_capacity(n);
        for k in 0..n {
            let t = TABLE_OFF + ENTRY_SZ * k;
            let entry = SectionEntry {
                id: be32(&c, t),
                offset: be32(&c, t + 4),
                comp_len: be32(&c, t + 8),
                dest: be32(&c, t + 12),
            };
            let (s, e) = (entry.offset as usize, (entry.offset + entry.comp_len) as usize);
            if e > c.len() {
                return Err(Error::Container(format!("section {} overruns container", entry.id)));
            }
            let (storage, payload) = classify(&c[s..e]);
            let data = match storage {
                Storage::Packed => depack_section(payload)?,
                Storage::Raw => payload.to_vec(),
            };
            sections.push(Section { entry, storage, data });
        }
        Ok(Firmware { container: c, ele2: false, build, version, sections })
    }

    /// An ELE2 container (from its magic): MAIN OS only.
    fn from_ele2(c: Vec<u8>) -> Result<Firmware> {
        const PACKED: usize = 0x18;
        if c.len() < PACKED + 8 {
            return Err(Error::Container("truncated ELE2 header".into()));
        }
        let build = String::from_utf8_lossy(&c[0x04..0x10]).trim().to_string();
        let version = String::from_utf8_lossy(&c[0x10..0x14]).to_string();
        let dest = be32(&c, 0x14);
        let len = be32(&c, PACKED) as usize;
        if PACKED + 8 + len > c.len() {
            return Err(Error::Container("ELE2 MAIN OS overruns container".into()));
        }
        let stream = &c[PACKED..PACKED + 8 + len];
        let (storage, _) = classify(stream);
        if storage != Storage::Packed {
            return Err(Error::Container("ELE2 MAIN OS: byte sum does not match".into()));
        }
        let data = depack_to_end(stream, 8, 8 + len)?;
        let entry = SectionEntry { id: SEC_MAIN_OS, offset: PACKED as u32, comp_len: (8 + len) as u32, dest };
        let sections = vec![Section { entry, storage, data }];
        Ok(Firmware { container: c, ele2: true, build, version, sections })
    }

    pub fn from_file(path: &std::path::Path) -> Result<Firmware> {
        let d = std::fs::read(path)
            .map_err(|e| Error::Syx(format!("cannot read {}: {e}", path.display())))?;
        // an MKI's NOR dump (or a bare ELE2 container): the container the OS upgrade wrote
        // (at 0x20000 on the Rytm MKI, after the bootstrap), booted as the bootstrap would boot it.
        if d.first() != Some(&0xF0) {
            if let Some(off) = d.windows(4).position(|w| w == b"ELE2") {
                return Self::from_ele2(d[off..].to_vec());
            }
        }
        Self::from_syx_bytes(&d)
    }

    /// The OS container in a flash image, at its slot: parsed and `verify`d.
    pub fn from_flash(c: &[u8]) -> Result<Firmware> {
        let fw = Self::from_container(c)?;
        fw.verify()?;
        Ok(fw)
    }

    /// Every section whose header carries a byte sum (packed sections) still
    /// matches it: what booting from the flash requires, since a unit's
    /// bootstrap checks the container before it starts the OS ("CRC CHECK",
    /// [inferred]: the check itself is not modelled, only its outcome).
    /// A half-written container fails here even when MAIN OS is whole.
    pub fn verify(&self) -> Result<()> {
        for s in &self.sections {
            let (o, n) = (s.entry.offset as usize, s.entry.comp_len as usize);
            let stream = &self.container[o..o + n];
            if !stream.is_empty() && stream.iter().all(|&b| b == 0xFF) {
                return Err(Error::Container(format!("section {} is erased", s.entry.id)));
            }
            // A packed section's header: its length fills the section, give or
            // take the padding to a word (A4 1.56's panel section: 1 byte).
            let fills = |ln: usize| ln + 8 <= stream.len() && stream.len() - (ln + 8) <= 3;
            if stream.len() < 8 || !fills(be32(stream, 0) as usize) {
                continue; // raw, nothing to check
            }
            let ln = be32(stream, 0) as usize;
            let ok = stream[8..8 + ln].iter().fold(0u32, |a, &b| a.wrapping_add(b as u32)) == be32(stream, 4);
            if !ok {
                return Err(Error::Container(format!("section {} fails its byte sum", s.entry.id)));
            }
        }
        Ok(())
    }

    pub fn section(&self, id: u32) -> Option<&Section> {
        self.sections.iter().find(|s| s.entry.id == id)
    }

    pub fn main_os(&self) -> Result<&Section> {
        self.section(SEC_MAIN_OS)
            .ok_or_else(|| Error::Container("no MAIN OS section (id 3)".into()))
    }
}

// ---------------------------------------------------------------------------
// The LZ codec (aPLib-style):
//
//  * control bits are read MSB first from tag bytes fetched on demand;
//    1 = literal byte, 0 = match;
//  * a match begins with an interlaced Elias-gamma g. g == 2 reuses the last
//    offset; otherwise raw = (g << 8) + byte, raw == 767 ends the stream, and
//    offset = raw - 767;
//  * two bits give a short length 1..3; 00 means gamma + 2 follows. Past
//    offset 3328 the length gains 1. The copy count is length + 1.

const BIAS: u32 = 767;
const REUSE: u32 = 2;
const FAR: u32 = 3328;

struct Bits<'a> {
    d: &'a [u8],
    p: usize,
    end: usize,
    tag: u32,
}

impl Bits<'_> {
    fn byte(&mut self) -> Result<u8> {
        if self.p >= self.end {
            return Err(Error::Depack("stream ended early".into()));
        }
        let v = self.d[self.p];
        self.p += 1;
        Ok(v)
    }

    fn bit(&mut self) -> Result<u32> {
        self.tag = (self.tag << 1) & 0x1FF;
        if self.tag & 0xFF == 0 {
            let b = self.byte()? as u32;
            self.tag = (b << 1) | 1;
            return Ok(b >> 7);
        }
        Ok(self.tag >> 8)
    }

    fn gamma(&mut self) -> Result<u32> {
        let mut v = 1u32;
        loop {
            v = (v << 1) | self.bit()?;
            if self.bit()? != 0 {
                return Ok(v);
            }
            if v > 0x0200_0000 {
                return Err(Error::Depack("gamma overflow".into()));
            }
        }
    }
}

/// -> (output, input position after the end marker)
pub fn depack(data: &[u8], pos: usize, end: usize) -> Result<(Vec<u8>, usize)> {
    depack_inner(data, pos, end, false)
}

/// Depack an ELE2 stream, which has no end marker: running out of input ends
/// it (an end marker, if one comes, too).
pub fn depack_to_end(data: &[u8], pos: usize, end: usize) -> Result<Vec<u8>> {
    depack_inner(data, pos, end, true).map(|r| r.0)
}

fn depack_inner(data: &[u8], pos: usize, end: usize, to_end: bool) -> Result<(Vec<u8>, usize)> {
    let mut s = Bits { d: data, p: pos, end, tag: 0 };
    let mut out: Vec<u8> = Vec::with_capacity((end - pos) * 3);
    let r = depack_run(&mut s, &mut out);
    match r {
        Ok(p) => Ok((out, p)),
        Err(Error::Depack(ref m)) if to_end && m == "stream ended early" => Ok((out, s.p)),
        Err(e) => Err(e),
    }
}

fn depack_run(s: &mut Bits, out: &mut Vec<u8>) -> Result<usize> {
    let mut last = 1u32;
    loop {
        if s.bit()? != 0 {
            out.push(s.byte()?);
            continue;
        }
        let g = s.gamma()?;
        let off = if g == REUSE {
            last
        } else {
            let raw = (g << 8).wrapping_add(s.byte()? as u32);
            if raw == BIAS {
                return Ok(s.p);
            }
            let off = raw.wrapping_sub(BIAS);
            last = off;
            off
        };
        let short = 2 * s.bit()? + s.bit()?;
        let mut length = if short != 0 { short } else { s.gamma()? + 2 };
        if off > FAR {
            length += 1;
        }
        let off = off as usize;
        if off == 0 || off > out.len() {
            return Err(Error::Depack(format!(
                "offset {off} outside {} bytes of output",
                out.len()
            )));
        }
        for _ in 0..=length {
            let b = out[out.len() - off];
            out.push(b);
        }
    }
}

/// Depack a section that starts with its `[u32 length][u32 sum]` header.
pub fn depack_section(stream: &[u8]) -> Result<Vec<u8>> {
    let length = be32(stream, 0) as usize;
    let (out, end) = depack(stream, 8, 8 + length)?;
    if end != 8 + length {
        return Err(Error::Depack(format!(
            "end marker at {} of {} stream bytes",
            end - 8,
            length
        )));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_raw_with_zero_sum_header() {
        let s = [0, 0, 0, 4, 0, 0, 0, 0, 1, 2, 3, 4];
        let (k, p) = classify(&s);
        // The declared length fits but the sum (10) is not 0, so: raw.
        assert_eq!(k, Storage::Raw);
        assert_eq!(p, &[1, 2, 3, 4]);
    }

    #[test]
    fn eight_in_seven() {
        // One 126-byte data message whose first group sets every high bit.
        let mut body = vec![0u8; 126];
        body[9] = 0x7F;
        for b in &mut body[10..17] {
            *b = 0x01;
        }
        let mut syx = vec![0xF0];
        syx.extend(&body);
        syx.push(0xF7);
        let out = decode_syx(&syx).unwrap();
        assert_eq!(&out[..7], &[0x81; 7]);
    }

    /// 8-in-7 SysEx data messages carrying `d` (padded with zeros).
    fn to_syx(d: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in d.chunks(101) {
            let mut c = chunk.to_vec();
            c.resize(101, 0);
            let mut body = vec![0u8; 9];
            let mut k = 0;
            while k < 101 {
                let g = &c[k..(k + 7).min(101)];
                let ms = g.iter().enumerate().fold(0u8, |m, (n, &b)| m | ((b >> 7) << (6 - n)));
                body.push(ms);
                body.extend(g.iter().map(|b| b & 0x7F));
                k += 7;
            }
            body.push(0);
            assert_eq!(body.len(), 126);
            out.push(0xF0);
            out.extend(&body);
            out.push(0xF7);
        }
        out
    }

    #[test]
    fn ele2_main_os_runs_to_end_of_input() {
        // Packed: one tag byte of eight literals, then the input ends (no end
        // marker, as in the MKI files).
        let stream = [0xFFu8, 1, 2, 3, 4, 5, 6, 7, 0x88];
        let sum = stream.iter().fold(0u32, |a, &b| a + b as u32);
        let mut c = vec![0u8; 8]; // preamble
        c.extend(b"ELE20204        1.56");
        c.extend(0x4000_0400u32.to_be_bytes());
        c.extend((stream.len() as u32).to_be_bytes());
        c.extend(sum.to_be_bytes());
        c.extend(stream);
        let fw = Firmware::from_syx_bytes(&to_syx(&c)).unwrap();
        assert!(fw.ele2);
        assert_eq!((fw.build.as_str(), fw.version.as_str()), ("0204", "1.56"));
        let os = fw.main_os().unwrap();
        assert_eq!(os.entry.dest, 0x4000_0400);
        assert_eq!(os.data, [1, 2, 3, 4, 5, 6, 7, 0x88]);
    }
}
