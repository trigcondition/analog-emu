//! The emulator in a window, with sound: Analog Rytm MKII by default, or the Digitakt
//! or Analog Four MKII (pass its .syx). Built a second time as `a4` (src/bin/a4.rs),
//! which defaults to the A4 OS 1.56 and the card a4.img. The window draws the
//! loaded device's front panel (`gui::hw`; the Digitakt keeps the plain one).
//!
//!     rytm [FIRMWARE.syx] [--snapshot FILE] [--ips N] [--no-audio]
//!              [--headless --after SECS --screenshot OUT.png] [--press CODE@SECS[:HOLD]]
//!
//! With `--snapshot`, the window resumes from FILE when it exists (F5 saves
//! it, and it is saved again on exit); otherwise the firmware boots from
//! reset, intro and all, and a blank +Drive is formatted on first boot.
//!
//! The emulator runs on its own thread. With audio it is paced by the sound
//! card: it renders ahead until about 60 ms are buffered and then waits, so
//! emulated time follows the card's clock. Without audio it follows the wall
//! clock.

use dtemu::firmware::Firmware;
use dtemu::gui::{self, Canvas, Control, View};
use dtemu::machine::Machine;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Whether this is the `a4` binary (src/bin/a4.rs includes this file as a module).
fn a4_bin() -> bool {
    env!("CARGO_BIN_NAME") == "a4"
}

/// The firmware the window boots when none is given.
fn default_fw() -> &'static str {
    if a4_bin() { "fw/Analog-Four_MKII_OS1.56.syx" } else { "fw/Analog-Rytm_MKII_OS1.73.syx" }
}

enum Ev {
    Key(u8, bool),
    /// A pad struck at a force (0..255): where on the pad it was clicked.
    PadDown(u8, u8),
    /// A held pad's pressure (0..255), following the pointer.
    Pressure(u8, u8),
    /// A MIDI message from the virtual input port, for the DIN MIDI input.
    Midi(Vec<u8>),
    /// A MIDI message for the USB MIDI port ("rytm-emu usb in").
    UsbMidi(Vec<u8>),
    Turn(u8, i8),
    /// `steps` value steps on an encoder (the arrow keys): Machine::nudge_steps.
    Nudge(u8, i8, u8),
    ReleaseAll,
    Save,
}

struct Audio {
    ring: Mutex<VecDeque<f32>>,
    /// Underruns heard: the ring ran dry while the output was not silent.
    dropouts: AtomicU64,
    rate: AtomicU64,
    live: AtomicBool,
    /// Set once the emulator first fills the ring; until then the output
    /// plays silence and counts no dropouts.
    primed: AtomicBool,
    /// The host's audio input, resampled to 48 kHz stereo, for the Rytm's
    /// AUDIO IN / EXT IN (`--audio-in`).
    input: Mutex<VecDeque<(f32, f32)>>,
}

struct Opts {
    syx: PathBuf,
    /// A FIRMWARE.syx was named (otherwise --flash's OS boots, if it has one).
    syx_given: bool,
    /// --flash FILE: the SPI flash, loaded if the file exists and written back
    /// on save and exit, so an OS upgrade run in the window persists. When the
    /// firmware resets (the end of an upgrade) the window boots the flash's OS.
    flash: Option<PathBuf>,
    snapshot: Option<PathBuf>,
    ips: f64,
    audio: bool,
    headless: bool,
    after: f64,
    screenshot: Option<PathBuf>,
    presses: Vec<(u8, f64, f64)>,
    turns: Vec<(u8, i8, f64)>,
    card: Option<PathBuf>,
    audio_null: bool,
    wav: Option<PathBuf>,
    /// Audio kept buffered ahead of the output, in ms.
    latency: f64,
    /// Virtual MIDI ports "rytm-emu in" / "rytm-emu out" for the DIN MIDI port.
    midi: bool,
    /// --midi: the ports in --headless runs too (for tests).
    midi_headless: bool,
    /// --audio-in audio|ext|both: the host's default input into AUDIO IN
    /// (sampling, USB), EXT IN (the main mix) or both.
    audio_in: Option<String>,
    /// --midi-from NAME: an existing MIDI source into the DIN MIDI input.
    midi_from: Option<String>,
    /// --usb-wav FILE: the USB audio stream (12 channels), written on exit.
    usb_wav: Option<PathBuf>,
    /// --ob-link [ADDR]: serve the Overbridge input to a host (a simulated
    /// Teensy) over TCP (dtemu::oblink).
    ob_link: Option<String>,
    usb_host: bool,
    /// --usb-midi-tcp ADDR: the USB MIDI port as a raw MIDI byte stream over TCP (usbtcp), for
    /// tools on a host without virtual MIDI ports (Windows), and one address per emulator.
    usb_midi_tcp: Option<String>,
    /// --power-off: closing the window switches the Rytm off (Machine::power_off):
    /// the firmware saves its working state to the card, which is written
    /// back; no snapshot is saved, so the next start is a cold boot from it.
    power_off: bool,
}

