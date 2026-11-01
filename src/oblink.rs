//! A stand-in for the Overbridge host -> Rytm audio stream: 12 channels a frame,
//! in Overwitch's host->Rytm order (MAIN L/R, MAIN FX L/R, then BD, SD, RS/CP,
//! BT, LT, MT/HT, CH/OH, CY/CB). The firmware's `voice_output_mix` adds each
//! voice's channel into the voice's digital layer (rytm-teensy
//! docs/overbridge-voice-inputs.md); `Machine` writes these frames into the
//! buffer it reads, above the USB controller, as it feeds USB MIDI.
//!
//! Over TCP (`--ob-link [ADDR]`, 127.0.0.1:7301 by default; 0.0.0.0:7301 for a
//! host on the LAN, e.g. the rytm-teensy Pi engine) the other end plays the
//! Overbridge host. The Rytm is the clock, as with real Overbridge: the emulator
//! asks for frames and the host answers.
//!
//!   emulator -> host: b'N' + u32 LE   frames wanted (the first: 512)
//!                     b'T' + voice, note, velocity (u8 each)   a trig on a voice
//!                       that plays the EXT machine
//!                     b'P' + voice + 8 x u8   the EXT machine's P1..P8 on that
//!                       voice, when they change
//!   (T and P stand in for the Rytm-side report a mod sends over USB MIDI; a mod
//!   build with ext_render reports itself, and then T and P never come.)
//!                     b'R' + u32 LE n + n x 12 x i32 LE   what the Rytm sends the
//!                       host, with a simulated USB host streaming (usbhost.rs):
//!                       MAIN L/R, the 8 voices, AUDIO IN L/R (full scale 2^31);
//!                       not after a hello with HELLO_NO_R
//!                     b'M' + u16 LE len + len bytes   the Rytm's USB MIDI output,
//!                       as it comes (after a hello with HELLO_MIDI); more than
//!                       65535 bytes at once (a long SysEx) is split across M
//!                       messages, so the host must treat M as a byte stream,
//!                       not as whole MIDI messages
//!                     b'H' + 1 + granted flags   the answer to the host's hello; queued
//!                       before the grant takes effect, so no M comes ahead of it (an R
//!                       queued before a HELLO_NO_R hello may still arrive just after it)
//!   host -> emulator: b'A' + u32 LE n + n x 12 x i32 LE   samples (full scale
//!                     2^31, as Overbridge's int32)
//!                     b'a' + u32 LE n + n x 8 x i16 LE   the 8 voices only, in
//!                       physical voice order (after HELLO_VOICES): voice v on
//!                       channel VOICE_CH[v] as v << 16, every other channel 0
//!                     b'H' + version 1 + flags   a hello: the extras below, all
//!                       optional; a host that sends none gets the stream above
//!                     b'M' + u16 LE len + len bytes   MIDI into the Rytm's USB MIDI
//!                       input (after HELLO_MIDI), a byte stream as the other way: a
//!                       SysEx (up to SYSEX_MAX) or a running status may span M
//!                       messages; a longer SysEx is dropped and counted
//!
//! One host at a time, the newest: a new connection closes the one before, so a
//! stalled or dead host (no FIN seen) never holds the link; the Pi engine just
//! reconnects. A host that stops reading is dropped after WRITE_TIMEOUT of a
//! blocked write. There is no read timeout: a paused emulator asks for nothing,
//! and a live host may then send nothing.
//!
//! Every queue is bounded. Emulator -> host messages wait for a host that stops
//! reading in at most TO_HOST_CAP messages; past that they are dropped and
//! counted (an N that cannot be queued is asked again later). Host -> emulator
//! frames beyond FRAME_CAP (a host sending more than it was asked for) and MIDI
//! beyond MIDI_CAP messages or MIDI_BYTES_CAP bytes are dropped and counted;
//! machine.rs takes the host's MIDI only while the Rytm's USB MIDI input holds
//! less than USB_IN_HELD bytes. A host that answers N with what it was asked for
//! never fills them.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

pub const CHANNELS: usize = 12;
pub type Frame = [i32; CHANNELS];

/// Frames to keep buffered (about 10 ms).
const TARGET: usize = 512;

/// The hello's flags.
pub const HELLO_MIDI: u8 = 1;
pub const HELLO_NO_R: u8 = 2;
pub const HELLO_VOICES: u8 = 4;
/// The most frames one `A`/`a` message may carry; more drops the connection.
const MAX_FRAMES: usize = 65536;
const GRANTED: u8 = HELLO_MIDI | HELLO_NO_R | HELLO_VOICES;
/// Emulator -> host messages queued for a host's writer. Every 32-frame block (about 0.67 ms) queues at most an
/// N, an R and a few M/T/P: about 2 s of output, a few MB of R at most.
const TO_HOST_CAP: usize = 8192;
/// Host -> emulator frames held, in the channel and in the caller's queue each: 4 x what is ever asked for (the
/// queue plus the frames in flight stay at TARGET).
const FRAME_CAP: usize = 4 * TARGET;
/// Host -> emulator MIDI messages waiting for the next block (drained every block).
const MIDI_CAP: usize = 4096;
/// Host -> emulator MIDI bytes in flight (received, not yet taken by `take_midi`).
const MIDI_BYTES_CAP: usize = 256 * 1024;
/// The longest SysEx from the host (the Rytm's USB MIDI pool holds 1024 x 152 bytes).
const SYSEX_MAX: usize = 64 * 1024;
/// machine.rs gives the host's MIDI to the Rytm only while its USB MIDI input (io.rs `usb_in`) holds less.
pub const USB_IN_HELD: usize = 64 * 1024;
/// A write to the host blocked this long (it stopped reading) drops the connection.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Physical voice v's host -> Rytm channel (the firmware's table at 0x401bea90, OS 1.73; rytm-teensy ob_block.h).
pub const VOICE_CH: [usize; 8] = [4, 7, 5, 8, 10, 9, 11, 6];

