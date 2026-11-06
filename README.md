# rytm-emu - an emulator for the Analog Rytm and Analog Four, MKI and MKII, and the Syntakt (work in progress)

| Device | OS | Firmware file (yours; not included) | Start |
|---|---|---|---|
| Analog Rytm MKII | 1.73 | `fw/Analog-Rytm_MKII_OS1.73.syx` | `./target/release/rytm --card rytm.img` |
| Analog Four MKII | 1.56 | `fw/Analog-Four_MKII_OS1.56.syx` | `./target/release/a4` (card `a4.img`) |
| Analog Rytm MKI | 1.74 | `fw/Analog-Rytm_OS1.74.syx` | `./target/release/rytm fw/Analog-Rytm_OS1.74.syx --card rytmmk1.img` |
| Analog Four MKI / Analog Keys | 1.56 | `fw/Analog-Four_Analog-Keys_OS1.56.syx` | `./target/release/rytm fw/Analog-Four_Analog-Keys_OS1.56.syx --card a4mk1.img` |
| Syntakt (main screen; digital tracks 1-8 and modelled analog tracks 9-12 sound) | 1.42 | `fw/Syntakt_OS1.42.syx` | `./target/release/rytm fw/Syntakt_OS1.42.syx --card syntakt.img` |
| Digitakt MK1 (upstream) | 1.53 | `fw/Digitakt_OS1.53.syx` | `./target/release/rytm fw/Digitakt_OS1.53.syx` |
| Digitakt MK1 | 1.54 | `fw/Digitakt_OS1.54.syx` | `./target/release/rytm fw/Digitakt_OS1.54.syx` |

**New to this? Follow [`docs/INSTALL.md`](docs/INSTALL.md)**: a step-by-step guide from installing Rust to the first boot.

The emulator picks the device from the firmware file. Make a card image once per device with
`./target/release/dtcard NAME.img format` (MKI and MKII cards are not interchangeable).
In the window: click keys, drag or scroll a knob to turn it, click a knob to push it,
ctrl-drag to push and turn.

Goal: run the Analog Rytm MKII's own OS, stock or patched by
[`rytm-mods`](../rytm-mods), on emulated hardware, to check that a firmware boots
and to see and drive its UI before anything is flashed. The analog voice circuits
are stubbed: the CPU side runs for real, and what it sends to the voice hardware is
accepted and can be inspected.

This is a fork of [rouletrro](https://github.com/Ondrysak/rouletrro) (a Digitakt
MK1 emulator in Rust, MIT per its `Cargo.toml`), whose upstream README is in
[`docs/UPSTREAM-README.md`](docs/UPSTREAM-README.md). The Digitakt MK1 and the Rytm
MKII appear to share a CPU (ColdFire V4e, MCF5441x family), a memory map, an OS
framework and a container format, so most of that work carries over. See
[`docs/RYTM-PORT.md`](docs/RYTM-PORT.md) for the evidence and the plan.

**Status: Analog Rytm MKII OS 1.73 boots** to its main screen and answers keys;
see [`docs/RYTM-PORT.md`](docs/RYTM-PORT.md). Digitakt OS 1.53 still runs as before.