fn parse() -> Opts {
    let mut o = Opts {
        syx: PathBuf::from(default_fw()),
        syx_given: false,
        flash: None,
        snapshot: None,
        ips: 200e6,
        audio: true,
        headless: false,
        after: 5.0,
        screenshot: None,
        presses: Vec::new(),
        turns: Vec::new(),
        card: a4_bin().then(|| PathBuf::from("a4.img")),
        audio_null: false,
        wav: None,
        latency: 60.0,
        midi: true,
        midi_headless: false,
        audio_in: None,
        midi_from: None,
        usb_wav: None,
        ob_link: None,
        usb_host: false,
        usb_midi_tcp: None,
        power_off: false,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--snapshot" => o.snapshot = it.next().map(PathBuf::from),
            "--ips" => o.ips = it.next().unwrap().replace('M', "e6").parse().expect("--ips"),
            "--no-audio" => o.audio = false,
            "--no-midi" => o.midi = false,
            "--midi-from" => o.midi_from = it.next().cloned(),
            "--usb-wav" => o.usb_wav = it.next().map(PathBuf::from),
            "--usb-host" => o.usb_host = true,
            "--usb-midi-tcp" => o.usb_midi_tcp = it.next().cloned(),
            "--ob-link" => {
                let next = it.clone().next().filter(|a| !a.starts_with("--")).cloned();
                if next.is_some() {
                    it.next();
                }
                o.ob_link = Some(next.unwrap_or_else(|| "127.0.0.1:7301".into()));
            }
            "--audio-in" => {
                let d = it.next().cloned().unwrap_or_default();
                if !["audio", "ext", "both"].contains(&d.as_str()) {
                    eprintln!("--audio-in takes audio, ext or both");
                    std::process::exit(2);
                }
                o.audio_in = Some(d);
            }
            "--midi" => o.midi_headless = true,
            "--audio-null" => o.audio_null = true,
            "--card" => o.card = it.next().map(PathBuf::from),
            "--flash" => o.flash = it.next().map(PathBuf::from),
            "--wav" => o.wav = it.next().map(PathBuf::from),
            "--latency" => o.latency = it.next().unwrap().parse().expect("--latency MS"),
            "--turn" => {
                let v = it.next().unwrap();
                let (ed, t) = v.split_once('@').expect("--turn ENC:DELTA@SECS");
                let (e, d) = ed.split_once(':').unwrap();
                o.turns.push((e.parse().unwrap(), d.parse().unwrap(), t.parse().unwrap()));
            }
            "--headless" => o.headless = true,
            "--power-off" => o.power_off = true,
            "--after" => o.after = it.next().unwrap().parse().expect("--after"),
            "--screenshot" => o.screenshot = it.next().map(PathBuf::from),
            "--press" => {
                let v = it.next().unwrap();
                let (c, t) = v.split_once('@').expect("--press CODE@SECS[:HOLD]");
                let (t, hold) = t.split_once(':').unwrap_or((t, "0.15"));
                o.presses.push((c.parse().unwrap(), t.parse().unwrap(), hold.parse().expect("--press HOLD")));
            }
            "-h" | "--help" => {
                let (bin, port) = if a4_bin() { ("a4", "a4-emu") } else { ("rytm", "rytm-emu") };
                let pad = " ".repeat(bin.len());
                println!(
                    "usage: {bin} [FIRMWARE.syx] [--snapshot FILE] [--card IMAGE] [--ips N] [--no-audio]\n\
                     \x20       {pad}[--headless --after SECS --screenshot OUT.png] [--press CODE@SECS[:HOLD]] [--turn ENC:DELTA@SECS]\n\
                     \x20       {pad}[--latency MS] [--audio-null] [--wav OUT.wav] [--no-midi]\n\
                     \x20       {pad}[--audio-in audio|ext|both]  (the Mac's default input into AUDIO IN / EXT IN)\n\
                     \x20       {pad}[--midi-from NAME] [--usb-host]  (an existing MIDI source, e.g. a real Rytm, into the DIN input)\n\
                     \x20       {pad}[--usb-wav FILE]  (record the USB audio stream: MAIN, 8 voices, AUDIO IN; 12 ch)\n\
                     \x20       {pad}[--ob-link [ADDR]]  (Overbridge input from a host over TCP, default 127.0.0.1:7301; 0.0.0.0:7301 for a host on the LAN)\n\
                     \x20       {pad}[--usb-midi-tcp ADDR]  (the USB MIDI port as a raw MIDI byte stream over TCP, e.g. 127.0.0.1:7321;\n\
                     \x20       {pad}                        headless too; one client at a time, the newest)\n\
                     \x20       {pad}[--flash FILE]  (the SPI flash, kept in FILE: an OS upgrade over USB MIDI persists;\n\
                     \x20       {pad}                 with no FIRMWARE named, the OS in it boots)\n\n\
                     Defaults: FIRMWARE {fw}, --card {card}.\n\
                     MIDI: virtual ports \"{port} in\" and \"{port} out\" carry the DIN MIDI port,\n\
                     \"{port} usb in\" and \"{port} usb out\" the USB MIDI port\n\
                     (not with --headless or --no-midi).",
                    fw = default_fw(),
                    card = if a4_bin() { "a4.img" } else { "none (a blank card)" },
                );
                std::process::exit(0);
            }
            _ => {
                o.syx = PathBuf::from(a);
                o.syx_given = true;
            }
        }
    }
    o
}

fn view_of(m: &Machine, status: String) -> View {
    let p = &m.cpu.bus.io.panel;
    View {
        rytm: m.is_rytm(),
        a4: m.is_a4(),
        mk1: m.is_mk1(),
        oled: p.oled.to_vec(),
        // The MKI's LEDs are single colour (red) [community: panel photos].
        leds: (0..dtemu::panel::ALL_LEDS).map(|i| if m.is_mk1() { p.led_mono(i).then_some((255, 48, 32)) } else { p.led_rgb(i) }).collect(),
        status,
    }
}