struct Shared {
    /// The connected host's number and writer (None between hosts: what is sent then is dropped). Per
    /// connection, so a host never loses a message to the previous host's writer.
    tx: Mutex<Option<(u64, mpsc::SyncSender<Vec<u8>>)>>,
    /// Connections accepted so far (`poll` restarts its request count on a new one).
    conns: AtomicU64,
    /// The connected host's grant (stored under `tx`'s lock, for the current host only).
    flags: AtomicU8,
    /// Host MIDI bytes received and not yet taken (bounded by MIDI_BYTES_CAP).
    midi_in_flight: AtomicUsize,
    midi_to_host: AtomicU64,
    midi_from_host: AtomicU64,
    /// Messages for the host dropped because its writer's queue was full (a host that stopped reading).
    dropped_to_host: AtomicU64,
    /// Host frames and MIDI messages dropped because their queue was full (more than was asked for).
    dropped_frames: AtomicU64,
    dropped_midi: AtomicU64,
    dropped_midi_bytes: AtomicU64,
    to_host_cap: usize,
    frame_cap: usize,
    write_timeout: Duration,
}

/// What the link dropped since the start (every queue is bounded; see the module doc).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dropped {
    /// Messages for the host (its writer's queue was full).
    pub to_host: u64,
    /// Frames from the host beyond what the emulator holds.
    pub frames: u64,
    /// MIDI from the host beyond what the emulator holds (M messages, or a SysEx too long or cut short), and
    /// its bytes.
    pub midi: u64,
    pub midi_bytes: u64,
}

pub struct ObLink {
    /// Host frames, each tagged with its connection's number (1, 2, ...): frames an earlier host left in the
    /// channel are dropped, not counted against the next host's requests.
    frames: mpsc::Receiver<(u64, Frame)>,
    midi: mpsc::Receiver<Vec<u8>>,
    shared: Arc<Shared>,
    addr: SocketAddr,
    outstanding: usize,
    conn: u64,
    /// A host is connected and has sent frames.
    pub connected: bool,
    /// Frames the Rytm needed from the link while a host was connected and none had come (the path without a
    /// simulated USB host; with one, usbhost.rs counts `out_underruns`).
    pub underruns: u64,
    /// The host's MIDI came while the profile has no USB MIDI queue to take it (logged once; machine.rs).
    pub midi_unrouted_logged: bool,
}

impl ObLink {
    /// Listen on `addr`; the newest host to connect is served (one at a time).
    pub fn listen(addr: &str) -> std::io::Result<ObLink> {
        Self::listen_caps(addr, TO_HOST_CAP, FRAME_CAP, WRITE_TIMEOUT)
    }

    fn listen_caps(addr: &str, to_host_cap: usize, frame_cap: usize, write_timeout: Duration) -> std::io::Result<ObLink> {
        let l = TcpListener::bind(addr)?;
        let local = l.local_addr()?;
        let (ftx, frx) = mpsc::sync_channel::<(u64, Frame)>(frame_cap);
        let (mtx, mrx) = mpsc::sync_channel::<Vec<u8>>(MIDI_CAP);
        let shared = Arc::new(Shared {
            tx: Mutex::new(None),
            conns: AtomicU64::new(0),
            flags: AtomicU8::new(0),
            midi_in_flight: AtomicUsize::new(0),
            midi_to_host: AtomicU64::new(0),
            midi_from_host: AtomicU64::new(0),
            dropped_to_host: AtomicU64::new(0),
            dropped_frames: AtomicU64::new(0),
            dropped_midi: AtomicU64::new(0),
            dropped_midi_bytes: AtomicU64::new(0),
            to_host_cap,
            frame_cap,
            write_timeout,
        });
        let sh = shared.clone();
        std::thread::spawn(move || {
            let mut prev: Option<TcpStream> = None;
            for s in l.incoming().flatten() {
                eprintln!("overbridge link: host connected from {}", s.peer_addr().map(|a| a.to_string()).unwrap_or_default());
                if let Some(p) = prev.take() {
                    let _ = p.shutdown(std::net::Shutdown::Both); // the newest host wins: its reader and writer end
                }
                let _ = s.set_nodelay(true);
                let (Ok(w), Ok(c)) = (s.try_clone(), s.try_clone()) else { continue };
                prev = Some(c);
                // Counted before its writer is installed: `poll` never sees this writer under an older number.
                let conn = sh.conns.fetch_add(1, Ordering::SeqCst) + 1;
                let (ctx, crx) = mpsc::sync_channel::<Vec<u8>>(sh.to_host_cap);
                {
                    let mut g = sh.tx.lock().unwrap();
                    *g = Some((conn, ctx)); // the previous host's sender dropped: its writer ends
                    sh.flags.store(0, Ordering::SeqCst); // a new host: no extras until it asks
                }
                let (ftx, mtx, sh) = (ftx.clone(), mtx.clone(), sh.clone());
                std::thread::spawn(move || serve(s, w, crx, conn, &ftx, &mtx, &sh));
            }
        });
        Ok(ObLink {
            frames: frx,
            midi: mrx,
            shared,
            addr: local,
            outstanding: TARGET,
            conn: 0,
            connected: false,
            underruns: 0,
            midi_unrouted_logged: false,
        })
    }

    /// A new host (connection `c`): its writer opens with N TARGET, and nothing asked of an earlier host will
    /// come. Not `connected` (no underruns counted) until its first frames.
    fn new_conn(&mut self, c: u64) {
        self.conn = c;
        self.outstanding = TARGET;
        self.connected = false;
    }

