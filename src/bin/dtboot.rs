//! Headless boot: run the firmware and report progress.
//!
//!     dtboot [FIRMWARE.syx] [options]   (see --help)

use dtemu::cpu::Stop;
use dtemu::firmware::Firmware;
use dtemu::machine::Machine;
use std::time::Instant;

fn num(s: &str) -> u64 {
    let s = s.replace('_', "");
    let (n, mul) = match s.chars().last() {
        Some('M') => (&s[..s.len() - 1], 1_000_000),
        Some('G') => (&s[..s.len() - 1], 1_000_000_000),
        Some('k') => (&s[..s.len() - 1], 1_000),
        _ => (&s[..], 1),
    };
    n.parse::<f64>().map(|v| (v * mul as f64) as u64).expect("number")
}

const USAGE: &str = "\
usage: dtboot [FIRMWARE.syx] [options]

Runs the firmware headless and reports progress. Clocks are instruction
counts (suffixes k, M, G).

  --instr N            run N instructions (default 100M)
  --every N            report every N (default 10M)
  --ips N              instructions per emulated second (default 200M)
  --load SNAP          resume a snapshot
  --save SNAP          save a snapshot at the end
  --card IMAGE         back the eMMC with an image file (read only: changes
                       are not written back; a missing image starts formatted)
  --card-save          write the card (as the run left it) back to --card's
                       image at the end; with --load, the snapshot's card
  --power-off AT       switch the power off at clock AT (Rytm): the firmware's
                       power-fail handler saves the working state to the card
                       (raw eMMC slots 0x7a000/0x7c000), then the run ends and
                       the card is written back (implies --card-save); a cold
                       boot on that card comes up in the saved state
  --press KEY@AT[:HOLD]
                       press KEY at clock AT for HOLD (default 30M): a control
                       code (Digitakt) or a key name (Rytm: panel::RYTM_KEYS,
                       e.g. YES, FUNC, T1, BD, BANK_A, PUSH_A)
  --turn ENC:DELTA@ATxCOUNT[/SPACING]
                       turn encoder ENC by DELTA, COUNT times, SPACING apart
  --raw HEX@AT         send raw panel-MCU bytes at clock AT (e.g. 4064@5G)
  --midi HEX@AT        DIN MIDI in at clock AT, at 31250 baud (e.g. 903c64@5G)
  --midi-file F@AT     a file's bytes on DIN MIDI in (a .syx dump)
  --wav OUT.wav        record the audio output (every DAC word of the codec frame)
  --main-wav OUT.wav   record the speakers' stereo mix (Machine::speaker_pair; the
                       Syntakt's adds its modelled analog voices and FX block)
  --dsp-wav OUT.wav    (Syntakt) record tracks 1..8 as the second CPU renders
                       them, one channel each
  --break ADDR         stop at a PC (hex); --break-after N arms it after N
  --poke ADDR=VALUE    write a 32-bit word (hex) once before the run
  --watch ADDR         log writes to a DDR or SRAM (0x8000....) address (hex;
                       SRAM writes are seen with DTEMU_NO_JIT=1 only)
  --midi-out FILE      write everything sent on MIDI out to FILE
  --usb-midi HEX@AT    bytes on the USB MIDI port (Rytm; Digitakt with --usb-host)
  --usb-midi-file F@AT[xN/SPACING]  a file's bytes on the USB MIDI port, N times
  --analog-wav FILE    record the modelled analog voices' mix (Rytm; A4 with its
                       effects' return; Syntakt: tracks 9..12 and the analog FX
                       block, without the DAC's main pair), stereo
  --usb-wav FILE       record the USB audio stream (12 channels: MAIN L/R, voices, AUDIO IN L/R)
  --audio-in FILE.wav  play a 16-bit WAV into AUDIO IN L/R from the start (Rytm)
  --ob-in FILE.wav[:CH]  a WAV's first channel as Overbridge input channel CH
                       (4..11 = BD SD RS/CP BT LT MT/HT CH/OH CY/CB; default 4)
  --nudge ENC:DIR[:F|:8]@CLOCK[xN/SPACING]  one value step on an encoder, as the
                       window's arrow keys make it (F: with FUNC held; 8: 8 steps)
  --usb-host           attach a simulated USB host (usbhost.rs): enumerates the
                       Rytm or Digitakt and, in OVERBRIDGE mode, runs the audio stream
                       (DTEMU_USB_LOG=1 logs it)
  --usb-midi-real      run the firmware's own USB MIDI sender and its request
                       pool: the USB host reads MIDI IN (endpoint 0x81) and the
                       requests complete as it does (needs --usb-host; also
                       DTEMU_USB_MIDI_REAL=1). Lists waits from interrupt level.
  --usb-host-ack N     (Digitakt) answer every SysEx the device sends with an N-byte
                       one, on the MIDI bulk OUT endpoint
  --usb-host-midi-every N  read MIDI IN one dTD each N microframes (a slow host)
  --usb-host-sleep FROM[:TO]  the USB host suspends the bus between these clocks
                       (a sleeping laptop: no SOF, no transfers); no TO: to the end
  --usb-host-noread FROM[:TO]  the host keeps the bus running but stops reading
                       MIDI IN (an application with the port closed)
  --usb-host-gone FROM:TO  the host browns out and restarts: VBUS off, no bus
                       from FROM, then at TO a fresh host (bus reset, a new
                       enumeration); repeatable
  --ob-link [ADDR]     serve the Overbridge input to a host over TCP (oblink.rs;
                       default 127.0.0.1:7301; 0.0.0.0:PORT serves the LAN)
  --ext-in FILE.wav    play a 16-bit WAV into EXT IN L/R from the start (Rytm)
  --watch-read ADDR LEN  log CPU reads and writes of LEN bytes at ADDR (hex)
  --count ADDR         count visits to a PC (hex)
  --trace N            keep the last N PCs for fault reports
  --profile-after N    sample guest PCs from clock N on
  --flash FILE         the SPI flash (MKII units, Syntakt) from FILE if it exists,
                       written back at the end: what the OS wrote (an OS upgrade)
                       persists. With no .syx given, boots the OS the flash holds.
                       A host file only; nothing here reaches a unit
  --usb-serial TEXT@AT  (implies --usb-host) the host as a serial (CDC ACM) device's
                       driver, as the factory-test mode enumerates (--test-mode):
                       TEXT and a newline on its data
                       port at clock AT; what comes back is printed at the end
  --test-mode          boot into the factory-test console (Rytm MKII, A4 MKII): the
                       boot flag the bootstrap sets for its power-on key combo
  --reboot             when the firmware resets (RCR SOFTRST, as at the end of an
                       OS upgrade), boot again from the OS in the flash, keeping
                       the flash and the card; events not yet due run on the new
                       boot's clock. Without it the run ends at the reset
  --no-masks           ignore interrupt masks