/// The emulator loop. Runs until `quit`.
fn emulate(
    mut m: Machine,
    o: &Opts,
    rx: mpsc::Receiver<Ev>,
    view: Arc<Mutex<View>>,
    audio: Arc<Audio>,
    quit: Arc<AtomicBool>,
) -> Machine {
    let ips = o.ips;
    m.cpu.bus.io.ips = ips;
    m.cpu.bus.io.usb_audio_capture = o.usb_wav.is_some();
    let ports = o.midi && (!o.headless || o.midi_headless);
    // DTEMU_MIDI_NAME renames the ports (default "rytm-emu", "a4-emu" on an A4,
    // "syntakt-emu" on a Syntakt),
    // so a second emulator can run beside a window without their ports colliding.
    let pname = std::env::var("DTEMU_MIDI_NAME").unwrap_or_else(|_| if m.is_a4() { "a4-emu" } else if m.is_syntakt() { "syntakt-emu" } else { "rytm-emu" }.into());
    let mut midi_out = if ports { midi::Out::start(&format!("{pname} out"), "out") } else { None };
    let mut usb_out = if ports && (m.is_rytm() || m.is_a4()) { midi::Out::start(&format!("{pname} usb out"), "uout") } else { None };
    let slice = (ips / 500.0) as u64; // 2 ms of emulated time
    let target_ms = o.latency;
    let mut t0 = Instant::now();
    let mut emu0 = m.now();
    let mut last_pub = Instant::now();
    let mut last_speed = (Instant::now(), m.now());
    let mut link_report = Instant::now();
    let mut speed = 0.0;
    let mut phase = 0.0f64;
    while !quit.load(Ordering::Relaxed) {
        if let Some(dest) = o.audio_in.as_deref() {
            // Host input into the codec's input queues; at most 100 ms queued,
            // so a slow emulation drops input rather than lagging behind it.
            let mut inq = audio.input.lock().unwrap();
            let ssi = &mut m.cpu.bus.io.ssi;
            for fr in inq.drain(..) {
                if dest != "ext" {
                    ssi.audio_in.push_back(fr);
                }
                if dest != "audio" {
                    ssi.ext_in.push_back(fr);
                }
            }
            for q in [&mut ssi.audio_in, &mut ssi.ext_in] {
                while q.len() > 4800 {
                    q.pop_front();
                }
            }
        }
        while let Ok(ev) = rx.try_recv() {
            match ev {
                Ev::Key(c, d) => m.key_compat(c, d),
                Ev::PadDown(c, force) => {
                    m.pad_force = force;
                    m.key_compat(c, true);
                }
                Ev::Pressure(c, v) => {
                    if let Some(p) = m.pad_of_code(c) {
                        m.pad_pressure(p, v);
                    }
                }
                Ev::Midi(b) => {
                    midi::log("in ", &b);
                    m.midi_send(&b)
                }
                Ev::UsbMidi(b) => {
                    midi::log("uin", &b);
                    m.usb_midi_send(&b)
                }
                Ev::Turn(e, d) => m.turn_detents(e, d),
                Ev::Nudge(e, d, n) => m.nudge_steps(e, d, n, false),
                Ev::ReleaseAll => m.release_all_keys(),
                Ev::Save => {
                    save_card(&mut m, o);
                    if let Some(p) = &o.snapshot {
                        match dtemu::snapshot::save(&m, p) {
                            Ok(()) => eprintln!("saved {}", p.display()),
                            Err(e) => eprintln!("save failed: {e}"),
                        }
                    }
                }
            }
        }
        let rate = audio.rate.load(Ordering::Relaxed) as f64;
        let live = audio.live.load(Ordering::Relaxed);
        if live {
            let buffered = audio.ring.lock().unwrap().len() as f64 / 2.0 / rate * 1000.0;
            if buffered > target_ms / 2.0 {
                audio.primed.store(true, Ordering::Relaxed);
            }
            if buffered > target_ms {
                std::thread::sleep(Duration::from_micros(500));
                continue;
            }
        } else {
            let emu_t = (m.now() - emu0) as f64 / ips;
            if emu_t > t0.elapsed().as_secs_f64() + 0.005 {
                std::thread::sleep(Duration::from_micros(500));
                continue;
            }
        }
        if let dtemu::cpu::Stop::Reset = m.run_until(m.now() + slice) {
            // The end of an OS upgrade: boot what is in the flash now.
            save_flash(&mut m, o);
            match m.reboot_from_flash() {
                Ok(name) => eprintln!("reset: booting {name} from the flash"),
                Err(e) => {
                    eprintln!("reset: {e}");
                    m.cpu.bus.io.reset_request = false;
                }
            }
            m.cpu.bus.io.ips = ips;
            m.cpu.bus.io.usb_audio_capture = o.usb_wav.is_some();
            t0 = Instant::now();
            emu0 = m.now();
            last_speed = (Instant::now(), m.now());
        }
        if link_report.elapsed() >= Duration::from_secs(10) {
            link_report = Instant::now();
            if let Some(s) = m.ob_link_report() {
                eprintln!("{s}");
            }
        }
        let bytes: Vec<u8> = m.cpu.bus.io.usb_midi_out.drain(..).collect();
        if let Some(uo) = usb_out.as_mut() {
            uo.send(&bytes);
        }
        usbtcp::send(&bytes);
        if let Some(mo) = midi_out.as_mut() {
            let bytes: Vec<u8> = m.cpu.bus.io.midi_out.drain(..).collect();
            mo.send(&bytes);
        } else {
            m.cpu.bus.io.midi_out.clear();
        }
        let frame_words = m.ssi_frame_words();
        // Audio out: 24-bit samples, stereo, at the SSI's 48 kHz.
        let out = &mut m.cpu.bus.io.ssi.out;
        // One stereo pair per codec frame: the Digitakt's frame is the pair; the
        // Rytm's 16-word frame is mixed below.
        let fw = frame_words;
        if live && out.len() >= fw {
            let mut ring = audio.ring.lock().unwrap();
            let step = 48_000.0 / rate;
            let words: Vec<i32> = out.drain(..out.len() - out.len() % fw).collect();
            // The A4 MKI too: its DAC words are the noise generator (TX 0..3)
            // and the effects' return, not the speakers' L/R (MK1-PORT.md 9).
            let a4 = m.is_a4() || m.prof.device == dtemu::symbols::Device::A4Mk1;
            let frames: Vec<(f32, f32)> = if fw == 16 || a4 || m.cpu.bus.io.ssi0.on {
                // The Rytm's output is analog: the model (analog.rs) runs each
                // voice's chain with its sample layer (DAC words 0..7), the tom
                // exciters (8..10) and the FX returns (12..15), and mixes it all,
                // one stereo pair per receive frame. The DAC words are its input,
                // not part of the speaker mix.
                // One model frame per codec frame: 16 words on the Rytm, 8 on
                // the A4 (chunking the A4's by 16 drained the queue at half
                // rate, so it grew to its 1 s cap and played every other frame).
                let voices = &mut m.cpu.bus.io.ssi.voices;
                words
                    .chunks(fw)
                    .map(|_| voices.pop_front().unwrap_or((0.0, 0.0)))
                    .collect()
            } else {
                // The Syntakt adds its analog side (stanalog.rs), one pair a
                // codec frame.
                let st = m.cpu.bus.st.is_some();
                let pairs: Vec<(f32, f32)> = words.chunks(fw).map(|p| m.speaker_pair(p)).collect();
                if st {
                    let voices = &mut m.cpu.bus.io.ssi.voices;
                    pairs.into_iter().map(|(l, r)| {
                        let (al, ar) = voices.pop_front().unwrap_or((0.0, 0.0));
                        (l + al, r + ar)
                    }).collect()
                } else {
                    pairs
                }
            };
            // Resample by stepping through the source at the rate ratio.
            while phase < frames.len() as f64 {
                let (l, r) = frames[phase as usize];
                ring.push_back(l);
                ring.push_back(r);
                phase += step;
            }
            phase -= frames.len() as f64;
        } else if !live {
            out.clear();
        }
        if last_pub.elapsed() > Duration::from_millis(15) {
            last_pub = Instant::now();
            let (t, n) = last_speed;
            if t.elapsed() > Duration::from_millis(500) {
                speed = (m.now() - n) as f64 / ips / t.elapsed().as_secs_f64();
                last_speed = (Instant::now(), m.now());
            }
            let audio_s = if live {
                let ms = audio.ring.lock().unwrap().len() as f64 / 2.0 / rate * 1000.0;
                format!("AUDIO LIVE {:.0} KHZ  {:3.0} MS BUFFERED  {} DROPOUTS", rate / 1000.0, ms, audio.dropouts.load(Ordering::Relaxed))
            } else {
                "AUDIO OFF".to_string()
            };
            let status = format!("{}   {audio_s}   SPEED {:3.0}%   T {:.1} S", m.prof.name.to_uppercase(), speed * 100.0, m.now() as f64 / ips);
            *view.lock().unwrap() = view_of(&m, status);
        }
    }
    m
}

/// A stand-in sound card: drains the ring at 48 kHz of wall time and counts
/// the blocks it found short. What it drains can be kept for a WAV.
fn start_null_audio(audio: Arc<Audio>, keep: Option<Arc<Mutex<Vec<f32>>>>) {
    audio.rate.store(48_000, Ordering::Relaxed);
    audio.live.store(true, Ordering::Relaxed);
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let mut played = 0u64;
        let mut last = 0.0f32;
        loop {
            std::thread::sleep(Duration::from_millis(5));
            let due = (t0.elapsed().as_secs_f64() * 48_000.0) as u64;
            let n = (due - played) as usize;
            played = due;
            if !audio.primed.load(Ordering::Relaxed) {
                if let Some(k) = &keep {
                    k.lock().unwrap().extend(std::iter::repeat_n(0.0, n * 2));
                }
                continue;
            }
            let mut ring = audio.ring.lock().unwrap();
            let take = (n * 2).min(ring.len());
            let got: Vec<f32> = ring.drain(..take).collect();
            drop(ring);
            if let Some(&x) = got.last() {
                last = x;
            }
            if take < n * 2 && audible(last) {
                audio.dropouts.fetch_add(1, Ordering::Relaxed);
            }
            if let Some(k) = &keep {
                k.lock().unwrap().extend(got);
            }
        }
    });
}