**Analog Four MKII OS 1.56 boots** to its main UI too (`fw/Analog-Four_MKII_OS1.56.syx`,
card image e.g. `a4.img`); see [`docs/A4-PORT.md`](docs/A4-PORT.md). The emulator picks
the Rytm, the A4 or the Digitakt by the firmware's hash. On an A4 the window draws the
A4's panel (keys, the 25-key keyboard, encoders A..J, LEDs) and plays a model of its
analog voices from the OS's voice frame, with the OS's own digital chorus, delay and
reverb (the voices go to the codec input, the effects' return is mixed back), in tune
to within about a cent (the frequency word is a 24.576 MHz NCO increment); its MIDI
ports are `a4-emu in/out`. A synthetic calibration record keeps "MISSING CAL" away
(`DTEMU_NO_CAL=1` for none).

    ./target/release/a4      # the window: rytm with fw/Analog-Four_MKII_OS1.56.syx and --card a4.img as defaults
    ./target/release/rytm fw/Analog-Four_MKII_OS1.56.syx --card a4.img   # the same
    ./target/release/dtboot fw/Analog-Four_MKII_OS1.56.syx --instr 3G --card a4.img

**Analog Four MKI OS 1.56 boots** to its main screen as well (`fw/Analog-Four_Analog-Keys_OS1.56.syx`,
an ELE2 file; on a copy of an A4 MKII card such as `a4.img`, or on a blank card, which it
initialises as the bootstrap's FACTORY RESET does); see
[`docs/MK1-PORT.md`](docs/MK1-PORT.md). The window draws the A4 MKI's panel with its
122x32 LCD and single-colour LEDs; keys, the 13-key keyboard and encoders A..J work, SSI
audio and the A4 voice model run, and a synthetic calibration record sits in the
emulated NOR flash. **Analog Rytm MKI OS 1.74 boots** to its main screen on a blank card
(`fw/Analog-Rytm_OS1.74.syx`; it lays out its own +Drive, so not on a Rytm MKII card), after
closing its start-up popups with NO; open items are in the same document.

**Syntakt OS 1.42 boots** to its main screen (`fw/Syntakt_OS1.42.syx`, on a `dtcard`-formatted
card or none); see [`docs/SYNTAKT-PORT.md`](docs/SYNTAKT-PORT.md). The Syntakt has a second
ColdFire for its digital voices behind an FPGA mailbox. It runs here as a second emulated
CPU with the firmware's own DSP program (section 7): the main CPU's boot handshake reports
"DSP OK", and the digital tracks 1..8 sound (the pattern, the keys, MIDI). The analog tracks
9..12 and the analog FX block are modelled from the 500-word voice frame the firmware sends
them (`src/stanalog.rs`, section 12): two oscillators with pitch and amp envelopes per drum
voice, the cymbal voice's oscillator banks, the digital layers, overdrive, the analog filter
and VCA per track, and the FX block's drive, filter and amp; pitch within a cent of the note,
curves and levels inferred (`DTEMU_NO_ANALOG=1` turns the model off).
`DTEMU_SYNTAKT_DSP=stub` swaps the second CPU for the stage 1 scripted stand-in (silent).
The window draws the Syntakt's panel (keys, encoders A..H and Level/Data, the RGB key LEDs)
and plays the digital mix; its MIDI ports are `syntakt-emu in/out`.

    ./target/release/dtcard syntakt.img format
    ./target/release/rytm fw/Syntakt_OS1.42.syx --card syntakt.img
    ./target/release/dtboot fw/Syntakt_OS1.42.syx --instr 4G --card syntakt.img
    ./target/release/dtboot fw/Syntakt_OS1.42.syx --instr 6G --press PLAY@4G \
        --main-wav main.wav --dsp-wav tracks.wav    # the speakers' mix; tracks 1..8 as rendered
    ./target/release/dtboot fw/Syntakt_OS1.42.syx --instr 5G --midi 983c64@4.5G \
        --analog-wav analog.wav                     # track 9's note: the analog side alone

    ./target/release/rytm fw/Analog-Four_Analog-Keys_OS1.56.syx --card a4mk1.img   # a copy of an A4 card, or a new image
    ./target/release/dtboot fw/Analog-Four_Analog-Keys_OS1.56.syx --instr 3G --card a4mk1.img
    ./target/release/dtboot fw/Analog-Rytm_OS1.74.syx --instr 3700M --card rytmmk1.img --card-save \
        --press NO@3000M --press NO@3300M     # a new image: initialised on the first boot

Interactive, in a window (the firmware defaults to `fw/Analog-Rytm_MKII_OS1.73.syx`):

    ./target/release/rytm --card rytm.img --snapshot rytm.snap

Headless, for scripted tests (runs a fixed budget, prints the screen, exits):

    ./target/release/dtboot fw/Analog-Rytm_MKII_OS1.73.syx --instr 4G --card rytm.img \
        --press YES@3000M --press NO@3400M

The window draws the loaded device's front panel. Mouse: click a key to press it,
shift-click to latch it (again to release; Esc releases all). On a knob, drag
(up or right is clockwise) or use the wheel to turn it; a click without a drag
pushes it; ctrl-click (or cmd-click) holds its push, so ctrl-drag is push + turn.
The arrow keys nudge the last knob touched (up/down 1, left/right 8). On a Rytm
pad, clicking higher strikes harder. Every key is also reachable by name in
`dtboot` (`panel::RYTM_KEYS` etc.).

**No firmware is included or ever committed.** Bring your own `.syx` from Elektron;
`*.syx`, extracted sections, snapshots and card images are git-ignored.

**MIDI**: the window creates two virtual MIDI ports, `rytm-emu in` and `rytm-emu out`,
wired to the emulated DIN MIDI port (`--no-midi` to skip). Connect a keyboard, a DAW
or a SysEx tool to them as to a real Rytm's DIN MIDI. Windows has no virtual MIDI
ports: there the window opens existing ports with those names, so create them first
with a loopback driver such as loopMIDI (`rytm-emu in`, `rytm-emu out`, and for the
USB MIDI port `rytm-emu usb in` / `rytm-emu usb out`; `a4-emu ...` for the A4); a missing one is reported
and skipped. Headless tests can send MIDI
with `dtboot --midi 9d0064@5G` or a SysEx file with `--midi-file dump.syx@5G`.
`DTEMU_MIDI_LOG=FILE` logs every message through the ports. Elektron's own protocol
(Transfer, elektroid), which the Rytm only serves over USB, is answered on these
ports too: the emulator hands it to the OS as USB input and returns the replies
(docs/RYTM-PORT.md, DIN MIDI). `tools/ekproto.py` encodes and decodes it for tests.

By default the window takes the OS's USB MIDI sender calls itself, and the sender's own state
stays as with no host: port not configured (0x413db254 = 0 on the Rytm MKII 1.73/1.74),
endpoint 1 off. Code that checks that state before it sends, such as a mod's own SysEx replies,
then sends nothing. `--usb-midi-real` (window, implies `--usb-host`; dtboot's `--usb-host
--usb-midi-real`) runs the OS's own sender instead, read by the simulated USB host, and keeps
both across an OS upgrade's reboot. Its output reaches `rytm-emu usb out` and `--usb-midi-tcp` as before.

**OS upgrades** (MKII units): the SPI flash is writable (`src/spiflash.rs`), so an OS
upgrade sent by Transfer, elektroid or elkflash runs as on a unit: the transfer, the
UPGRADE OS NOW? prompt, ERASING FLASH..., the write and the reset. With `--flash
FILE` (window and dtboot) the flash lives in FILE and survives the run; the window
then boots the new OS, and `dtboot --reboot` does the same. With no `.syx` named, the
tools boot the OS the flash holds. A flash image holds the OS: name it `*.img` so git
ignores it. Nothing in the emulator reaches a real unit.

    ./target/release/dtboot fw/Analog-Rytm_MKII_OS1.73.syx --card rytm.img --flash rytm-flash.img \
        --reboot --instr 26G --usb-midi-file upgrade.syx@5G --press YES@18G
    ./target/release/dtboot --card rytm.img --flash rytm-flash.img --instr 4G   # boots the upgraded OS

**Overbridge input** (Rytm): `--ob-link [ADDR]` (window and dtboot) serves the
host->Rytm Overbridge stream over TCP (default 127.0.0.1:7301; `src/oblink.rs`):
a host such as rytm-teensy's `sim/obhost.py` sends 12 channels a frame and each
voice's channel plays through that voice's analog chain while it is triggered.
`dtboot --ob-in FILE.wav[:CH]` plays a WAV into channel CH (4..11 = BD SD RS/CP BT
LT MT/HT CH/OH CY/CB).

**Firmware**: stock Analog Rytm MKII OS 1.73 and 1.74, and modified images built from
them (rytm-mods builds), recognised by keeping stock code where the emulator hooks.
Use a separate `--snapshot` per firmware image; snapshots do not cross images.

**Checking the window without looking at it**: `DTEMU_WIN_DUMP=DIR[:SECS]` writes the canvas and, on macOS, what the window
server composites for this window (a process may capture its own window) as PNGs every SECS, with the status line and the lit
OLED pixels on stderr. It needs a real window, so not for runs beside someone using the Mac; `--headless --screenshot` draws the same canvas without one.

**Digitakt MK1, OS 1.53 and 1.54** (new):

- The firmware's own USB stack runs with `--usb-host`: enumeration from a cold boot, MIDI both ways through the OS's own sender, and
  the Overbridge stream, see [`docs/DIGITAKT-USB.md`](docs/DIGITAKT-USB.md). None of it was compared with a real Digitakt.
- OS 1.54 has its own profile.
- The window draws a hardware-style Digitakt panel.
- The window has USB MIDI ports on a Digitakt, `rytm-emu usb in/out`, and `--usb-midi-tcp` works there too. They carry MIDI with `--usb-host`.
- A patched OS boots on the stock profile with `DTEMU_DIGITAKT_153=1` or `DTEMU_DIGITAKT_154=1`. Without `--usb-host`, a stand-in takes the
  OS's USB MIDI sender.
- Test peers in `dtboot`: `--usb-host-ack N` answers each SysEx, `--usb-host-midi-every N` reads MIDI slowly, `DTEMU_USB_SEND_LOG=1` logs each
  message and `DTBOOT_USB_OUT=FILE` keeps them.

To run one:

    ./target/release/dtcard digitakt.img format
    ./target/release/rytm fw/Digitakt_OS1.53.syx --card digitakt.img --usb-host      # the window, USB host attached
    ./target/release/dtboot fw/Digitakt_OS1.54.syx --card digitakt.img --usb-host --instr 25G   # no window, prints what enumerated
    DTEMU_DIGITAKT_153=1 ./target/release/rytm my_patched_os.syx --card digitakt.img --usb-host  # a patched 1.53

A fresh card comes up in USB MIDI mode (`1935:000d`). USB CONFIG set to OVERBRIDGE on the panel leaves the bus and comes back as `1935:0b2c`.

**More I/O** (window options): `--audio-in audio|ext|both` feeds the Mac's default
input to AUDIO IN (sampling) or EXT IN (the main mix); virtual ports `rytm-emu usb
in/out` are the USB MIDI port; `--midi-from NAME` connects an existing MIDI source
(for example a real Rytm, as a controller) to the DIN input; `--usb-wav FILE`
records the 12-channel USB audio stream (MAIN, the eight voices, AUDIO IN). Pads
strike harder the higher you click them, and send pressure (aftertouch) while held
as you move the pointer.

**Samples**: with the emulator closed, `dtcard rytm.img add sound.wav` puts a WAV in
the +Drive's `incoming` folder. In the emulator, GLOBAL > SAMPLES, open `incoming`,
YES to tick the file, RIGHT, LOAD TO PROJ, YES. The sample is then in the project's
slot list (FUNC+SMPL on a track) and plays from that track.

## Build

```sh
cargo build --release
```

Upstream's window app `digitakt` is now `rytm`; the other tools (`dtboot`, `fwinfo`,
`dtpeek`, ...) keep their names. See the upstream README for their options.