    /// Send `m` to the connected host's writer; false without a host, or with its queue full (counted).
    fn send(&self, m: Vec<u8>) -> bool {
        self.shared.send(m)
    }

    /// The address the link listens on (port 0 in `listen` picks a free one).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Frames received since the last call (appended to `q`); asks the host for
    /// more so that `q` plus what is in flight stays near TARGET.
    pub fn poll(&mut self, q: &mut VecDeque<Frame>) {
        let c = self.shared.conns.load(Ordering::SeqCst);
        if c > self.conn {
            self.new_conn(c);
        }
        let mut got = false;
        // At most frame_cap held here; the rest wait in the (bounded) channel.
        while q.len() < self.shared.frame_cap {
            let Ok((c, f)) = self.frames.try_recv() else { break };
            if c < self.conn {
                continue; // an earlier host's: nothing was asked of it any more
            }
            if c > self.conn {
                self.new_conn(c); // a host newer than the count read above
            }
            q.push_back(f);
            self.outstanding = self.outstanding.saturating_sub(1);
            got = true;
        }
        // Connected only while this host's writer is installed: frames a host sent before it left are played,
        // but no underruns are counted for it.
        if !self.shared.current(self.conn) {
            self.connected = false;
        } else if got {
            self.connected = true;
        }
        let have = q.len() + self.outstanding;
        if have < TARGET {
            let want = TARGET - have;
            let mut m = vec![b'N'];
            m.extend((want as u32).to_le_bytes());
            if self.send(m) {
                self.outstanding += want;
            }
        }
    }

    /// Rytm -> host frames (the Overbridge IN stream), unless the host's hello declined them.
    pub fn send_rytm(&self, frames: &[Frame]) {
        if frames.is_empty() || self.shared.flags.load(Ordering::SeqCst) & HELLO_NO_R != 0 {
            return;
        }
        let mut m = Vec::with_capacity(5 + frames.len() * CHANNELS * 4);
        m.push(b'R');
        m.extend((frames.len() as u32).to_le_bytes());
        for f in frames {
            for x in f {
                m.extend(x.to_le_bytes());
            }
        }
        let _ = self.send(m);
    }

    /// A trig on `voice` (note 0..127, velocity 0..127).
    pub fn send_trig(&self, voice: u8, note: u8, velocity: u8) {
        let _ = self.send(vec![b'T', voice, note, velocity]);
    }

    /// The EXT machine's eight parameters on `voice`.
    pub fn send_params(&self, voice: u8, p: [u8; 8]) {
        let mut m = vec![b'P', voice];
        m.extend(p);
        let _ = self.send(m);
    }

    /// The Rytm's USB MIDI output to the host, if its hello asked for it.
    pub fn send_midi(&self, bytes: &[u8]) {
        self.shared.send_midi(bytes)
    }

    /// The host's MIDI for the Rytm's USB MIDI input since the last call (whole messages, running status
    /// expanded), until at least `max` bytes are taken; the rest waits for the next call.
    pub fn take_midi(&mut self, max: usize) -> Vec<u8> {
        let mut v = Vec::new();
        while v.len() < max {
            let Ok(m) = self.midi.try_recv() else { break };
            self.shared.midi_in_flight.fetch_sub(m.len(), Ordering::SeqCst);
            v.extend(m);
        }
        v
    }

    /// MIDI bytes sent to the host, and received from it, since the start.
    pub fn midi_counts(&self) -> (u64, u64) {
        (self.shared.midi_to_host.load(Ordering::Relaxed), self.shared.midi_from_host.load(Ordering::Relaxed))
    }

    /// What the bounded queues dropped since the start.
    pub fn dropped(&self) -> Dropped {
        Dropped {
            to_host: self.shared.dropped_to_host.load(Ordering::Relaxed),
            frames: self.shared.dropped_frames.load(Ordering::Relaxed),
            midi: self.shared.dropped_midi.load(Ordering::Relaxed),
            midi_bytes: self.shared.dropped_midi_bytes.load(Ordering::Relaxed),
        }
    }

    /// The flags granted to the connected host (0: none, or no hello).
    pub fn granted(&self) -> u8 {
        self.shared.flags.load(Ordering::SeqCst)
    }
}

impl Shared {
    /// Never blocks: a host that stopped reading fills its writer's queue, and what comes then is dropped.
    fn send(&self, m: Vec<u8>) -> bool {
        let g = self.tx.lock().unwrap();
        let Some((_, t)) = g.as_ref() else { return false };
        Self::try_send(t, m, &self.dropped_to_host)
    }

    fn try_send(t: &mpsc::SyncSender<Vec<u8>>, m: Vec<u8>, dropped: &AtomicU64) -> bool {
        match t.try_send(m) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(_)) => {
                dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(mpsc::TrySendError::Disconnected(_)) => false, // its writer died: the connection is closing
        }
    }

    fn send_midi(&self, bytes: &[u8]) {
        if bytes.is_empty() || self.flags.load(Ordering::SeqCst) & HELLO_MIDI == 0 {
            return;
        }
        // A u16 length: a longer run (a long SysEx) goes in several M messages; the host reads M as a byte stream.
        for c in bytes.chunks(65535) {
            let mut m = vec![b'M'];
            m.extend((c.len() as u16).to_le_bytes());
            m.extend(c);
            if self.send(m) {
                self.midi_to_host.fetch_add(c.len() as u64, Ordering::Relaxed);
            }
        }
    }

    fn current(&self, conn: u64) -> bool {
        self.tx.lock().unwrap().as_ref().is_some_and(|(c, _)| *c == conn)
    }

    /// Host `conn`'s hello: the answer queued first, then the grant stored, under one lock, so nothing the
    /// grant lets out (M) is queued ahead of the answer. Nothing for a host already replaced.
    fn hello(&self, conn: u64, granted: u8) {
        let g = self.tx.lock().unwrap();
        let Some((c, t)) = g.as_ref() else { return };
        if *c == conn {
            Self::try_send(t, vec![b'H', 1, granted], &self.dropped_to_host);
            self.flags.store(granted, Ordering::SeqCst);
        }
    }

    /// Host `conn` left: its writer ends and its grant goes, unless a newer host has the link already.
    fn disconnect(&self, conn: u64) {
        let mut g = self.tx.lock().unwrap();
        if g.as_ref().is_some_and(|(c, _)| *c == conn) {
            *g = None;
            self.flags.store(0, Ordering::SeqCst);
        }
    }

    fn drop_midi(&self, msgs: u64, bytes: u64) {
        self.dropped_midi.fetch_add(msgs, Ordering::Relaxed);
        self.dropped_midi_bytes.fetch_add(bytes, Ordering::Relaxed);
    }
}