/// Whether a gap after this sample would be heard (-80 dBFS and up).
fn audible(x: f32) -> bool {
    x.abs() > 1e-4
}

/// The host's default input device, mixed or split to stereo and resampled to
/// 48 kHz into `audio.input`.
fn start_input(audio: Arc<Audio>) -> Option<cpal::Stream> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let dev = cpal::default_host().default_input_device()?;
    let cfg = dev.default_input_config().ok()?;
    if cfg.sample_format() != cpal::SampleFormat::F32 {
        eprintln!("audio in: {:?} input not supported (f32 only)", cfg.sample_format());
        return None;
    }
    let ch = cfg.channels() as usize;
    let step = cfg.sample_rate().0 as f64 / 48_000.0;
    let mut phase = 0.0f64;
    let stream = dev
        .build_input_stream(
            &cfg.config(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let frames = data.len() / ch.max(1);
                let mut q = audio.input.lock().unwrap();
                while phase < frames as f64 {
                    let i = phase as usize * ch;
                    let l = data[i];
                    let r = if ch > 1 { data[i + 1] } else { l };
                    q.push_back((l, r));
                    phase += step;
                }
                phase -= frames as f64;
                while q.len() > 9600 {
                    q.pop_front();
                }
            },
            |e| eprintln!("audio in: {e}"),
            None,
        )
        .ok()?;
    stream.play().ok()?;
    Some(stream)
}

fn start_audio(audio: Arc<Audio>) -> Option<cpal::Stream> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let host = cpal::default_host();
    let dev = host.default_output_device()?;
    let supported = dev.supported_output_configs().ok()?.collect::<Vec<_>>();
    // Prefer 48 kHz stereo f32; take the device default otherwise.
    let cfg = supported
        .iter()
        .find(|c| c.channels() == 2 && c.min_sample_rate().0 <= 48_000 && c.max_sample_rate().0 >= 48_000 && c.sample_format() == cpal::SampleFormat::F32)
        .map(|c| c.with_sample_rate(cpal::SampleRate(48_000)))
        .or_else(|| dev.default_output_config().ok())?;
    if cfg.sample_format() != cpal::SampleFormat::F32 {
        eprintln!("audio: the device's format is {:?}, not f32; running without sound", cfg.sample_format());
        return None;
    }
    let channels = cfg.channels() as usize;
    audio.rate.store(cfg.sample_rate().0 as u64, Ordering::Relaxed);
    let a = audio.clone();
    let mut last = 0.0f32;
    let stream = dev
        .build_output_stream(
            &cfg.config(),
            move |data: &mut [f32], _| {
                if !a.primed.load(Ordering::Relaxed) {
                    data.fill(0.0);
                    return;
                }
                let mut ring = a.ring.lock().unwrap();
                let frames = data.len() / channels;
                if ring.len() < frames * 2 {
                    // What was already queued plays out first.
                    if let Some(&x) = ring.back() {
                        last = x;
                    }
                    if audible(last) {
                        a.dropouts.fetch_add(1, Ordering::Relaxed);
                    }
                } else if let Some(&x) = ring.get(frames * 2 - 1) {
                    last = x;
                }
                for f in data.chunks_mut(channels) {
                    let l = ring.pop_front().unwrap_or(0.0);
                    let r = ring.pop_front().unwrap_or(0.0);
                    for (i, s) in f.iter_mut().enumerate() {
                        *s = match (channels, i) {
                            (1, _) => (l + r) * 0.5,
                            (_, 0) => l,
                            (_, 1) => r,
                            _ => 0.0,
                        };
                    }
                }
            },
            |e| eprintln!("audio: {e}"),
            None,
        )
        .ok()?;
    stream.play().ok()?;
    audio.live.store(true, Ordering::Relaxed);
    Some(stream)
}