Environment:
  DTBOOT_OLED          end with the panel MCU's OLED, not the last captured frame
  DTBOOT_END_PNG=FILE  the screen at the end of the run as a PNG
  DTBOOT_DSP_STREAM=F  (Syntakt) write the bytes CPU1 streamed to the DSP's boot port
  DTEMU_SYNTAKT_DSP=stub  (Syntakt) the stage 1 stand-in instead of the second CPU
  DTEMU_DSP_PROFILE=1  (Syntakt) sample the second CPU's PCs; the hottest are listed
  DTBOOT_LEDS          (=rgb: also each LED's colour) print the LED selector byte of each group at the end
  DTBOOT_PEEK=A,B,..   print those words (hex addresses) on each status line
  DTBOOT_STEPS         after each key release, settle 40M and show the screen
                       (as PNGs in DTBOOT_PNG_DIR if set)
  DTBOOT_TURN_PNG=DIR  a PNG after each turn, DTBOOT_TURN_SETTLE (3M) later
  DTEMU_STRICT=1       unmapped pages and the SRAM mirror beyond 64 KB raise an
                       access error (interpreter only); listed as `strict:` lines
                       (DTEMU_STRICT=log: listed only, no fault)
  DTEMU_PROFILE_DUMP=FILE  with --profile-after: every sampled block start and its
                       sample count (one sample per 64 instructions) to FILE
  DTEMU_TIMING=unit    count core cycles from the MCF54418 timing tables and cache misses,
                       so CPU load follows the unit's (interpreter only; docs/TIMING.md);
                       DTEMU_TIMING_PARAMS=k=v,... sets its costs; clocks are then cycles
  DTEMU_CACHE=1        model the I- and D-caches (interpreter only; docs/CACHE.md);
                       =log also prints each stale instruction fetch
";

/// The flash holds no OS the emulator can boot: after an interrupted upgrade,
/// a unit's bootstrap finds its check failing and offers its own OS upgrade
/// (STARTUP MENU, `0x8000356c`); the emulator does not run the bootstrap.
fn no_os_in_flash(e: &str) -> ! {
    println!("the flash holds no bootable OS ({e}).");
    println!("On a unit the bootstrap decides next: its check (0x8000214c, inferred to be the OS check) failing opens its STARTUP MENU with OS UPGRADE. The emulator does not run the bootstrap.");
    std::process::exit(3);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut syx = None;
    let mut instr = 100_000_000u64;
    let mut every = 10_000_000u64;
    let mut ips = None;
    let mut no_masks = false;
    let mut watch: Option<u32> = None;
    let mut pokes: Vec<(u32, u32)> = Vec::new();
    let mut watch_read: Option<(u32, u32)> = None;
    let mut midi_out_file: Option<String> = None;
    let mut usb_midis: Vec<(Vec<u8>, u64)> = Vec::new();
    let mut analog_wav: Option<String> = None;
    let mut audio_in: Option<String> = None;
    let mut ob_in: Option<String> = None;
    let mut ob_link: Option<String> = None;
    let mut usb_host = false;
    let mut usb_midi_real = false;
    let mut usb_sleep: Option<(u64, u64)> = None;
    let mut usb_noread: Option<(u64, u64)> = None;
    let mut usb_ack: Option<usize> = None;
    let mut usb_midi_every = 0u64;
    let mut usb_gone: Vec<(u64, u64)> = Vec::new();
    let mut nudges: Vec<(u64, u8, i8, bool, bool)> = Vec::new();
    let mut usb_wav: Option<String> = None;
    let mut ext_in: Option<String> = None;
    let mut analog: Vec<i32> = Vec::new();
    let mut analog_mix: Vec<(f32, f32)> = Vec::new();
    let mut breaks: Vec<u32> = Vec::new();
    let mut trace = 0usize;
    let mut break_after = 0u64;
    let mut counts: Vec<u32> = Vec::new();
    let mut profile_after: Option<u64> = None;
    let mut save: Option<String> = None;
    let mut wav: Option<String> = None;
    let mut main_wav: Option<String> = None;
    let mut dsp_wav: Option<String> = None;
    let mut card: Option<String> = None;
    let mut card_save = false;
    let mut flash: Option<String> = None;
    let mut flash_loaded: Option<Vec<u8>> = None;
    let mut test_mode = false;
    let mut serials: Vec<(Vec<u8>, u64)> = Vec::new();
    let mut reboot = false;
    let mut power_off: Option<u64> = None;
    let mut pcm: Vec<i32> = Vec::new();
    let mut presses: Vec<(String, u64, u64)> = Vec::new();
    let mut raws: Vec<(Vec<u8>, u64)> = Vec::new();
    let mut midis: Vec<(Vec<u8>, u64)> = Vec::new();
    let mut step_no = 0;
    let mut turns: Vec<(u64, u8, i8)> = Vec::new();
    let mut load: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--instr" => instr = num(it.next().unwrap()),
            "--every" => every = num(it.next().unwrap()),
            "--ips" => ips = Some(num(it.next().unwrap())),
            "--no-masks" => no_masks = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            "--break" => {
                let a = u32::from_str_radix(it.next().unwrap().trim_start_matches("0x"), 16).unwrap();
                breaks.push(a);
            }
            "--press" => {
                // --press CODE@CLOCK[:HOLD]  e.g. 6@4100M:50M
                let v = it.next().unwrap();
                let (code, rest) = v.split_once('@').unwrap();
                let (at, hold) = rest.split_once(':').unwrap_or((rest, "30M"));
                presses.push((code.to_string(), num(at), num(hold)));
            }
            "--midi-out" => midi_out_file = it.next().cloned(),
            "--analog-wav" => analog_wav = it.next().cloned(),
            "--audio-in" => audio_in = it.next().cloned(),
            "--ob-in" => ob_in = it.next().cloned(),
            "--ob-link" => ob_link = Some(it.next().cloned().unwrap_or_else(|| "127.0.0.1:7301".into())),
            "--usb-host" => usb_host = true,
            "--nudge" => {
                // --nudge ENC:DIR[:F]@CLOCK[xCOUNT/SPACING]: one value step (F: with FUNC)
                let v = it.next().unwrap();
                let (ed, rest) = v.split_once('@').unwrap();
                let mut f = ed.split(':');
                let e: u8 = f.next().unwrap().parse().unwrap();
                let d: i8 = f.next().unwrap().parse().unwrap();
                let flag = f.next();
                let func = flag == Some("F");
                let big = flag == Some("8");
                let (rest, spacing) = rest.split_once('/').map(|(a, b)| (a, num(b))).unwrap_or((rest, 100_000_000));
                let (at, count) = rest.split_once('x').unwrap_or((rest, "1"));
                for k in 0..count.parse::<u64>().unwrap() {
                    nudges.push((num(at) + k * spacing, e, d, func, big));
                }
            }
            "--usb-wav" => usb_wav = it.next().cloned(),
            "--ext-in" => ext_in = it.next().cloned(),
            "--usb-midi" => {
                // --usb-midi HEX@CLOCK: bytes on the USB MIDI port
                let v = it.next().unwrap();
                let (what, at) = v.rsplit_once('@').unwrap();
                let bytes: Vec<u8> = (0..what.len()).step_by(2).map(|i| u8::from_str_radix(&what[i..i + 2], 16).unwrap()).collect();
                usb_midis.push((bytes, num(at)));
            }
            "--usb-midi-file" => {
                // --usb-midi-file FILE@CLOCK[xCOUNT/SPACING]: a file's bytes on the
                // USB MIDI port, optionally repeated COUNT times SPACING apart.
                let v = it.next().unwrap();
                let (what, when) = v.rsplit_once('@').unwrap();
                let bytes = std::fs::read(what).unwrap_or_else(|e| panic!("{what}: {e}"));
                let (at, rep) = match when.split_once('x') {
                    Some((at, r)) => (at, Some(r)),
                    None => (when, None),
                };
                let (count, spacing) = match rep {
                    Some(r) => {
                        let (c, sp) = r.split_once('/').expect("xCOUNT/SPACING");
                        (c.parse::<u64>().unwrap(), num(sp))
                    }
                    None => (1, 0),
                };
                for k in 0..count {
                    usb_midis.push((bytes.clone(), num(at) + k * spacing));
                }
            }
            "--midi" | "--midi-file" => {
                // --midi HEX@CLOCK: bytes on the DIN MIDI input, e.g. 903c64@5G
                // --midi-file FILE.syx@CLOCK: a file's bytes (a SysEx dump)
                let file = a == "--midi-file";
                let v = it.next().unwrap();
                let (what, at) = v.rsplit_once('@').unwrap();
                let bytes: Vec<u8> = if file {
                    std::fs::read(what).unwrap_or_else(|e| panic!("{what}: {e}"))
                } else {
                    (0..what.len()).step_by(2).map(|i| u8::from_str_radix(&what[i..i + 2], 16).unwrap()).collect()
                };
                midis.push((bytes, num(at)));
            }
            "--raw" => {
                // --raw HEX@CLOCK: bytes as the panel MCU would send them, e.g. 4064@5150M
                let v = it.next().unwrap();
                let (hex, at) = v.split_once('@').unwrap();
                let bytes = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
                raws.push((bytes, num(at)));
            }
            "--wav" => wav = it.next().cloned(),
            "--main-wav" => main_wav = it.next().cloned(),
            "--dsp-wav" => dsp_wav = it.next().cloned(),
            "--card" => card = it.next().cloned(),
            "--card-save" => card_save = true,
            "--flash" => flash = it.next().cloned(),
            "--usb-serial" => {
                // TEXT@CLOCK: a line to the serial device; a newline ends it.
                let v = it.next().unwrap();
                let (what, at) = v.rsplit_once('@').unwrap();
                let mut b = what.as_bytes().to_vec();
                if !b.ends_with(b"\n") {
                    b.push(b'\n');
                }
                serials.push((b, num(at)));
                usb_host = true;
            }
            "--reboot" => reboot = true,
            "--test-mode" => test_mode = true,
            "--power-off" => power_off = Some(num(it.next().unwrap())),
            "--usb-midi-real" => usb_midi_real = true,
            "--usb-host-midi-every" => usb_midi_every = it.next().map_or(0, |v| v.parse().expect("--usb-host-midi-every N")),
            "--usb-host-ack" => usb_ack = it.next().map(|v| v.parse().expect("--usb-host-ack N")),
            "--usb-host-gone" => {
                let v = it.next().unwrap();
                let (x, y) = v.split_once(':').expect("--usb-host-gone FROM:TO");
                usb_gone.push((num(x), num(y)));
            }
            "--usb-host-sleep" | "--usb-host-noread" => {
                let v = it.next().unwrap();
                let (x, y) = v.split_once(':').map_or((v.as_str(), None), |(x, y)| (x, Some(y)));
                let w = Some((num(x), y.map_or(u64::MAX, num)));
                if a == "--usb-host-sleep" {
                    usb_sleep = w;
                } else {
                    usb_noread = w;
                }
            }
            "--turn" => {
                // --turn ENC:DELTA@CLOCKxCOUNT[/SPACING], one turn every SPACING (4M)
                let v = it.next().unwrap();
                let (ed, rest) = v.split_once('@').unwrap();
                let (e, d) = ed.split_once(':').unwrap();
                let (rest, spacing) = rest.split_once('/').map(|(a, b)| (a, num(b))).unwrap_or((rest, 4_000_000));
                let (at, count) = rest.split_once('x').unwrap_or((rest, "1"));
                for k in 0..count.parse::<u64>().unwrap() {
                    turns.push((num(at) + k * spacing, e.parse().unwrap(), d.parse().unwrap()));
                }
            }
            "--save" => save = it.next().cloned(),
            "--load" => load = it.next().cloned(),
            "--profile-after" => profile_after = Some(num(it.next().unwrap())),
            "--count" => {
                let a = u32::from_str_radix(it.next().unwrap().trim_start_matches("0x"), 16).unwrap();
                counts.push(a);
            }
            "--break-after" => break_after = num(it.next().unwrap()),
            "--trace" => trace = num(it.next().unwrap()) as usize,
            "--poke" => {
                // ADDR=VALUE (hex, 32-bit), written once before the run starts.
                let v = it.next().unwrap();
                let (a, x) = v.split_once('=').expect("--poke ADDR=VALUE");
                let h = |t: &str| u32::from_str_radix(t.trim_start_matches("0x"), 16).expect("--poke hex");
                pokes.push((h(a), h(x)));
            }
            "--watch" => {
                let a = u32::from_str_radix(it.next().unwrap().trim_start_matches("0x"), 16).unwrap();
                watch = Some(a);
            }
            "--watch-read" => {
                let a = u32::from_str_radix(it.next().unwrap().trim_start_matches("0x"), 16).unwrap();
                watch_read = Some((a, num(it.next().unwrap()) as u32));
            }
            _ => syx = Some(a.clone()),
        }
    }
    // --flash FILE without a .syx: boot the OS the flash holds, as a unit does.
    let flash_image = flash.as_ref().and_then(|p| std::fs::read(p).ok());
    let fw = match (&syx, &flash_image) {
        (None, Some(img)) if img.len() > dtemu::machine::FLASH_SLOT => {
            let fw = Firmware::from_flash(&img[dtemu::machine::FLASH_SLOT..]).unwrap_or_else(|e| no_os_in_flash(&e.to_string()));
            println!("booting the OS in the flash: build {} version {}", fw.build, fw.version);
            fw
        }
        _ => {
            let syx = syx.clone().unwrap_or_else(|| "fw/Digitakt_OS1.53.syx".into());
            Firmware::from_file(std::path::Path::new(&syx)).unwrap()
        }
    };
    if let (Some(_), Some(img)) = (&syx, &flash_image) {
        let c = &fw.container;
        let at = dtemu::machine::FLASH_SLOT;
        if img.len() >= at + c.len() && img[at..at + c.len()] != c[..] {
            println!("note: the flash holds a different OS from the .syx; this run boots the .syx's (leave the .syx out to boot the flash's)");
        }
    }
    let booting_flash = syx.is_none() && flash_image.is_some();
    let mut m = Machine::new(&fw, 128).unwrap_or_else(|e| if booting_flash { no_os_in_flash(&e) } else { panic!("{e}") });
    if let Some(i) = ips {
        m.cpu.bus.io.ips = i as f64;
    }
    m.cpu.bus.io.ignore_masks = no_masks;
    if !breaks.is_empty() {
        m.cpu.history = Some((vec![0; 48], 0));
    }
    for c in &counts {
        m.add_counter(*c);
    }
    // DTEMU_HEAP=STATE:PC,PC..  the buddy allocator's low-water mark (Machine::add_heap_probe)
    if let Ok(v) = std::env::var("DTEMU_HEAP") {
        let (st, pcs) = v.split_once(':').expect("DTEMU_HEAP=STATE:PC,PC");
        let hex = |x: &str| u32::from_str_radix(x.trim_start_matches("0x"), 16).unwrap();
        m.add_heap_probe(hex(st), pcs.split(',').map(hex).collect());
    }
    if break_after > 0 {
        m.run_until(break_after);
    }
    for b in &breaks {
        m.add_breakpoint(*b);
    }
    if let Some(a) = watch {
        if a >> 28 == 8 {
            let o = a & 0xFFFF;
            m.cpu.bus.sram_watch = Some((o, o + 4));
        } else {
            let o = a & 0x07FF_FFFF;
            m.cpu.bus.watch = Some((o, o + 4));
        }
    }
    if let Some((a, n)) = watch_read {
        let o = a & 0x07FF_FFFF;
        m.cpu.bus.watch = Some((o, o + n));
        m.cpu.bus.watch_reads = true;
    }
    if test_mode {
        match m.set_test_mode() {
            Some(f) => println!("test mode: boot flag 0x{f:x}"),
            None => println!("test mode: not known for {}", m.prof.name),
        }
    }
    if let Some(p) = &flash {
        match m.load_flash(std::path::Path::new(p)) {
            Ok(true) => {
                println!("flash {p}: loaded");
                flash_loaded = m.cpu.bus.spi.as_ref().map(|f| f.mem.clone());
            }
            Ok(false) => println!("flash {p}: new (built from the .syx)"),
            Err(e) => panic!("--flash {e}"),
        }
    }
    if let Some(p) = &card {
        let fresh = m.attach_card(std::path::Path::new(p)).unwrap();
        println!("card {p}: {} sectors in use{}", m.cpu.bus.io.esdhc.card.sectors.len(), if fresh { " (new, sample area formatted)" } else { "" });
    }
    if let Some(p) = &load {
        dtemu::snapshot::load(&mut m, std::path::Path::new(p)).unwrap();
        println!("restored {p} at clock {}", m.now());
        // A snapshot carries its own flash (an older one does not, and leaves
        // the file's): say so when it replaced a different --flash file.
        if let (Some(before), Some(f)) = (&flash_loaded, m.cpu.bus.spi.as_ref()) {
            if *before != f.mem {
                println!("note: the snapshot's flash differs from --flash's file; running the snapshot's, which is written back to the file at the end");
            }
        }
        instr += m.now();
    }
    // --ob-in FILE.wav[:CH]: a 16-bit WAV's first channel as Overbridge input
    // channel CH (default 4, BD), from the start, at 48 kHz.
    if let Some(spec) = &ob_in {
        let (path, ch) = match spec.rsplit_once(':') {
            Some((p, c)) if c.parse::<usize>().is_ok() => (p, c.parse::<usize>().unwrap()),
            _ => (spec.as_str(), 4),
        };
        for (l, _) in read_wav16(path) {
            let mut f = [0i32; dtemu::oblink::CHANNELS];
            f[ch.min(dtemu::oblink::CHANNELS - 1)] = (l * 2_147_483_647.0) as i32;
            m.ob_in.push_back(f);
        }
        println!("overbridge in: {} frames from {path} on channel {ch}", m.ob_in.len());
    }
    if usb_host {
        let mut h = dtemu::usbhost::UsbHost::new();
        h.suspend = usb_sleep;
        h.midi_sleep = usb_noread;
        h.ack = usb_ack;
        h.midi_every = usb_midi_every;
        h.gone = usb_gone.clone();
        h.cdc = !serials.is_empty();
        m.usb_host = Some(h);
    }
    if usb_midi_real {
        m.set_usb_midi_real(true);
    }
    if let Some(a) = &ob_link {
        m.ob_link = Some(dtemu::oblink::ObLink::listen(a).unwrap_or_else(|e| panic!("{a}: {e}")));
        println!("overbridge link: listening on {a}");
    }
    for (path, ext) in [(&audio_in, false), (&ext_in, true)] {
        if let Some(p) = path {
            let frames = read_wav16(p);
            println!("{} {p}: {} frames", if ext { "ext in" } else { "audio in" }, frames.len());
            let q = if ext { &mut m.cpu.bus.io.ssi.ext_in } else { &mut m.cpu.bus.io.ssi.audio_in };
            q.extend(frames);
        }
    }
    m.cpu.bus.io.usb_audio_capture = usb_wav.is_some();
    m.dsp_capture = dsp_wav.is_some();
    let mut dsp_pcm: Vec<i32> = Vec::new();
    for &(a, x) in &pokes {
        m.cpu.bus.poke32(a, x);
    }
    let t0 = Instant::now();
    let (mut clock0, mut idle0) = (m.now(), m.stats.idle_skipped);
    let mut logged = 0;
    let mut busy_prev = (m.now(), m.stats.idle_skipped);
    let mut target = m.now();
    while target < instr {
        // This chunk's events: [start, target) by their own clocks, whatever clock the CPU overshot to.
        let start = target;
        target = (target + every).min(instr);
        if let Some(pa) = profile_after {
            if m.now() >= pa && m.cpu.profile.is_none() {
                m.cpu.profile = Some(Default::default());
            }
        }
        // Key presses and encoder turns scheduled inside this chunk, in time order.
        // kind: 0 = key down, 1 = key up, 2 = turn
        // For keys the third field indexes `presses`; for turns it is the encoder.
        let mut events: Vec<(u64, u8, usize, i8)> = Vec::new();
        for (i, (_, at, hold)) in presses.iter().enumerate() {
            events.push((*at, 0, i, 0));
            events.push((at + hold, 1, i, 0));
        }
        for &(at, e, d) in &turns {
            events.push((at, 2, e as usize, d));
        }
        for (i, (_, at)) in raws.iter().enumerate() {
            events.push((*at, 3, i, 0));
        }
        for (i, (_, at)) in midis.iter().enumerate() {
            events.push((*at, 4, i, 0));
        }
        for (i, (_, at)) in usb_midis.iter().enumerate() {
            events.push((*at, 5, i, 0));
        }
        for (i, n) in nudges.iter().enumerate() {
            events.push((n.0, 6, i, 0));
        }
        for (i, (_, at)) in serials.iter().enumerate() {
            events.push((*at, 8, i, 0));
        }
        if let Some(at) = power_off {
            events.push((at, 7, 0, 0));
        }
        events.sort();
        // A breakpoint or fault before an event ends the run there.
        let mut early: Option<Stop> = None;
        let mut powered_off = false;
        for (at, kind, code, d) in events {
            if at < start || at >= target {
                continue;
            }
            // An event the run already passed (the CPU stops at block boundaries, so the previous event may have
            // overshot its clock by a few instructions) happens now, not never: two events at one clock both happen.
            if at > m.now() {
                let s = m.run_until(at);
                if matches!(s, Stop::Hook(_) | Stop::Fault(..) | Stop::Reset) {
                    early = Some(s);
                    break;
                }
            }
            if kind == 7 {
                let r = m.power_off(2_000_000_000);
                println!(
                    "[{:>12}] power off: handler {} after {:.1}M instructions, {} sectors written, state slots 0x7a000 {} 0x7c000 {}{}",
                    m.now(),
                    if r.done { "finished" } else if r.armed { "DID NOT FINISH" } else { "not armed (EPORT IRQ2 off: no OS yet?)" },
                    r.instructions as f64 / 1e6,
                    r.sectors_written,
                    if r.slots[0] { "valid" } else { "-" },
                    if r.slots[1] { "valid" } else { "-" },
                    if matches!(r.stop, Stop::Fault(..)) { format!(" ({:?})", r.stop) } else { String::new() }
                );
                if let Some(c) = &r.flash_cut {
                    println!("[{:>12}] power off: flash {c}", m.now());
                }
                if card.is_some() {
                    card_save = true;
                }
                early = Some(r.stop);
                powered_off = true;
                break;
            }
            if kind == 8 {
                if let Some(h) = m.usb_host.as_mut() {
                    h.cdc_to_device.extend(serials[code].0.iter().copied());
                }
                println!("[{:>12}] usb serial out: {:?}", m.now(), String::from_utf8_lossy(&serials[code].0));
                continue;
            }
            if kind == 6 {
                let (_, e, d, func, big) = nudges[code];
                m.nudge_steps(e, d, if big { 8 } else { 1 }, func);
                continue;
            }
            if kind == 5 {
                m.usb_midi_send(&usb_midis[code].0.clone());
                if std::env::var_os("DTBOOT_QUIET_USB").is_none() {
                    println!("[{:>12}] usb midi in: {} bytes", m.now(), usb_midis[code].0.len());
                }
                continue;
            }
            if kind == 4 {
                m.midi_send(&midis[code].0.clone());
                println!("[{:>12}] midi in: {} bytes", m.now(), midis[code].0.len());
                continue;
            }
            if kind == 3 {
                m.panel_send(&raws[code].0.clone());
                println!("[{:>12}] raw {:02x?}", m.now(), raws[code].0);
                continue;
            }
            if kind == 2 {
                m.turn(code as u8, d);
                if let Some(dir) = std::env::var_os("DTBOOT_TURN_PNG") {
                    let settle = m.now() + std::env::var("DTBOOT_TURN_SETTLE").ok().and_then(|v| v.parse().ok()).unwrap_or(3_000_000);
                    m.run_until(settle);
                    step_no += 1;
                    oled_png(&m, &std::path::Path::new(&dir).join(format!("turn{step_no:02}.png")));
                }
                continue;
            }
            let down = kind == 0;
            // A number is an upstream control code (Digitakt); anything else a key name.
            let spec = presses[code].0.clone();
            let label = match spec.parse::<u8>() {
                Ok(c) => {
                    m.key(c, down);
                    dtemu::panel::key_name(c).unwrap_or("?").to_string()
                }
                Err(_) => {
                    if !m.key_named(&spec, down) {
                        eprintln!("no key named {spec:?} on this machine");
                    }
                    spec.to_uppercase()
                }
            };
            println!("[{:>12}] key {} {}", m.now(), label, if down { "down" } else { "up" });
            if !down && std::env::var_os("DTBOOT_STEPS").is_some() {
                let settle = m.now() + 40_000_000;
                m.run_until(settle);
                if let Some(dir) = std::env::var_os("DTBOOT_PNG_DIR") {
                    step_no += 1;
                    oled_png(&m, &std::path::Path::new(&dir).join(format!("step{step_no:02}_{label}.png")));
                } else {
                    print!("{}", Machine::pixels_ascii(&m.panel_pixels()));
                }
            }
        }
        let stop = match early {
            Some(s) => s,
            None => m.run_until(target),
        };
        {
            // The Syntakt's speakers' mix adds its analog side (stanalog.rs).
            let st = m.cpu.bus.st.is_some() && main_wav.is_some();
            let v = &mut m.cpu.bus.io.ssi.voices;
            if st {
                analog_mix.extend(v.iter().copied());
            }
            if analog_wav.is_some() {
                for (l, r) in v.drain(..) {
                    analog.push((l.clamp(-1.0, 1.0) * 8_388_607.0) as i32);
                    analog.push((r.clamp(-1.0, 1.0) * 8_388_607.0) as i32);
                }
            } else {
                v.clear();
            }
        }
        if dsp_wav.is_some() {
            dsp_pcm.extend(m.dsp_take_capture());
        }
        if wav.is_some() || main_wav.is_some() {
            // Whole frames only, so channels keep their places.
            let fw = m.cpu.bus.io.ssi_frame_words().max(1) as usize;
            let out = &mut m.cpu.bus.io.ssi.out;
            let n = out.len() - out.len() % fw;
            pcm.extend(out.drain(..n));
        }
        for l in &m.log[logged..] {
            println!("{l}");
        }
        logged = m.log.len();
        let io = &m.cpu.bus.io;
        let mut top: Vec<(usize, u64)> = io.irq_taken.iter().copied().enumerate().filter(|x| x.1 > 0).collect();
        top.sort_by_key(|b| std::cmp::Reverse(b.1));
        let irqs: Vec<String> = top.iter().take(8).map(|(v, n)| format!("v{v}:{n}")).collect();
        let lit = m.frame.as_ref().map(|f| Machine::lit(f)).unwrap_or(0);
        // busy: the share of this report's clock not skipped in idle spins,
        // the load analoghealth reads as CPU % (a chunk with its idle spins).
        let (dn, di) = (m.now().saturating_sub(busy_prev.0), m.stats.idle_skipped.saturating_sub(busy_prev.1));
        let busy = if dn > 0 { 100.0 * (1.0 - di as f64 / dn as f64) } else { 0.0 };
        busy_prev = (m.now(), m.stats.idle_skipped);
        println!(
            "{:>6.0}M busy={busy:.1}% pc={:08x} sr={:04x} idle={} frames={} lit={} tasks={} mainloop={} irq[{}] {:.1}s {}",
            m.now() as f64 / 1e6,
            m.cpu.pc,
            m.cpu.sr,
            m.stats.idle_hits,
            m.stats.frames,
            lit,
            m.stats.tasks.len(),
            m.stats.mainloop,
            irqs.join(" "),
            t0.elapsed().as_secs_f64(),
            m.counters.iter().map(|(a, n)| format!("{a:08x}:{n}")).collect::<Vec<_>>().join(" ")
        );
        if let Ok(list) = std::env::var("DTBOOT_PEEK") {
            // DTBOOT_PEEK=ADDR,ADDR,...: those words (hex) on each status line
            let w: Vec<String> = list
                .split(',')
                .filter_map(|a| u32::from_str_radix(a.trim_start_matches("0x"), 16).ok())
                .map(|a| format!("[{a:08x}]={:08x}", m.cpu.bus.peek32(a)))
                .collect();
            println!("  peek {} usb_midi_out={}", w.join(" "), m.cpu.bus.io.usb_midi_out.len());
        }
        if let Stop::Fault(v, p) = stop {
            println!("FAULT vector {v} at 0x{p:08x}");
            break;
        }
        if let Stop::Hook(pc) = stop {
            println!("BREAK at 0x{pc:08x}");
            let sp = m.cpu.a[7];
            let st: Vec<String> = (0..6).map(|k| format!("{:08x}", m.cpu.bus.peek32(sp + 4 * k))).collect();
            println!("stack at a7={sp:08x}: {}", st.join(" "));
            if let Some((h, i)) = &m.cpu.history {
                let n = h.len();
                let from = i.saturating_sub(n);
                let pcs: Vec<String> = (from..*i).map(|k| format!("{:08x}", h[k % n])).collect();
                println!("history: {}", pcs.join(" "));
            }
            for _ in 0..trace {
                let c = &m.cpu;
                let op = c.bus.peek16(c.pc);
                println!("  {:08x} {:04x}  sr={:04x} d0={:08x} d1={:08x} a0={:08x} a7={:08x}", c.pc, op, c.sr, c.d[0], c.d[1], c.a[0], c.a[7]);
                m.cpu.step();
            }
            break;
        }
        if let Stop::Stopped = stop {
            println!("STOPPED with nothing to wake it at pc=0x{:08x}", m.cpu.pc);
            break;
        }
        if let Stop::Reset = stop {
            let at = m.now();
            println!("[{at:>12}] reset (RCR SOFTRST) at pc=0x{:08x}", m.cpu.pc);
            if let Some(f) = m.cpu.bus.spi.as_ref() {
                println!("{}", f.summary());
            }
            if !reboot {
                break;
            }
            match m.reboot_from_flash() {
                Ok(name) => {
                    println!("[{at:>12}] rebooting from the flash: {name}");
                    // The new machine starts without the tools' attachments.
                    if usb_host {
                        let mut h = dtemu::usbhost::UsbHost::new();
                        h.suspend = usb_sleep;
                        h.midi_sleep = usb_noread;
                        h.ack = usb_ack;
                        h.midi_every = usb_midi_every;
                        h.cdc = !serials.is_empty();
                        m.usb_host = Some(h);
                    }
                }
                Err(e) => {
                    println!("cannot reboot: {e}");
                    break;
                }
            }
            // The new boot's clock starts at 0: what was due is done.
            presses.retain(|p| p.1 > at);
            turns.retain(|t| t.0 > at);
            raws.retain(|r| r.1 > at);
            midis.retain(|x| x.1 > at);
            usb_midis.retain(|x| x.1 > at);
            nudges.retain(|n| n.0 > at);
            serials.retain(|x| x.1 > at);
            power_off = power_off.filter(|&t| t > at);
            instr -= at.min(instr);
            target = 0;
            (clock0, idle0) = (0, 0);
            logged = 0;
            continue;
        }
        if powered_off {
            break;
        }
    }
    if let Some(p) = &main_wav {
        let fw = m.cpu.bus.io.ssi_frame_words().max(2) as usize;
        let st: Vec<i32> = pcm.chunks(fw).enumerate().flat_map(|(i, f)| {
            let (l, r) = m.speaker_pair(f);
            let (al, ar) = analog_mix.get(i).copied().unwrap_or((0.0, 0.0));
            [l + al, r + ar].map(|x| (x.clamp(-1.0, 1.0) * 8_388_607.0) as i32)
        }).collect();
        write_wav(p, &st, 2);
        let peak = st.iter().map(|x| x.unsigned_abs()).max().unwrap_or(0);
        println!("wrote {p}: {} stereo frames (the speakers' mix), peak {peak} (of 8388607)", st.len() / 2);
    }
    if let Some(p) = &dsp_wav {
        // The DSP's words are 32-bit; write_wav takes 24.
        let w: Vec<i32> = dsp_pcm.iter().map(|&x| x >> 8).collect();
        write_wav(p, &w, dtemu::syntakt::TRACKS as u16);
        let peak = dsp_pcm.iter().map(|x| x.unsigned_abs()).max().unwrap_or(0);
        println!("wrote {p}: {} frames of tracks 1..8 as the DSP rendered them, peak {peak} (of 2^31)", dsp_pcm.len() / dtemu::syntakt::TRACKS);
    }
    if let Some(p) = &wav {
        write_wav(p, &pcm, m.cpu.bus.io.ssi_frame_words().max(2) as u16);
        let peak = pcm.iter().map(|x| x.unsigned_abs()).max().unwrap_or(0);
        let nz = pcm.iter().filter(|&&x| x != 0).count();
        println!("wrote {p}: {} frames, peak {peak} (of 8388607), {nz} nonzero samples", pcm.len() / 2);
    }
    if let Some(p) = &flash {
        if let Some(f) = m.cpu.bus.spi.as_ref() {
            println!("{}", f.summary());
        }
        m.save_flash(std::path::Path::new(p)).expect("--flash");
        println!("flash written back to {p}");
    }
    if card_save {
        m.cpu.bus.io.esdhc.card.dirty = true;
        m.flush_card().expect("--card-save");
        println!("card written back ({} sectors)", m.cpu.bus.io.esdhc.card.sectors.len());
    }
    if let Some(p) = &save {
        dtemu::snapshot::save(&m, std::path::Path::new(p)).unwrap();
        println!("saved {p} ({} bytes)", std::fs::metadata(p).unwrap().len());
    }
    let secs = t0.elapsed().as_secs_f64();
    let executed = (m.now() - clock0) - (m.stats.idle_skipped - idle0);
    println!(
        "{} instructions ({} executed, {} idle-skipped) in {:.2}s = {:.1} MIPS executed, {:.0}% of real time",
        m.now() - clock0,
        executed,
        m.stats.idle_skipped - idle0,
        secs,
        executed as f64 / secs / 1e6,
        (m.now() - clock0) as f64 / m.cpu.bus.io.ips / secs * 100.0
    );
    let exc: Vec<String> = m.cpu.exc_counts.iter().enumerate().filter(|x| *x.1 > 0 && x.0 < 64).map(|(v, n)| format!("{v}:{n}")).collect();
    println!("cpu exceptions: {}", exc.join(" "));
    if let Some(h) = &m.heap_probe {
        let end = m.heap_free(h.state);
        println!(
            "heap: lowest free {} B at clock {} ({} live allocations), most live {}, {} samples; at the end {:?} (free B, live)",
            h.min_free, h.min_at, h.min_live, h.max_live, h.samples, end
        );
    }
    for (v, pc, op, t) in &m.cpu.fault_log {
        println!("  fault vector {v} at 0x{pc:08x} opcode {op:04x} (clock {t})");
    }
    if m.usb_midi_real && m.prof.name.contains("1.73") {
        let b = &m.cpu.bus;
        // The sender's request pool (OS 1.73): free list 0x413db390, next at +52.
        let (mut free, mut r) = (0, b.peek32(0x413D_B390));
        while r != 0 && free < 64 {
            free += 1;
            r = b.peek32(r + 52);
        }
        println!("usb midi real: free requests {free}, sender ring fill {}, waits from interrupt level: {}", b.peek32(0x413D_B258), m.isr_pends.len());
        for ((lv, sem, ret), (t, n)) in &m.isr_pends {
            println!("  BLOCKING sem_pend(0x{sem:08x}) at level {lv}, return 0x{ret:08x}: first at clock {t}, {n} times");
        }
    }
    if let Some(h) = &m.usb_host {
        if h.cdc {
            println!("usb serial in ({} bytes):\n{}", h.cdc_from_device.len(), String::from_utf8_lossy(&h.cdc_from_device));
        }
        let b = &m.cpu.bus;
        println!("usb regs: cmd {:08x} sts {:08x} intr {:08x} portsc {:08x} otgsc {:08x} mode {:08x} eplist {:08x} epctrl1 {:08x}",
            b.peek32(0xFC0B_0140), b.peek32(0xFC0B_0144), b.peek32(0xFC0B_0148), b.peek32(0xFC0B_0184), b.peek32(0xFC0B_01A4),
            b.peek32(0xFC0B_01A8), b.peek32(0xFC0B_0158), b.peek32(0xFC0B_01C4));
        println!("usb host: streaming={} {:?}", h.streaming(), h.stats);
        if h.device_stack && !h.from_rytm.is_empty() {
            let mut peak = [0u32; dtemu::usbhost::OB_CHANNELS];
            for fr in &h.from_rytm {
                for (p, x) in peak.iter_mut().zip(fr) {
                    *p = (*p).max(x.unsigned_abs());
                }
            }
            println!("usb host: stream IN, last {} frames, peak per channel {peak:x?}", h.from_rytm.len());
        }
        println!("usb host: device descriptor {:02x?}", h.device_desc);
        println!("usb host: configuration ({} bytes) {:02x?}", h.config_desc.len(), h.config_desc);
    }
    if let Some(s) = m.ob_link_report() {
        println!("{s}");
    }
    let uo = &m.cpu.bus.io.usb_midi_out;
    if !uo.is_empty() {
        println!("usb midi out ({} bytes): {}", uo.len(), uo.iter().take(64).map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));        // DTBOOT_USB_OUT=FILE: all of it, raw.
        if let Ok(f) = std::env::var("DTBOOT_USB_OUT") {
            let v: Vec<u8> = uo.iter().copied().collect();
            std::fs::write(&f, v).expect("DTBOOT_USB_OUT");
        }
    }
    let mo = &m.cpu.bus.io.midi_out;
    if let Some(p) = &usb_wav {
        let s = &m.cpu.bus.io.usb_audio;
        Machine::write_wav16(std::path::Path::new(p), s, 12).unwrap();
        println!("usb audio: {} frames to {p}", s.len() / 12);
    }
    if let Some(p) = &analog_wav {
        write_wav(p, &analog, 2);
    }
    if let Some(f) = &midi_out_file {
        std::fs::write(f, mo.iter().copied().collect::<Vec<u8>>()).unwrap();
    }
    if !mo.is_empty() {
        println!("midi out ({} bytes): {}", mo.len(), mo.iter().take(64).map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
    }
    let p = &m.cpu.bus.io.panel;
    if p.unknown > 0 {
        println!("panel: {} messages not understood; last ones: {:02x?}", p.unknown, p.unknown_log.iter().rev().take(12).collect::<Vec<_>>());
    }
    let pages: Vec<String> = m.cpu.bus.unmapped_touch.iter().filter(|(p, _)| **p >= 0xE000_0000).map(|(p, (pc, n))| format!("{p:08x}(pc {pc:08x} x{n})")).collect();
    println!("unmodelled io pages: {}", pages.join(" "));
    // Every page no model claims that was touched, I/O or not (first pc).
    for (p, (pc, n)) in &m.cpu.bus.unmapped_touch {
        println!("unmapped_touch: page {p:08x} first pc {pc:08x} io-count {n}");
    }
    for ((pc, page, w), (a, n)) in &m.cpu.bus.strict_log {
        println!("strict: pc {pc:08x} {} page {page:08x} first {a:08x} x{n}", if *w { "write" } else { "read " });
    }
    if let Some(c) = &m.cpu.bus.cache {
        print!("{}", c.report());
    }
    if let Some(t) = &m.cpu.bus.timing {
        println!("{}", t.summary(m.now()));
    }
    print!("{}", m.task_report());
    let vt: Vec<String> = [97u32, 99, 154, 155, 180, 191, 192, 205, 207, 208, 223].iter().map(|v| format!("v{v}={:08x}", m.cpu.bus.peek32(m.cpu.vbr + v * 4))).collect();
    println!("vectors: {}", vt.join(" "));
    if let Some(p) = &m.cpu.profile {
        let total: u64 = p.values().sum();
        // DTEMU_PROFILE_DUMP=FILE: every sampled block start and its count (one sample per 64 instructions)
        if let Some(f) = std::env::var_os("DTEMU_PROFILE_DUMP") {
            let mut v: Vec<(u32, u64)> = p.iter().map(|(a, n)| (*a, *n)).collect();
            v.sort();
            let txt: String = v.iter().map(|(a, n)| format!("{a:08x} {n}\n")).collect();
            std::fs::write(f, txt).expect("DTEMU_PROFILE_DUMP");
        }
        let mut by_fn: std::collections::HashMap<u32, u64> = Default::default();
        for (pc, n) in p {
            *by_fn.entry(pc & !0xFF).or_insert(0) += n;
        }
        let mut v: Vec<_> = by_fn.into_iter().collect();
        v.sort_by_key(|b| std::cmp::Reverse(b.1));
        let mut ops: std::collections::HashMap<u16, u64> = Default::default();
        for (pc, n) in p {
            *ops.entry(m.cpu.bus.peek16(*pc)).or_insert(0) += n;
        }
        let mut ov: Vec<_> = ops.into_iter().collect();
        ov.sort_by_key(|b| std::cmp::Reverse(b.1));
        let mut acc = 0.0;
        println!("opcodes:");
        for (o, n) in ov.iter().take(40) {
            let f = *n as f64 * 100.0 / total as f64;
            acc += f;
            println!("  {o:04x} {f:5.2}% (cum {acc:5.1}%)");
        }
        println!("distinct opcodes: {}", ov.len());
        let mut blocks: Vec<(u32, u64)> = p.iter().map(|(a, n)| (*a, *n)).collect();
        blocks.sort_by_key(|b| std::cmp::Reverse(b.1));
        println!("hottest blocks:");
        for (a, n) in blocks.iter().take(20) {
            println!("  {a:08x} {:5.1}%", *n as f64 * 100.0 / total as f64);
        }
        println!("guest profile ({total} samples), by 256-byte region:");
        for (a, n) in v.iter().take(25) {
            println!("  {a:08x} {:5.1}%", *n as f64 * 100.0 / total as f64);
        }
    }
    println!("+Drive mounted: {}", m.drive_mounted());
    if let Some(d) = m.cpu.bus.io.dsp.as_ref() {
        println!(
            "dsp link: {:?}, version {:#06x}, boot stream {} B, kicks {}, irqs {}, data blocks {}, audio blocks {}",
            d.dsp.phase, d.dsp.version, d.boot_stream.len(), d.kicks, d.irqs, d.dsp.blocks, d.dsp.frames
        );
        if let Some(c) = m.dsp.as_ref() {
            let st = &m.dsp_stats;
            let ran = c.bus.io.now - st.started_at;
            println!(
                "dsp core: boots {}, pc {:08x}, sr {:04x}, ran {} (idle {}: {:.1}% busy), kicks taken {}, irq edges {}, mbox +0..+10 {:02x?}, fault {:?}",
                st.boots,
                c.pc,
                c.sr,
                ran,
                st.idle_skipped,
                100.0 * (ran - st.idle_skipped.min(ran)) as f64 / ran.max(1) as f64,
                c.bus.io.dsp_side.as_ref().map_or(0, |s| s.kicks_taken),
                d.irqs,
                &d.mbox[0..16],
                st.fault
            );
            if let Some(p) = c.profile.as_ref() {
                let total: u64 = p.values().sum();
                let mut v: Vec<(u32, u64)> = p.iter().map(|(a, n)| (*a, *n)).collect();
                v.sort_by_key(|b| std::cmp::Reverse(b.1));
                println!("dsp hottest blocks ({total} samples):");
                for (a, n) in v.iter().take(20) {
                    println!("  {a:08x} {:5.1}%", *n as f64 * 100.0 / total.max(1) as f64);
                }
            }
        } else if m.dsp_real() {
            println!("dsp core: not started");
        }
        if let Ok(f) = std::env::var("DTBOOT_DSP_STREAM") {
            std::fs::write(&f, &d.boot_stream).expect("DTBOOT_DSP_STREAM");
        }
    }
    println!("icache: {} decodes, {} invalidating stores", m.cpu.bus.icache_decodes, m.cpu.bus.icache_invalidations);
    let c = &m.cpu;
    for (pc, a, n, v) in c.bus.watch_log.iter().take(40) {
        println!("watch: pc={pc:08x} [{a:08x}].{n} <- {v:08x}");
    }
    println!("d: {}", c.d.iter().map(|v| format!("{v:08x}")).collect::<Vec<_>>().join(" "));
    println!("a: {}", c.a.iter().map(|v| format!("{v:08x}")).collect::<Vec<_>>().join(" "));
    println!("macsr: {:08x}", c.macsr);
    let sp = c.a[7];
    println!("stack: {}", (0..24).map(|i| format!("{:08x}", c.bus.peek32(sp + 4 * i))).collect::<Vec<_>>().join(" "));
    println!("uart8 tx ({} bytes): {}", c.bus.io.uart8.tx.len(), c.bus.io.uart8.tx.iter().take(64).map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
    let el = &c.bus.io.esdhc.log;
    let log: Vec<String> = el.iter().skip(el.len().saturating_sub(24)).map(|(i, a)| format!("CMD{i}({a:x})")).collect();
    println!("esdhc: {} commands: {}", c.bus.io.esdhc.log.len(), log.join(" "));
    for (i, ic) in c.bus.io.intc.iter().enumerate() {
        let lv: Vec<String> = ic.icr.iter().enumerate().filter(|x| *x.1 != 0).map(|(s, l)| format!("{s}:{l}")).collect();
        println!("intc{i}: imr={:016x} frc={:016x} lines={:016x} icr {}", ic.imr, ic.frc, ic.lines, lv.join(" "));
    }
    for (i, p) in c.bus.io.pit.iter().enumerate() {
        println!("pit{i}: pcsr={:04x} pmr={:04x} fired={}", p.pcsr, p.pmr, p.fired);
    }
    for (i, d) in c.bus.io.dtim.iter().enumerate() {
        println!("dtim{i}: dtmr={:04x} dter={:02x} dtrr={:08x} fired={}", d.dtmr, d.dter, d.dtrr, d.fired);
    }
    let ssi = &c.bus.io.ssi;
    let nz = ssi.out.iter().filter(|&&x| x != 0).count();
    let peak = ssi.out.iter().map(|x| x.unsigned_abs()).max().unwrap_or(0);
    println!("ssi: frames={} buffered={} nonzero={} peak={} tx_pending={}", ssi.frames, ssi.out.len(), nz, peak, ssi.tx_pending);
    println!("edma: erq={:016x} int={:016x} majors[34]={} [35]={} [59]={}", c.bus.io.edma.erq, c.bus.io.edma.int, c.bus.io.edma.majors[34], c.bus.io.edma.majors[35], c.bus.io.edma.majors[59]);
    if std::env::var_os("DTBOOT_OLED").is_some() {
        println!("panel MCU OLED ({} frames):", m.cpu.bus.io.panel.oled_frames);
        print!("{}", Machine::pixels_ascii(&m.panel_pixels()));
    } else if let Some(f) = &m.frame {
        print!("{}", Machine::frame_ascii(f));
    }
    if let Some(p) = std::env::var_os("DTBOOT_END_PNG") {
        oled_png(&m, std::path::Path::new(&p));
    }
    if std::env::var_os("DTBOOT_LEDS").is_some() {
        // The LED selector bytes per group (`0x2g`/`0x3g`): on the MKI units
        // each is 8 single-colour LEDs, one bit each.
        let sel: Vec<String> = m.cpu.bus.io.panel.selectors.iter().map(|s| s.map_or("--".into(), |v| format!("{v:02x}"))).collect();
        println!("led groups: {}", sel.join(" "));
        if std::env::var("DTBOOT_LEDS").is_ok_and(|v| v == "rgb") {
            // DTBOOT_LEDS=rgb: every colour LED that is defined, as 8-bit RGB.
            let p = &m.cpu.bus.io.panel;
            let lit: Vec<String> = (0..dtemu::panel::ALL_LEDS).filter_map(|i| p.led_rgb(i).map(|(r, g, b)| format!("{i}:{r:02x}{g:02x}{b:02x}"))).collect();
            println!("led rgb: {}", lit.join(" "));
        }
    }
}

/// 16-bit stereo WAV at 48 kHz from 24-bit samples.
/// One channel per SSI frame word: 2 on the Digitakt, 16 on the Rytm (digital
/// layers 0..7, tom exciters 8..10, effect returns 12..15).
fn write_wav(path: &str, pcm: &[i32], chans: u16) {
    let data: Vec<u8> = pcm.iter().flat_map(|&s| ((s >> 8) as i16).to_le_bytes()).collect();
    let mut f = Vec::new();
    f.extend_from_slice(b"RIFF");
    f.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    f.extend_from_slice(b"WAVEfmt ");
    f.extend_from_slice(&16u32.to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&chans.to_le_bytes());
    f.extend_from_slice(&48_000u32.to_le_bytes());
    f.extend_from_slice(&(48_000u32 * 2 * chans as u32).to_le_bytes());
    f.extend_from_slice(&(2 * chans).to_le_bytes());
    f.extend_from_slice(&16u16.to_le_bytes());
    f.extend_from_slice(b"data");
    f.extend_from_slice(&(data.len() as u32).to_le_bytes());
    f.extend_from_slice(&data);
    std::fs::write(path, f).unwrap();
}

fn oled_png(m: &Machine, path: &std::path::Path) {
    // The MKII's 128x64 OLED, or the MKI's 122x32 LCD.
    let rows = m.panel_pixels();
    let (w, h) = (rows[0].len(), rows.len());
    let mut cv = dtemu::gui::Canvas::new(w * 3, h * 3);
    for (y, row) in rows.iter().enumerate() {
        for (x, &on) in row.iter().enumerate() {
            cv.fill(x as i32 * 3, y as i32 * 3, 3, 3, if on { 0xFFFFFF } else { 0 });
        }
    }
    dtemu::gui::save_png(&cv, path).unwrap();
}

/// A 16-bit PCM WAV as stereo frames at its own rate (mono is doubled).
fn read_wav16(path: &str) -> Vec<(f32, f32)> {
    let b = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut i = 12;
    let mut chans = 2usize;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let n = u32::from_le_bytes(b[i + 4..i + 8].try_into().unwrap()) as usize;
        if id == b"fmt " {
            chans = u16::from_le_bytes([b[i + 10], b[i + 11]]) as usize;
        }
        if id == b"data" {
            let d = &b[i + 8..(i + 8 + n).min(b.len())];
            let s: Vec<f32> = d.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).collect();
            return s.chunks(chans.max(1)).map(|f| (f[0], *f.get(1).unwrap_or(&f[0]))).collect();
        }
        i += 8 + n + (n & 1);
    }
    Vec::new()
}