/// One host connection: requests out on a writer thread, audio and the rest in on this one.
fn serve(
    s: TcpStream,
    mut w: TcpStream,
    crx: mpsc::Receiver<Vec<u8>>,
    conn: u64,
    out: &mpsc::SyncSender<(u64, Frame)>,
    midi: &mpsc::SyncSender<Vec<u8>>,
    sh: &Shared,
) {
    let _ = w.set_write_timeout(Some(sh.write_timeout));
    let writer = std::thread::spawn(move || {
        let mut r = w.write_all(&b"N".iter().copied().chain((TARGET as u32).to_le_bytes()).collect::<Vec<u8>>());
        // Ends when this connection's sender is dropped (the host left or was replaced) or a write fails.
        while r.is_ok() {
            let Ok(m) = crx.recv() else { return };
            r = w.write_all(&m);
        }
        // A host that stopped reading (or a dead socket): drop the connection, so the reader ends too.
        eprintln!("overbridge link: host not reading (write failed or timed out), dropped");
        let _ = w.shutdown(std::net::Shutdown::Both);
    });
    let closer = s.try_clone();
    let res = read_host(s, conn, out, midi, sh);
    sh.disconnect(conn); // this host's writer ends; nothing queues for the next
    if let Ok(c) = closer {
        let _ = c.shutdown(std::net::Shutdown::Both); // the writer thread holds a clone: close for real
    }
    if res.is_err() {
        eprintln!("overbridge link: host disconnected");
    }
    drop(writer);
}

/// Data bytes after a status byte (0 for one without, or not a status).
fn data_len(status: u8) -> usize {
    match status {
        0xC0..=0xDF | 0xF1 | 0xF3 => 1,
        0x80..=0xEF | 0xF2 => 2,
        _ => 0,
    }
}

/// The host's M bytes as a stream, per connection: whole messages out, with a SysEx and a running status carried
/// across M messages (io.rs `usb_midi_send` parses each call on its own).
#[derive(Default)]
struct MidiIn {
    /// The message being built (a SysEx from its F0; empty between messages).
    msg: Vec<u8>,
    /// Data bytes the message being built takes.
    need: usize,
    running: u8,
    sysex: bool,
    /// Bytes of a SysEx beyond SYSEX_MAX (it is dropped at its end).
    over: usize,
    /// Messages and bytes dropped since the caller last took them: a SysEx too long, or a message cut short.
    dropped: (u64, u64),
}

impl MidiIn {
    fn feed(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        for &b in bytes {
            if b >= 0xF8 {
                out.push(b); // real time: anywhere, even inside a SysEx
            } else if b == 0xF7 && self.sysex {
                if self.over > 0 {
                    self.over += 1;
                    self.cut();
                } else {
                    self.msg.push(b);
                    out.append(&mut self.msg);
                    self.sysex = false;
                }
            } else if b >= 0x80 {
                self.cut(); // a status byte ends an unfinished message
                self.running = if b < 0xF0 { b } else { 0 };
                self.need = data_len(b);
                match b {
                    0xF0 => {
                        self.sysex = true;
                        self.msg.push(b);
                    }
                    0xF4 | 0xF5 | 0xF7 => {} // undefined, or an EOX outside a SysEx: ignored
                    _ if self.need == 0 => out.push(b), // F6
                    _ => self.msg.push(b),
                }
            } else if self.sysex {
                if self.msg.len() < SYSEX_MAX {
                    self.msg.push(b);
                } else {
                    self.over += 1;
                }
            } else {
                if self.msg.is_empty() {
                    if self.running == 0 {
                        continue; // a data byte without a status: ignored
                    }
                    self.msg.push(self.running);
                    self.need = data_len(self.running);
                }
                self.msg.push(b);
                if self.msg.len() == 1 + self.need {
                    out.append(&mut self.msg);
                }
            }
        }
    }

    /// Drop the message being built (counted).
    fn cut(&mut self) {
        if !self.msg.is_empty() {
            self.dropped.0 += 1;
            self.dropped.1 += (self.msg.len() + self.over) as u64;
        }
        self.msg.clear();
        self.over = 0;
        self.sysex = false;
    }
}