pub fn main() {
    let o = parse();
    let flash_image = o.flash.as_ref().filter(|_| !o.syx_given).and_then(|p| std::fs::read(p).ok());
    let fw = match flash_image.as_ref().filter(|img| img.len() > dtemu::machine::FLASH_SLOT) {
        Some(img) => Firmware::from_flash(&img[dtemu::machine::FLASH_SLOT..]).unwrap_or_else(|e| {
            eprintln!("{}: no OS in the flash: {e}", o.flash.as_ref().unwrap().display());
            std::process::exit(1)
        }),
        None => Firmware::from_file(&o.syx).unwrap_or_else(|e| {
            eprintln!("{}: {e}", o.syx.display());
            std::process::exit(1)
        }),
    };
    let mut m = Machine::new(&fw, 128).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    if let Some(p) = &o.flash {
        match m.load_flash(p) {
            Ok(true) => eprintln!("flash {}", p.display()),
            Ok(false) => eprintln!("flash {}: new", p.display()),
            Err(e) => {
                eprintln!("--flash {e}");
                std::process::exit(1)
            }
        }
    }
    // The card first: a snapshot then brings its own sectors (the firmware's
    // cached view of the drive matches those) and keeps the image path.
    let mut image = None;
    if let Some(c) = &o.card {
        match m.attach_card(c) {
            Ok(fresh) => {
                eprintln!("card {}{}", c.display(), if fresh { ": new, sample area formatted" } else { "" });
                if !fresh {
                    image = Some(m.cpu.bus.io.esdhc.card.sectors.clone());
                }
            }
            Err(e) => eprintln!("{}: {e}; using a blank card", c.display()),
        }
    }
    if let Some(p) = &o.snapshot {
        if p.exists() {
            match dtemu::snapshot::load(&mut m, p) {
                Ok(()) => {
                    eprintln!("resumed {}", p.display());
                    let card = &mut m.cpu.bus.io.esdhc.card;
                    card.dirty = card.path.is_some();
                    if image.is_some_and(|i| i != card.sectors) {
                        // Changed since the snapshot (dtcard, another
                        // session): the snapshot's copy runs and will be
                        // written back, so keep the image's version.
                        BACKUP_CARD.store(true, Ordering::Relaxed);
                        eprintln!(
                            "warning: {} differs from the snapshot's copy of the card; running the snapshot's. \
                             The image is kept as IMAGE.bak when the card is saved; start without --snapshot to use the image.",
                            o.card.as_ref().unwrap().display()
                        );
                    }
                }
                Err(e) => eprintln!("{}: {e}; booting from reset", p.display()),
            }
        }
    }
    let view = Arc::new(Mutex::new(view_of(&m, String::from("STARTING"))));
    let audio = Arc::new(Audio {
        ring: Mutex::new(VecDeque::new()),
        dropouts: AtomicU64::new(0),
        rate: AtomicU64::new(48_000),
        live: AtomicBool::new(false),
        primed: AtomicBool::new(false),
        input: Mutex::new(VecDeque::new()),
    });
    // The host input only when asked: it opens the microphone (macOS asks).
    let _input = match o.audio_in.as_deref() {
        Some(dest) => {
            let s = start_input(audio.clone());
            match &s {
                Some(_) => eprintln!("audio in: host default input -> {dest}"),
                None => eprintln!("audio in: no usable input device"),
            }
            s
        }
        None => None,
    };
    let kept: Option<Arc<Mutex<Vec<f32>>>> = o.wav.as_ref().map(|_| Arc::new(Mutex::new(Vec::new())));
    let _stream = if o.audio_null {
        start_null_audio(audio.clone(), kept.clone());
        None
    } else if o.audio && !o.headless {
        let s = start_audio(audio.clone());
        if s.is_none() {
            eprintln!("audio: no output device; running without sound");
        }
        s
    } else {
        None
    };
    let quit = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    if o.usb_host {
        m.usb_host = Some(dtemu::usbhost::UsbHost::new());
        eprintln!("usb: a simulated host is attached (set USB CONFIG to OVERBRIDGE)");
    }
    if let Some(addr) = &o.ob_link {
        match dtemu::oblink::ObLink::listen(addr) {
            Ok(l) => {
                eprintln!("overbridge link: listening on {addr}");
                m.ob_link = Some(l);
            }
            Err(e) => eprintln!("overbridge link {addr}: {e}"),
        }
    }
    let ports = o.midi && (!o.headless || o.midi_headless);
    // The same names as the output ports.
    let pname = std::env::var("DTEMU_MIDI_NAME").unwrap_or_else(|_| if m.is_a4() { "a4-emu" } else if m.is_syntakt() { "syntakt-emu" } else { "rytm-emu" }.into());
    let _midi_in = if ports { midi::start_in(tx.clone(), &format!("{pname} in"), false) } else { None };
    let _usb_in = if ports && { let v = view.lock().unwrap(); v.rytm || v.a4 } { midi::start_in(tx.clone(), &format!("{pname} usb in"), true) } else { None };
    // --midi-from NAME: an existing MIDI source (a real Rytm, a keyboard) into
    // the DIN MIDI input, without a routing app.
    let _from = o.midi_from.as_deref().and_then(|n| midi::connect_from(tx.clone(), n));
    if let Some(addr) = &o.usb_midi_tcp {
        let usb = { let v = view.lock().unwrap(); v.rytm || v.a4 };
        if usb {
            usbtcp::start(addr, tx.clone());
        } else {
            eprintln!("usb midi tcp: this device has no USB MIDI port here");
        }
    }
    let opts = Arc::new(o);
    let th = {
        let (view, audio, quit, opts) = (view.clone(), audio.clone(), quit.clone(), opts.clone());
        std::thread::spawn(move || emulate(m, &opts, rx, view, audio, quit))
    };
    let (rytm, a4, mk1) = { let v = view.lock().unwrap(); (v.rytm, v.a4, v.mk1) };
    let places = gui::layout_for(rytm, a4, mk1);
    let (cw, ch) = gui::window_size(rytm, a4, mk1);
    let mut cv = Canvas::new(cw, ch);
    if opts.headless {
        let t0 = Instant::now();
        let mut presses = opts.presses.clone();
        presses.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        let mut pending: Vec<(u8, f64)> = Vec::new();
        let mut turns = opts.turns.clone();
        let mut last_report = 0.0;
        while t0.elapsed().as_secs_f64() < opts.after {
            let t = t0.elapsed().as_secs_f64();
            turns.retain(|&(e, d, at)| {
                if at <= t {
                    let _ = tx.send(Ev::Turn(e, d));
                    false
                } else {
                    true
                }
            });
            if t - last_report >= 1.0 {
                last_report = t;
                eprintln!("{:5.1} s  {}", t, view.lock().unwrap().status);
            }
            while let Some(&(c, at, hold)) = presses.first() {
                if at > t {
                    break;
                }
                presses.remove(0);
                let _ = tx.send(Ev::Key(c, true));
                pending.push((c, at + hold));
            }
            pending.retain(|&(c, up)| {
                if up <= t {
                    let _ = tx.send(Ev::Key(c, false));
                    false
                } else {
                    true
                }
            });
            std::thread::sleep(Duration::from_millis(10));
        }
        quit.store(true, Ordering::Relaxed);
        let mut m = th.join().unwrap();
        if let Some(p) = &opts.usb_wav {
            let s = &m.cpu.bus.io.usb_audio;
            match Machine::write_wav16(p, s, 12) {
                Ok(()) => eprintln!("wrote {} (USB audio, {:.1} s, 12 channels)", p.display(), s.len() as f64 / 12.0 / 48_000.0),
                Err(e) => eprintln!("{}: {e}", p.display()),
            }
        }
        if opts.power_off {
            power_off(&mut m, &opts);
        } else {
            save_card(&mut m, &opts);
        }
        let v = view_of(&m, format!("HEADLESS, T {:.1} S", m.now() as f64 / opts.ips));
        gui::draw(&mut cv, &v, &places, &[], &[]);
        if let Some(p) = &opts.screenshot {
            gui::save_png(&cv, p).unwrap();
            eprintln!("wrote {}", p.display());
        }
        if let (Some(p), Some(k)) = (&opts.wav, &kept) {
            let s = k.lock().unwrap();
            write_wav(p, &s);
            let peak = s.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
            eprintln!("wrote {} ({:.2} s, peak {:.4})", p.display(), s.len() as f64 / 96_000.0, peak);
        }
        return;
    }
    run_window(&opts, tx, view, quit.clone(), &places, &mut cv);
    quit.store(true, Ordering::Relaxed);
    let mut m = th.join().unwrap();
    if let Some(p) = &opts.usb_wav {
        let s = &m.cpu.bus.io.usb_audio;
        match Machine::write_wav16(p, s, 12) {
            Ok(()) => eprintln!("wrote {} (USB audio, {:.1} s, 12 channels)", p.display(), s.len() as f64 / 12.0 / 48_000.0),
            Err(e) => eprintln!("{}: {e}", p.display()),
        }
    }
    if opts.power_off {
        power_off(&mut m, &opts);
        return;
    }
    save_card(&mut m, &opts);
    if let Some(p) = &opts.snapshot {
        match dtemu::snapshot::save(&m, p) {
            Ok(()) => eprintln!("saved {}", p.display()),
            Err(e) => eprintln!("save failed: {e}"),
        }
    }
}

/// Switch the Rytm off (the firmware saves its working state to the card) and
/// write the card back. The snapshot is left alone: the machine is off.
fn power_off(m: &mut Machine, o: &Opts) {
    let r = m.power_off(2_000_000_000);
    eprintln!(
        "power off: {} ({} sectors written; state slots 0x7a000 {} 0x7c000 {})",
        if r.done { "working state saved" } else { "the firmware did not finish its power-fail handler" },
        r.sectors_written,
        if r.slots[0] { "valid" } else { "-" },
        if r.slots[1] { "valid" } else { "-" }
    );
    if o.card.is_none() {
        eprintln!("power off: no --card, nothing kept");
    }
    save_card(m, o);
    if o.snapshot.is_some() {
        eprintln!("power off: snapshot not saved; start without --snapshot to boot into the saved state");
    }
}

fn run_window(o: &Opts, tx: mpsc::Sender<Ev>, view: Arc<Mutex<View>>, _quit: Arc<AtomicBool>, places: &[gui::Place], cv: &mut Canvas) {
    use minifb::{Key, KeyRepeat, MouseButton, MouseMode, Window, WindowOptions};
    let title = if a4_window() { "a4-emu (dtemu)" } else { "rytm-emu (dtemu)" };
    let mut win = match Window::new(title, cv.w, cv.h, WindowOptions::default()) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("cannot open a window: {e}");
            return;
        }
    };
    win.set_target_fps(60);
    let mut held: Vec<u8> = Vec::new(); // momentary, under the mouse
    let mut latched: Vec<u8> = Vec::new();
    let mut was_down = false;
    let mut drag: Option<gui::KnobDrag> = None;
    // Push switches clicked: released at these times.
    let mut push_up: Vec<(u8, Instant)> = Vec::new();
    // The knob the arrow keys turn: the last one clicked.
    let mut selected: Option<u8> = None;
    // A held pad: its code, place and last pressure sent.
    let mut pad_held: Option<(u8, gui::Place, u8)> = None;
    // Height on a pad -> force: the top edge 255, the bottom about 30.
    let force_at = |p: &gui::Place, y: f32| -> u8 { (30.0 + 225.0 * (1.0 - ((y - p.y as f32) / p.h as f32).clamp(0.0, 1.0))) as u8 };
    let mut shot_at = o.screenshot.as_ref().map(|_| Instant::now() + Duration::from_secs_f64(o.after));
    while win.is_open() {
        let (mx, my) = win.get_mouse_pos(MouseMode::Discard).unwrap_or((-1.0, -1.0));
        let down = win.get_mouse_down(MouseButton::Left);
        let shift = win.is_key_down(Key::LeftShift) || win.is_key_down(Key::RightShift);
        let ctrl = [Key::LeftCtrl, Key::RightCtrl, Key::LeftSuper, Key::RightSuper].iter().any(|&k| win.is_key_down(k));
        let hit = places.iter().find(|p| p.hit(mx as i32, my as i32)).copied();
        if down && !was_down {
            match hit.map(|p| p.c) {
                Some(Control::Key(c)) | Some(Control::Knob(_, Some(c))) if shift => {
                    if let Some(i) = latched.iter().position(|&k| k == c) {
                        latched.remove(i);
                        let _ = tx.send(Ev::Key(c, false));
                    } else {
                        latched.push(c);
                        let _ = tx.send(Ev::Key(c, true));
                    }
                }
                Some(Control::Key(c)) if rytm_pad_code(c) => {
                    let p = hit.unwrap();
                    let f = force_at(&p, my);
                    held.push(c);
                    pad_held = Some((c, p, f));
                    let _ = tx.send(Ev::PadDown(c, f));
                }
                Some(Control::Key(c)) => {
                    held.push(c);
                    let _ = tx.send(Ev::Key(c, true));
                }
                Some(Control::Knob(e, push)) => {
                    // A drag turns, a click pushes, ctrl-click holds the push (`KnobDrag`).
                    let d = gui::KnobDrag::new(e, push, ctrl, mx, my);
                    if let Some(c) = d.pushed {
                        held.push(c);
                        let _ = tx.send(Ev::Key(c, true));
                    }
                    drag = Some(d);
                    selected = Some(e);
                }
                None => {}
            }
        }
        if down {
            if let Some((c, p, last)) = pad_held {
                let v = force_at(&p, my);
                if v != last {
                    let _ = tx.send(Ev::Pressure(c, v));
                    pad_held = Some((c, p, v));
                }
            }
            if let Some(d) = drag.as_mut() {
                // Followed outside the knob and the window while held.
                let (px, py) = win.get_mouse_pos(MouseMode::Pass).unwrap_or((mx, my));
                if let Some(n) = d.motion(px, py) {
                    let _ = tx.send(Ev::Turn(d.enc, n));
                }
            }
        }
        if !down && was_down {
            pad_held = None;
            for c in held.drain(..) {
                let _ = tx.send(Ev::Key(c, false));
            }
            // A ctrl-clicked push switch was in `held`.
            if let Some(gui::DragEnd::Click(c)) = drag.take().map(|d| d.release()) {
                let _ = tx.send(Ev::Key(c, true));
                push_up.push((c, Instant::now() + Duration::from_millis(120)));
            }
        }
        push_up.retain(|&(c, at)| {
            if Instant::now() >= at {
                let _ = tx.send(Ev::Key(c, false));
                false
            } else {
                true
            }
        });
        was_down = down;
        if let Some((_, wy)) = win.get_scroll_wheel() {
            if let Some(Control::Knob(e, _)) = hit.map(|p| p.c) {
                let n = if wy > 0.0 { 4 } else { -4 };
                let _ = tx.send(Ev::Turn(e, n));
            }
        }
        // Arrow keys on the selected knob: up/down one step, right/left eight;
        // held, they repeat.
        if let Some(e) = selected {
            for k in win.get_keys_pressed(KeyRepeat::Yes) {
                let step = match k {
                    Key::Up => Some((1, 1)),
                    Key::Down => Some((-1, 1)),
                    Key::Right => Some((1, 8)),
                    Key::Left => Some((-1, 8)),
                    _ => None,
                };
                if let Some((d, n)) = step {
                    let _ = tx.send(Ev::Nudge(e, d, n));
                }
            }
        }
        for k in win.get_keys_pressed(KeyRepeat::No) {
            match k {
                Key::Escape => {
                    latched.clear();
                    selected = None;
                    let _ = tx.send(Ev::ReleaseAll);
                }
                Key::F5 => {
                    let _ = tx.send(Ev::Save);
                }
                _ => {}
            }
        }
        let v = view.lock().unwrap().clone();
        let mut shown = held.clone();
        shown.extend(latched.iter());
        shown.extend(push_up.iter().map(|p| p.0));
        gui::draw_sel(cv, &v, places, &shown, &latched, selected);
        if let Some(t) = shot_at {
            if Instant::now() >= t {
                if let Some(p) = &o.screenshot {
                    let _ = gui::save_png(cv, p);
                    eprintln!("wrote {}", p.display());
                }
                shot_at = None;
            }
        }
        if win.update_with_buffer(&cv.px, cv.w, cv.h).is_err() {
            break;
        }
    }
}