fn read_host(
    mut s: TcpStream,
    conn: u64,
    out: &mpsc::SyncSender<(u64, Frame)>,
    midi: &mpsc::SyncSender<Vec<u8>>,
    sh: &Shared,
) -> std::io::Result<()> {
    let mut k = [0u8; 1];
    // This host's own grant (the shared flags may already be a newer host's) and MIDI parser.
    let mut granted = 0u8;
    let mut parser = MidiIn::default();
    loop {
        s.read_exact(&mut k)?;
        match k[0] {
            b'A' | b'a' => {
                let mut n4 = [0u8; 4];
                s.read_exact(&mut n4)?;
                let n = u32::from_le_bytes(n4) as usize;
                let voices = k[0] == b'a';
                if n > MAX_FRAMES || (voices && granted & HELLO_VOICES == 0) {
                    return Err(std::io::Error::other("bad frame message"));
                }
                let per = if voices { 16 } else { CHANNELS * 4 };
                let mut buf = vec![0u8; n * per];
                s.read_exact(&mut buf)?;
                for f in buf.chunks_exact(per) {
                    let mut fr = [0i32; CHANNELS];
                    if voices {
                        for (v, b) in f.as_chunks::<2>().0.iter().enumerate() {
                            fr[VOICE_CH[v]] = (i16::from_le_bytes([b[0], b[1]]) as i32) << 16;
                        }
                    } else {
                        for (c, b) in f.as_chunks::<4>().0.iter().enumerate() {
                            fr[c] = i32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                        }
                    }
                    match out.try_send((conn, fr)) {
                        Ok(()) => {}
                        Err(mpsc::TrySendError::Full(_)) => {
                            sh.dropped_frames.fetch_add(1, Ordering::Relaxed); // more than was asked for
                        }
                        Err(mpsc::TrySendError::Disconnected(_)) => return Ok(()),
                    }
                }
            }
            b'H' => {
                let mut h = [0u8; 2];
                s.read_exact(&mut h)?;
                granted = if h[0] == 1 { h[1] & GRANTED } else { 0 };
                eprintln!("overbridge link: host hello, flags {granted:#x} (MIDI {}, R {}, voice frames {})",
                    granted & HELLO_MIDI != 0, granted & HELLO_NO_R == 0, granted & HELLO_VOICES != 0);
                sh.hello(conn, granted);
            }
            b'M' => {
                if granted & HELLO_MIDI == 0 {
                    return Err(std::io::Error::other("M without hello"));
                }
                let mut l2 = [0u8; 2];
                s.read_exact(&mut l2)?;
                let mut b = vec![0u8; u16::from_le_bytes(l2) as usize];
                s.read_exact(&mut b)?;
                let mut m = Vec::new();
                parser.feed(&b, &mut m);
                let (dm, db) = std::mem::take(&mut parser.dropped);
                sh.drop_midi(dm, db);
                if m.is_empty() {
                    continue;
                }
                if !sh.current(conn) {
                    return Ok(()); // a newer host has the link
                }
                let n = m.len();
                // Bytes in flight claimed before the send (take_midi may take them at once), given back if dropped.
                if sh.midi_in_flight.fetch_add(n, Ordering::SeqCst) + n > MIDI_BYTES_CAP {
                    sh.midi_in_flight.fetch_sub(n, Ordering::SeqCst);
                    sh.drop_midi(1, n as u64);
                    continue;
                }
                match midi.try_send(m) {
                    Ok(()) => {
                        sh.midi_from_host.fetch_add(n as u64, Ordering::Relaxed);
                    }
                    Err(e) => {
                        sh.midi_in_flight.fetch_sub(n, Ordering::SeqCst);
                        if let mpsc::TrySendError::Disconnected(_) = e {
                            return Ok(());
                        }
                        sh.drop_midi(1, n as u64);
                    }
                }
            }
            _ => return Err(std::io::Error::other("bad message")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn connect(l: &ObLink) -> TcpStream {
        let s = TcpStream::connect(l.local_addr()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        s
    }

    fn read_n(s: &mut TcpStream, n: usize) -> Vec<u8> {
        let mut b = vec![0u8; n];
        s.read_exact(&mut b).unwrap();
        b
    }

    fn frames(l: &mut ObLink, n: usize) -> VecDeque<Frame> {
        let mut q = VecDeque::new();
        let t = Instant::now();
        while q.len() < n && t.elapsed() < Duration::from_secs(2) {
            l.poll(&mut q);
            std::thread::sleep(Duration::from_millis(2));
        }
        q
    }

    #[test]
    fn old_host_sees_todays_stream() {
        let mut l = ObLink::listen("127.0.0.1:0").unwrap();
        let mut s = connect(&l);
        assert_eq!(read_n(&mut s, 5), [b'N', 0, 2, 0, 0]); // 512 frames
        let mut m = vec![b'A'];
        m.extend(2u32.to_le_bytes());
        for i in 0..24i32 {
            m.extend(i.to_le_bytes());
        }
        s.write_all(&m).unwrap();
        let q = frames(&mut l, 2);
        assert_eq!(q.len(), 2);
        assert_eq!(q[1][0], 12);
        assert!(l.connected);
        l.poll(&mut VecDeque::new()); // the 2 frames played: asked for again
        assert_eq!(read_n(&mut s, 5), [b'N', 2, 0, 0, 0]);
        l.send_midi(&[0xF0, 0x7D, 0xF7]); // never asked for: not sent
        l.send_trig(1, 60, 100);
        assert_eq!(read_n(&mut s, 4), [b'T', 1, 60, 100]);
    }

    #[test]
    fn hello_midi_both_ways() {
        let mut l = ObLink::listen("127.0.0.1:0").unwrap();
        let mut s = connect(&l);
        read_n(&mut s, 5);
        s.write_all(&[b'H', 1, HELLO_MIDI | 0x80]).unwrap(); // an unknown bit is not granted
        assert_eq!(read_n(&mut s, 3), [b'H', 1, HELLO_MIDI]);
        l.send_midi(&[0xF0, 0x7D, 0x52, 0x54, 0x10, b'A', 0xF7]);
        assert_eq!(read_n(&mut s, 10), [b'M', 7, 0, 0xF0, 0x7D, 0x52, 0x54, 0x10, b'A', 0xF7]);
        s.write_all(&[b'M', 3, 0, 0x90, 36, 100]).unwrap();
        let t = Instant::now();
        let mut got = Vec::new();
        while got.is_empty() && t.elapsed() < Duration::from_secs(2) {
            got = l.take_midi(usize::MAX);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(got, [0x90, 36, 100]);
        assert_eq!(l.midi_counts(), (7, 3));
    }

    #[test]
    fn voice_frames_and_no_r() {
        let mut l = ObLink::listen("127.0.0.1:0").unwrap();
        let mut s = connect(&l);
        read_n(&mut s, 5);
        s.write_all(&[b'H', 1, HELLO_NO_R | HELLO_VOICES]).unwrap();
        assert_eq!(read_n(&mut s, 3), [b'H', 1, HELLO_NO_R | HELLO_VOICES]);
        let mut m = vec![b'a'];
        m.extend(1u32.to_le_bytes());
        for v in 0..8i16 {
            m.extend((100 * (v + 1)).to_le_bytes());
        }
        s.write_all(&m).unwrap();
        let q = frames(&mut l, 1);
        assert_eq!(q.len(), 1);
        for v in 0..8 {
            assert_eq!(q[0][VOICE_CH[v]], (100 * (v as i32 + 1)) << 16);
        }
        assert_eq!(q[0][0], 0);
        l.send_rytm(&[[7; CHANNELS]]); // not wanted: nothing on the wire
        s.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        let mut b = [0u8; 1];
        assert!(s.read_exact(&mut b).is_err());
    }

    fn dropped(s: &mut TcpStream) -> bool {
        // The reader drops the connection: reads end (EOF or reset), never time out.
        let mut b = [0u8; 1];
        loop {
            match s.read(&mut b) {
                Ok(0) => return true,
                Ok(_) => continue,
                Err(e) => return e.kind() != std::io::ErrorKind::WouldBlock && e.kind() != std::io::ErrorKind::TimedOut,
            }
        }
    }

    #[test]
    fn oversize_frame_count_drops_host() {
        let mut l = ObLink::listen("127.0.0.1:0").unwrap();
        let mut s = connect(&l);
        read_n(&mut s, 5);
        let mut m = vec![b'A'];
        m.extend(u32::MAX.to_le_bytes());
        s.write_all(&m).unwrap();
        assert!(dropped(&mut s));
        assert!(frames(&mut l, 1).is_empty());
    }

    #[test]
    fn m_and_a_need_their_grant() {
        for msg in [vec![b'M', 1, 0, 0x90], vec![b'a', 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]] {
            let mut l = ObLink::listen("127.0.0.1:0").unwrap();
            let mut s = connect(&l);
            read_n(&mut s, 5);
            s.write_all(&msg).unwrap();
            assert!(dropped(&mut s));
            assert!(l.take_midi(usize::MAX).is_empty());
            assert!(frames(&mut l, 1).is_empty());
        }
    }

    #[test]
    fn a_new_host_gets_every_request() {
        // The previous host's writer must not take (and lose) a message meant for the next one.
        let mut l = ObLink::listen("127.0.0.1:0").unwrap();
        for round in 0..3u8 {
            let mut s = connect(&l);
            assert_eq!(read_n(&mut s, 5), [b'N', 0, 2, 0, 0]);
            for i in 0..4u8 {
                l.send_trig(round, i, 100);
            }
            for i in 0..4u8 {
                assert_eq!(read_n(&mut s, 4), [b'T', round, i, 100]);
            }
            drop(s);
            // Wait until the link has let this host go, then also poll (requests while no host are dropped).
            let t = Instant::now();
            while l.shared.tx.lock().unwrap().is_some() && t.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(2));
            }
            let mut q = VecDeque::new();
            l.poll(&mut q);
        }
    }

    fn wait(mut f: impl FnMut() -> bool) -> bool {
        let t = Instant::now();
        while !f() {
            if t.elapsed() > Duration::from_secs(5) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        true
    }

    fn a_msg(n: u32) -> Vec<u8> {
        let mut m = vec![b'A'];
        m.extend(n.to_le_bytes());
        m.extend(vec![0u8; n as usize * CHANNELS * 4]);
        m
    }

    #[test]
    fn a_stalled_host_drops_not_queues() {
        // A host that never reads: its writer blocks, the queue fills, and what comes then is dropped and
        // counted; sending never blocks.
        let l = ObLink::listen_caps("127.0.0.1:0", 4, FRAME_CAP, WRITE_TIMEOUT).unwrap();
        let _s = connect(&l);
        assert!(wait(|| l.shared.tx.lock().unwrap().is_some()));
        let big = vec![[0x55; CHANNELS]; 4096]; // 196 KB an R
        let t = Instant::now();
        for _ in 0..256 {
            l.send_rytm(&big);
        }
        assert!(t.elapsed() < Duration::from_secs(5));
        let d = l.dropped();
        assert!(d.to_host > 0, "{d:?}");
        assert!(d.to_host <= 256);
    }

    #[test]
    fn a_reading_host_loses_nothing() {
        let l = ObLink::listen("127.0.0.1:0").unwrap();
        let mut s = connect(&l);
        read_n(&mut s, 5);
        let r = std::thread::spawn(move || {
            for i in 0..2000u32 {
                assert_eq!(read_n(&mut s, 4), [b'T', (i % 8) as u8, (i % 128) as u8, 100]);
            }
        });
        for i in 0..2000u32 {
            l.send_trig((i % 8) as u8, (i % 128) as u8, 100);
        }
        r.join().unwrap();
        assert_eq!(l.dropped(), Dropped::default());
    }

    #[test]
    fn frames_beyond_the_cap_are_dropped() {
        // A host sending far more than it was asked for: the channel and the caller's queue hold frame_cap each.
        let mut l = ObLink::listen_caps("127.0.0.1:0", TO_HOST_CAP, 16, WRITE_TIMEOUT).unwrap();
        let mut s = connect(&l);
        read_n(&mut s, 5);
        s.write_all(&a_msg(1000)).unwrap();
        assert!(wait(|| l.dropped().frames == 1000 - 16));
        let mut q = VecDeque::new();
        l.poll(&mut q);
        assert_eq!(q.len(), 16);
        l.poll(&mut q); // the queue is full: nothing more taken
        assert_eq!(q.len(), 16);
    }

    #[test]
    fn an_old_hosts_frames_do_not_reach_the_next() {
        let mut l = ObLink::listen("127.0.0.1:0").unwrap();
        let mut s1 = connect(&l);
        read_n(&mut s1, 5);
        s1.write_all(&a_msg(100)).unwrap(); // never polled while this host is there
        drop(s1);
        let mut s2 = connect(&l);
        assert_eq!(read_n(&mut s2, 5), [b'N', 0, 2, 0, 0]);
        assert!(wait(|| l.shared.conns.load(Ordering::SeqCst) == 2));
        let mut q = VecDeque::new();
        l.poll(&mut q);
        assert!(q.is_empty()); // the first host's 100 frames dropped
        assert!(!l.connected);
        // The new host's N 512 is still all outstanding: no further N.
        s2.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        let mut b = [0u8; 1];
        assert!(s2.read_exact(&mut b).is_err());
        // Its own frames count.
        s2.write_all(&a_msg(3)).unwrap();
        let q = frames(&mut l, 3);
        assert_eq!(q.len(), 3);
        assert!(l.connected);
    }

    #[test]
    fn a_fast_reconnect_is_not_connected_until_it_sends() {
        let mut l = ObLink::listen("127.0.0.1:0").unwrap();
        let mut s1 = connect(&l);
        read_n(&mut s1, 5);
        s1.write_all(&a_msg(2)).unwrap();
        assert_eq!(frames(&mut l, 2).len(), 2);
        assert!(l.connected);
        drop(s1);
        let mut s2 = connect(&l);
        read_n(&mut s2, 5);
        // The link has the new host before this poll sees the old one gone: still not connected.
        assert!(wait(|| l.shared.conns.load(Ordering::SeqCst) == 2));
        l.poll(&mut VecDeque::new());
        assert!(!l.connected);
        s2.write_all(&a_msg(1)).unwrap();
        assert_eq!(frames(&mut l, 1).len(), 1);
        assert!(l.connected);
    }

    #[test]
    fn midi_to_host_counts_only_what_was_sent() {
        let l = ObLink::listen("127.0.0.1:0").unwrap();
        let mut s = connect(&l);
        read_n(&mut s, 5);
        s.write_all(&[b'H', 1, HELLO_MIDI]).unwrap();
        assert_eq!(read_n(&mut s, 3), [b'H', 1, HELLO_MIDI]);
        l.send_midi(&[0x90, 36, 100]);
        assert_eq!(read_n(&mut s, 6), [b'M', 3, 0, 0x90, 36, 100]);
        drop(s);
        assert!(wait(|| l.shared.tx.lock().unwrap().is_none()));
        l.send_midi(&[0x80, 36, 0]); // no host: not sent, not counted
        assert_eq!(l.midi_counts().0, 3);
    }

    #[test]
    fn a_long_sysex_is_split_across_m() {
        let l = ObLink::listen("127.0.0.1:0").unwrap();
        let mut s = connect(&l);
        read_n(&mut s, 5);
        s.write_all(&[b'H', 1, HELLO_MIDI]).unwrap();
        read_n(&mut s, 3);
        let mut sx = vec![0xF0];
        sx.extend((0..70000).map(|i| (i % 128) as u8));
        sx.push(0xF7);
        l.send_midi(&sx);
        let mut got = Vec::new();
        for want in [65535usize, sx.len() - 65535] {
            let h = read_n(&mut s, 3);
            assert_eq!(h[0], b'M');
            assert_eq!(u16::from_le_bytes([h[1], h[2]]) as usize, want);
            got.extend(read_n(&mut s, want));
        }
        assert_eq!(got, sx);
        assert_eq!(l.midi_counts().0, sx.len() as u64);
    }

    fn hello_midi(s: &mut TcpStream) {
        read_n(s, 5);
        s.write_all(&[b'H', 1, HELLO_MIDI]).unwrap();
        assert_eq!(read_n(s, 3), [b'H', 1, HELLO_MIDI]);
    }

    fn sysex_m(n: usize) -> Vec<u8> {
        let mut m = vec![b'M'];
        m.extend(((n + 2) as u16).to_le_bytes());
        m.push(0xF0);
        m.extend((0..n).map(|i| (i % 128) as u8));
        m.push(0xF7);
        m
    }

    #[test]
    fn host_midi_is_bounded_by_bytes() {
        // A Rytm not taking the host's MIDI: what is in flight stays under MIDI_BYTES_CAP, the rest is dropped and
        // counted (bytes too), and take_midi gives at most what it is asked for (plus one batch).
        let mut l = ObLink::listen("127.0.0.1:0").unwrap();
        let mut s = connect(&l);
        hello_midi(&mut s);
        let one = 60000 + 2;
        for _ in 0..5 {
            s.write_all(&sysex_m(60000)).unwrap(); // 4 fit in 256 KB, the 5th does not
        }
        assert!(wait(|| l.dropped().midi == 1));
        assert_eq!(l.dropped().midi_bytes, one as u64);
        assert_eq!(l.shared.midi_in_flight.load(Ordering::SeqCst), 4 * one);
        assert_eq!(l.midi_counts().1, 4 * one as u64); // only what was queued for the Rytm
        assert_eq!(l.take_midi(0).len(), 0);
        assert_eq!(l.take_midi(1).len(), one);
        assert_eq!(l.shared.midi_in_flight.load(Ordering::SeqCst), 3 * one);
        assert_eq!(l.take_midi(usize::MAX).len(), 3 * one);
        assert_eq!(l.shared.midi_in_flight.load(Ordering::SeqCst), 0);
        // Room again: the next one is taken.
        s.write_all(&sysex_m(10)).unwrap();
        assert!(wait(|| l.shared.midi_in_flight.load(Ordering::SeqCst) == 12));
        assert_eq!(l.take_midi(usize::MAX).len(), 12);
    }

    #[test]
    fn a_newer_host_replaces_a_stalled_one() {
        // A host that stops (no FIN: here one that just sits there) never holds the link.
        let mut l = ObLink::listen("127.0.0.1:0").unwrap();
        let mut s1 = connect(&l);
        hello_midi(&mut s1);
        let mut s2 = connect(&l);
        assert_eq!(read_n(&mut s2, 5), [b'N', 0, 2, 0, 0]);
        assert!(dropped(&mut s1));
        assert!(wait(|| l.shared.conns.load(Ordering::SeqCst) == 2));
        assert_eq!(l.granted(), 0); // the first host's grant is not the second's
        l.send_trig(3, 60, 100);
        assert_eq!(read_n(&mut s2, 4), [b'T', 3, 60, 100]);
        s2.write_all(&a_msg(2)).unwrap();
        assert_eq!(frames(&mut l, 2).len(), 2);
        assert!(l.connected);
    }

    #[test]
    fn a_host_that_stops_reading_is_dropped() {
        // Its writer blocks; after the write timeout the connection goes, and the link is free.
        let l = ObLink::listen_caps("127.0.0.1:0", 4, FRAME_CAP, Duration::from_millis(200)).unwrap();
        let _s = connect(&l);
        assert!(wait(|| l.shared.tx.lock().unwrap().is_some()));
        let big = vec![[0x55; CHANNELS]; 4096];
        assert!(wait(|| {
            l.send_rytm(&big);
            l.shared.tx.lock().unwrap().is_none()
        }));
        let mut s2 = connect(&l);
        assert_eq!(read_n(&mut s2, 5), [b'N', 0, 2, 0, 0]);
    }

    #[test]
    fn the_hello_answer_comes_before_any_m() {
        // The Rytm's MIDI going out all the time: the first thing after N is still H.
        for _ in 0..20 {
            let l = ObLink::listen("127.0.0.1:0").unwrap();
            let mut s = connect(&l);
            read_n(&mut s, 5);
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (sh, st) = (l.shared.clone(), stop.clone());
            let t = std::thread::spawn(move || {
                while !st.load(Ordering::SeqCst) {
                    sh.send_midi(&[0xF8]);
                    std::thread::yield_now();
                }
            });
            s.write_all(&[b'H', 1, HELLO_MIDI]).unwrap();
            assert_eq!(read_n(&mut s, 3), [b'H', 1, HELLO_MIDI]);
            assert_eq!(read_n(&mut s, 4), [b'M', 1, 0, 0xF8]);
            stop.store(true, Ordering::SeqCst);
            t.join().unwrap();
        }
    }

    #[test]
    fn a_gone_host_leaves_no_grant_and_no_connection() {
        let mut l = ObLink::listen("127.0.0.1:0").unwrap();
        let mut s = connect(&l);
        read_n(&mut s, 5);
        s.write_all(&[b'H', 1, GRANTED]).unwrap();
        read_n(&mut s, 3);
        assert_eq!(l.granted(), GRANTED);
        s.write_all(&a_msg(3)).unwrap();
        drop(s);
        assert!(wait(|| l.shared.tx.lock().unwrap().is_none()));
        assert_eq!(l.granted(), 0);
        let mut q = VecDeque::new();
        l.poll(&mut q);
        assert_eq!(q.len(), 3); // its last frames are played,
        assert!(!l.connected); // but it is not connected
    }

    fn parse(chunks: &[&[u8]]) -> (Vec<u8>, (u64, u64)) {
        let mut p = MidiIn::default();
        let mut out = Vec::new();
        for c in chunks {
            p.feed(c, &mut out);
        }
        (out, p.dropped)
    }

    #[test]
    fn host_midi_spans_m_messages() {
        // A SysEx split across M, with a clock inside it.
        assert_eq!(parse(&[&[0xF0, 0x7D, 1], &[2, 0xF8, 3], &[0xF7]]).0, [0xF8, 0xF0, 0x7D, 1, 2, 3, 0xF7]);
        // A running status, and a message, across M.
        assert_eq!(parse(&[&[0x90, 36], &[100, 38], &[90]]).0, [0x90, 36, 100, 0x90, 38, 90]);
        assert_eq!(parse(&[&[0xC0, 5], &[6]]).0, [0xC0, 5, 0xC0, 6]);
        // System common cancels the running status; a data byte without one is ignored.
        assert_eq!(parse(&[&[0x90, 36, 100, 0xF6, 38, 90]]).0, [0x90, 36, 100, 0xF6]);
        // A message cut by a status byte is dropped and counted.
        assert_eq!(parse(&[&[0x90, 36], &[0x80, 36, 0]]), (vec![0x80, 36, 0], (1, 2)));
        // A SysEx longer than SYSEX_MAX is dropped and counted, whole; the next one passes.
        let mut long = vec![0xF0];
        long.extend(vec![1u8; SYSEX_MAX]);
        long.push(0xF7);
        assert_eq!(parse(&[&long, &[0xF0, 2, 0xF7]]), (vec![0xF0, 2, 0xF7], (1, long.len() as u64)));
    }

    #[test]
    fn binds_every_address() {
        let l = ObLink::listen("0.0.0.0:0").unwrap();
        assert!(l.local_addr().ip().is_unspecified());
    }
}