/// Write the card back to its image, if it has one and it changed.
/// Set when the image on disk is not the card the snapshot resumed with.
static BACKUP_CARD: AtomicBool = AtomicBool::new(false);

fn save_flash(m: &mut Machine, o: &Opts) {
    if let Some(p) = &o.flash {
        if let Err(e) = m.save_flash(p) {
            eprintln!("--flash {e}");
        }
    }
}

fn save_card(m: &mut Machine, o: &Opts) {
    save_flash(m, o);
    if let Some(c) = &o.card {
        let dirty = m.cpu.bus.io.esdhc.card.dirty;
        if dirty && BACKUP_CARD.swap(false, Ordering::Relaxed) && c.exists() {
            let mut bak = c.clone().into_os_string();
            bak.push(".bak");
            let bak = PathBuf::from(bak);
            match std::fs::rename(c, &bak) {
                Ok(()) => eprintln!("kept the previous image as {}", bak.display()),
                Err(e) => {
                    BACKUP_CARD.store(true, Ordering::Relaxed);
                    eprintln!("{}: {e}; not overwriting {}", bak.display(), c.display());
                    return;
                }
            }
        }
        match m.flush_card() {
            Ok(()) if dirty => eprintln!("wrote card {}", c.display()),
            Ok(()) => {}
            Err(e) => eprintln!("{}: {e}", c.display()),
        }
    }
}

fn write_wav(path: &std::path::Path, s: &[f32]) {
    let data: Vec<u8> = s.iter().flat_map(|&x| ((x.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
    let mut f = Vec::new();
    f.extend_from_slice(b"RIFF");
    f.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    f.extend_from_slice(b"WAVEfmt ");
    f.extend_from_slice(&16u32.to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&2u16.to_le_bytes());
    f.extend_from_slice(&48_000u32.to_le_bytes());
    f.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
    f.extend_from_slice(&4u16.to_le_bytes());
    f.extend_from_slice(&16u16.to_le_bytes());
    f.extend_from_slice(b"data");
    f.extend_from_slice(&(data.len() as u32).to_le_bytes());
    f.extend_from_slice(&data);
    let _ = std::fs::write(path, f);
}

/// The virtual MIDI ports: the DIN MIDI port of the emulated machine, as seen
/// from the host. Messages from "rytm-emu in" go to the firmware's MIDI input;
/// what the firmware sends comes out of "rytm-emu out".
/// The USB MIDI port over TCP (--usb-midi-tcp): a raw MIDI byte stream both ways, no framing.
/// One client at a time: a new connection replaces the one before (a dead client never holds
/// the port). What arrives goes to the USB MIDI input as whole messages, as the "usb in" port
/// delivers them; the USB MIDI output goes to the client as it comes, or nowhere without one.
mod usbtcp {
    use super::Ev;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{mpsc, Mutex, OnceLock};

    static CLIENT: OnceLock<Mutex<Option<TcpStream>>> = OnceLock::new();

    pub fn start(addr: &str, tx: mpsc::Sender<Ev>) {
        let listener = match TcpListener::bind(addr) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("usb midi tcp {addr}: {e}");
                return;
            }
        };
        eprintln!("usb midi tcp: listening on {addr}");
        CLIENT.get_or_init(|| Mutex::new(None));
        std::thread::spawn(move || {
            for s in listener.incoming().flatten() {
                let _ = s.set_nodelay(true);
                let Ok(w) = s.try_clone() else { continue };
                if let Some(old) = CLIENT.get().unwrap().lock().unwrap().replace(w) {
                    let _ = old.shutdown(std::net::Shutdown::Both);
                }
                let tx = tx.clone();
                std::thread::spawn(move || reader(s, tx));
            }
        });
    }

    /// Data bytes after a status byte (0 for SysEx, which runs to F7).
    fn data_len(status: u8) -> usize {
        match status & 0xF0 {
            0xC0 | 0xD0 => 1,
            0x80..=0xE0 => 2,
            _ => match status {
                0xF1 | 0xF3 => 1,
                0xF2 => 2,
                _ => 0,
            },
        }
    }

    fn reader(mut s: TcpStream, tx: mpsc::Sender<Ev>) {
        let mut buf = [0u8; 4096];
        let (mut msg, mut sysex, mut running) = (Vec::new(), false, 0u8);
        loop {
            let n = match s.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            for &b in &buf[..n] {
                if b >= 0xF8 {
                    let _ = tx.send(Ev::UsbMidi(vec![b])); // real time, even inside a SysEx
                } else if b == 0xF0 {
                    (msg, sysex) = (vec![b], true);
                } else if sysex {
                    msg.push(b);
                    if b == 0xF7 {
                        let _ = tx.send(Ev::UsbMidi(std::mem::take(&mut msg)));
                        sysex = false;
                    }
                } else if b & 0x80 != 0 {
                    running = if b < 0xF0 { b } else { 0 };
                    msg = vec![b];
                    if data_len(b) == 0 {
                        let _ = tx.send(Ev::UsbMidi(std::mem::take(&mut msg)));
                    }
                } else {
                    if msg.is_empty() && running != 0 {
                        msg.push(running);
                    }
                    if msg.is_empty() {
                        continue; // a stray data byte
                    }
                    msg.push(b);
                    if msg.len() == 1 + data_len(msg[0]) {
                        let _ = tx.send(Ev::UsbMidi(std::mem::take(&mut msg)));
                    }
                }
            }
        }
    }

    pub fn send(bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let Some(c) = CLIENT.get() else { return };
        let mut c = c.lock().unwrap();
        if c.as_mut().is_some_and(|s| s.write_all(bytes).is_err()) {
            *c = None;
        }
    }
}

mod midi {
    use super::Ev;
    #[cfg(unix)]
    use midir::os::unix::{VirtualInput, VirtualOutput};
    use std::sync::mpsc;

    /// Windows has no virtual MIDI ports: the window opens existing ports by
    /// name instead, which a loopback driver (loopMIDI, for one) provides.
    /// The best match for `name`: the exact name, else the first port whose
    /// name starts with it (Windows may number duplicates, "NAME 1").
    #[cfg(not(unix))]
    fn find_port<T: midir::MidiIO>(io: &T, name: &str) -> Option<T::Port> {
        let ports = io.ports();
        let names: Vec<String> = ports.iter().map(|p| io.port_name(p).unwrap_or_default()).collect();
        let at = names.iter().position(|n| n.eq_ignore_ascii_case(name)).or_else(|| {
            names.iter().position(|n| n.to_lowercase().starts_with(&name.to_lowercase()))
        });
        if at.is_none() {
            eprintln!(
                "midi: no port named \"{name}\" (have: {}); on Windows create it with a loopback driver such as loopMIDI",
                if names.is_empty() { "none".to_string() } else { names.join(", ") }
            );
        }
        at.map(|i| ports[i].clone())
    }

    /// DTEMU_MIDI_LOG=FILE: every message through the virtual ports, both ways,
    /// with the time since the first one, appended to FILE.
    pub fn log(dir: &str, m: &[u8]) {
        use std::io::Write;
        use std::sync::OnceLock;
        static T0: OnceLock<std::time::Instant> = OnceLock::new();
        let Some(path) = std::env::var_os("DTEMU_MIDI_LOG") else { return };
        let t = T0.get_or_init(std::time::Instant::now).elapsed().as_secs_f64();
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let hex: Vec<String> = m.iter().map(|b| format!("{b:02x}")).collect();
            let _ = writeln!(f, "{t:9.3} {dir} {}", hex.join(" "));
        }
    }

    /// A virtual input port `name` (on Windows, the existing port `name`);
    /// `usb`: its messages go to the USB MIDI port.
    pub fn start_in(tx: mpsc::Sender<Ev>, name: &str, usb: bool) -> Option<midir::MidiInputConnection<()>> {
        let mut input = midir::MidiInput::new("rytm-emu").map_err(|e| eprintln!("midi in: {e}")).ok()?;
        input.ignore(midir::Ignore::None); // SysEx, clock and active sensing too
        // midir on Windows hands a long SysEx over in 1024-byte pieces (its
        // WinMM buffers; elsewhere it arrives whole): put them back together
        // so a piece without its F0 is not taken for stray data bytes.
        let mut sysex: Vec<u8> = Vec::new();
        let on_msg = move |_: u64, msg: &[u8], _: &mut ()| {
            let whole = if msg.first() == Some(&0xF0) || !sysex.is_empty() && msg.first().is_some_and(|&b| b < 0x80) {
                sysex.extend_from_slice(msg);
                if sysex.last() != Some(&0xF7) {
                    return;
                }
                std::mem::take(&mut sysex)
            } else {
                msg.to_vec()
            };
            let _ = tx.send(if usb { Ev::UsbMidi(whole) } else { Ev::Midi(whole) });
        };
        #[cfg(unix)]
        let (conn, kind) = (input.create_virtual(name, on_msg, ()), "virtual input");
        #[cfg(not(unix))]
        let (conn, kind) = {
            let port = find_port(&input, name)?;
            (input.connect(&port, name, on_msg, ()), "input")
        };
        match conn {
            Ok(c) => {
                eprintln!("midi: {kind} \"{name}\"");
                Some(c)
            }
            Err(e) => {
                eprintln!("midi in: {e}");
                None
            }
        }
    }

    /// Connect the first existing MIDI source whose name contains `name` to
    /// the DIN MIDI input.
    pub fn connect_from(tx: mpsc::Sender<Ev>, name: &str) -> Option<midir::MidiInputConnection<()>> {
        let mut input = midir::MidiInput::new("rytm-emu").ok()?;
        input.ignore(midir::Ignore::None);
        let ports = input.ports();
        let found = ports.iter().find(|p| input.port_name(p).is_ok_and(|n| n.to_lowercase().contains(&name.to_lowercase())));
        let Some(port) = found else {
            let names: Vec<String> = ports.iter().filter_map(|p| input.port_name(p).ok()).collect();
            eprintln!("midi from: no source matching \"{name}\" (have: {})", names.join(", "));
            return None;
        };
        let pname = input.port_name(port).unwrap_or_default();
        match input.connect(port, "rytm-emu from", move |_, msg, _| {
            let _ = tx.send(Ev::Midi(msg.to_vec()));
        }, ()) {
            Ok(c) => {
                eprintln!("midi from: \"{pname}\" -> the DIN MIDI input");
                Some(c)
            }
            Err(e) => {
                eprintln!("midi from: {e}");
                None
            }
        }
    }

    /// The firmware's MIDI byte stream, split into messages for the port.
    pub struct Out {
        conn: midir::MidiOutputConnection,
        tag: &'static str,
        msg: Vec<u8>,
        running: u8,
        sysex: bool,
    }

    impl Out {
        /// A virtual output port `name` (on Windows, the existing port `name`);
        /// `tag` labels it in DTEMU_MIDI_LOG.
        pub fn start(name: &str, tag: &'static str) -> Option<Out> {
            let out = midir::MidiOutput::new("rytm-emu").map_err(|e| eprintln!("midi out: {e}")).ok()?;
            #[cfg(unix)]
            let (conn, kind) = (out.create_virtual(name), "virtual output");
            #[cfg(not(unix))]
            let (conn, kind) = {
                let port = find_port(&out, name)?;
                (out.connect(&port, name), "output")
            };
            match conn {
                Ok(conn) => {
                    eprintln!("midi: {kind} \"{name}\"");
                    Some(Out { conn, tag, msg: Vec::new(), running: 0, sysex: false })
                }
                Err(e) => {
                    eprintln!("midi out: {e}");
                    None
                }
            }
        }

        fn emit(&mut self, m: &[u8]) {
            log(self.tag, m);
            let _ = self.conn.send(m);
        }

        /// Data bytes a status byte takes.
        fn data_len(status: u8) -> usize {
            match status {
                0xC0..=0xDF | 0xF1 | 0xF3 => 1,
                0x80..=0xEF | 0xF2 => 2,
                _ => 0,
            }
        }

        pub fn send(&mut self, bytes: &[u8]) {
            for &b in bytes {
                match b {
                    0xF8..=0xFF => self.emit(&[b]), // real time, anywhere
                    0xF0 => {
                        self.msg = vec![b];
                        self.sysex = true;
                    }
                    0xF7 if self.sysex => {
                        self.msg.push(b);
                        let m = std::mem::take(&mut self.msg);
                        self.emit(&m);
                        self.sysex = false;
                    }
                    0x80..=0xF6 => {
                        self.sysex = false;
                        if b < 0xF0 {
                            self.running = b;
                        }
                        self.msg = vec![b];
                        if Self::data_len(b) == 0 {
                            let m = std::mem::take(&mut self.msg);
                            self.emit(&m);
                        }
                    }
                    _ => {
                        if self.sysex {
                            self.msg.push(b);
                            continue;
                        }
                        if self.msg.is_empty() {
                            if self.running == 0 {
                                continue;
                            }
                            self.msg.push(self.running);
                        }
                        self.msg.push(b);
                        if self.msg.len() == 1 + Self::data_len(self.msg[0]) {
                            let m = std::mem::take(&mut self.msg);
                            self.emit(&m);
                        }
                    }
                }
            }
        }
    }
}

/// Whether a window key code is one of the Rytm's pads (they strike with a
/// force and report pressure while held).
fn rytm_pad_code(code: u8) -> bool {
    code >= dtemu::panel::RYTM_CODE_BASE
        && dtemu::panel::key_name(code).is_some_and(|n| dtemu::machine::RYTM_PADS.iter().any(|p| p.eq_ignore_ascii_case(n)))
}

/// Whether the window shows an Analog Four MKII (its panel tables are loaded).
fn a4_window() -> bool {
    dtemu::panel::a4_panel() || dtemu::panel::a4mk1_panel()
}
