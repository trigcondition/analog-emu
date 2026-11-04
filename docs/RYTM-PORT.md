# Porting to the Analog Rytm MKII

Measured 2026-10-06 against Analog Rytm MKII OS 1.74's MAIN OS (the `rytm-mods`
port target; 1.73 shares the platform). Upstream addresses are Digitakt OS 1.53.

## Why this is the right base

The Rytm's MAIN OS already touches the peripherals this emulator models, at the
same addresses **[measured]**:

| upstream model | address | in the Rytm |
|---|---|---|
| DDR, SRAM, code base | `0x40000000`, `0x80000000`, `0x40000400` | the same (rytm-mods `re/symbols.toml`) |
| eDMA + TCDs | `0xFC044000`, `0xFC045000` | 243 refs; the voice-control frame starts a TCD at `0xfc0451e0` |
| INTC0..2 | `0xFC048000`, `0xFC04C000`, `0xFC050000` | 88 refs |
| DMA timers | `0xFC070000`..`0xFC07C000` | 20 refs; mod 0013 times itself with DTIM0 |
| PIT | `0xFC080000`..`0xFC08C000` | 28 refs |
| eSDHC (+Drive) | `0xFC0CC000` | 124 refs |
| SSI1 (codec) | `0xFC0C8000` | 27 refs |
| DSPI0 (status forced) | `0xFC05C000` | 312 refs |
| UART8/9 (panel MCU) | `0xEC070000`, `0xEC074000` | 20 + 20 refs |
| GPIO | `0xEC094000` | 235 refs |

The last two correct `rytm-mods`'s `audio-engine.md`, which read the `0xec07xxxx` and
`0xec094xxx` blocks as an FPGA register window: they are on-chip UARTs and GPIO.

Not modelled upstream, by reference count: `0xFC0B0000` (147, probably USB, which
upstream does not model either), `0xFC03C000` (61), `0xEC090000` (14), `0xFC040000`
(17), `0xFC090000`/`0xFC094000` (26).

## The analog boundary

The voice hardware is fed once per 32-sample audio block: machine routines write a
984 B control frame at `0x800063c0`, and `0x4007a5a4` sends it as 492 SPI command
words (`0x8001wwww`, DSPI PUSHR format) through an eDMA TCD **[measured,
rytm-mods voice-controls.md]**. A value comes back by draining the receive FIFO at
`0xFC03C038` (`0x4007a684`), so `0xFC03C000` is the SPI port to the voice/FPGA side
**[inferred: a second DSPI]**.

That port is the stub: accept the frames (and keep the latest for inspection),
report transfers complete, and answer reads with plausible values. Boot-time FPGA
configuration (section 1) and any status pins on GPIO must read as "configured".
The voices come back to the CPU through the receive buffer (`0x80007440`, filled
from the codec); in the emulator they will read as silence, so the digital side -
samples, noise, delay, reverb - is what will be heard.

## Status, 2026-10-06

Analog Rytm MKII OS 1.73 (stock) boots headless in `dtboot` **[measured]**: 14 RTOS
tasks, the +Drive mounted and checked ("FILE SYSTEM OK", the 1.73 check), the
audio interrupt (v191, `0x4011fafa`) running with SSI1 moving frames, the panel
UART carrying the screen and LEDs, and the main screen drawn. A key press closes
the file-system dialog; the next one reads "MISSING CAL. SYNTH" / "NO FACTORY
SAMPLES" - no analog calibration and an empty card, as expected. About 6x real
time. Digitakt OS 1.53 runs exactly as before (identical counts to the unmodified
build).

What it took:
- **Profile** (`src/symbols.rs`, `rytm_173`): `tools/xmatch.py` matched 12 of 16 code
  addresses uniquely (its self-test returns all 16 unchanged) and 19 data
  addresses by unanimous reference votes; the SD driver's, display and progress
  variables keep their relative offsets, and the OS `entry` equals the image's
  own first word. Not found: the Digitakt main loop and intro hooks (the Rytm's
  splash differs), frame_sem, transport_state, first_boot_error. Both OSes read
  their container header at flash `0x80000`, and both entry routines set their
  own stack (`0x48000000`) at once.
- **DTIM2 from DTIN.** The bootstrap (`0x070207c2`) sets DTIM2's DTMR to 7, clocked
  from the DTIN pin; MAIN OS times a delay on it (`0x400055b2`) before setting up
  SSI1 and hangs if it does not count. The emulator enters MAIN OS directly, so
  `Machine::rytm_bootstrap_state` recreates that, with DTIN modelled at 12.288 MHz
  **[inferred]**; only that wait's length depends on it.

Findings on the way:
- **Section 8 is the panel MCU's firmware, not the Outbox 8's**: byte-identical in
  Digitakt OS 1.53, Rytm OS 1.73 and A4 OS 1.55D (sha256 `6943b2f2...`).
- The panel's key codes are not the Digitakt's. Of raw codes 1..90 pressed at the
  file-system dialog, 4 and 46 close it. The Rytm's button-name table, indexed by
  the OS's own button number, is at `0x40235610` (69 entries: YES is 29, the pads
  57..68); the raw-code -> button mapping is not traced yet.

## Analog calibration (2026-10-06)

The OS keeps its analog calibration in the SPI flash, at `0x340000` with a copy at
`0x300000`: 110706 bytes, read through `0x40136faa` (a `bra` to `flash_read`, so
the emulator already serves it, from a blank flash). Validation (`0x400fade6`)
**[measured]**: magic `RBOW` at +0, block 1's size 12490 at +4, a format word at +8
(5 is current; 4 and below are converted), at +12 the checksum of block 1's 12474
data bytes from +16 (sum of byte_i XOR i, i from 1; `0x400fabfc`), and from format 2
a second block at +12492 with an end offset and checksum. A record that fails
becomes the OS's default: the header and all zeros.

The first data word, +16, decides how the machine engines run: 0 uses built-in
defaults (`0x4011b08c`), anything else the calibration values (`tstw +16` at
`0x4011b07a`); the startup checks also read it (1 = an old calibration, which
brings up "THIS OS SUPPORTS IMPROVED TUNING"). A synthetic record that passes
every check with zeros elsewhere (`DTEMU_CAL=1`, then off by default; now the default, see Toms) makes the first
engine divide by zero (`remul` of 49152000 by +3126, `0x4011b112`), and the OS stops
in its fatal handler. So booting as calibrated needs plausible values for every
field the engines read; until then the uncalibrated boot is the working one, and
its three notices can be dismissed once and kept out of later starts with a
snapshot.

## Audio and the analog voices (2026-10-06)

**The codec link** [measured]: the Rytm's SSI1 frame is 16 words of 24 bits (STCCR
`0x56f00`: DC 15, WL 24 bits). Transmit DMA moves 8 words per request and receive
DMA 6, so each frame takes two requests each way; the emulator now issues as many
requests per frame as STCCR and the channel's minor loop imply (`Io::ssi_requests`).
Before this the Rytm's whole audio engine ran at half speed (audio interrupt about
700/s instead of 1500/s). The transmit frame feeds the analog mixer: channels
0..7 are the voices' digital layers, 8..10 the tom exciters, 12/13 the PRE and
14/15 the POST FX returns (negated) [rytm-mods, inferred; word 8 measured otherwise
below: BD's sample plays there]. MAIN is analog and only comes back through the
codec input. The raw receive frame (DMA ring at `0x80003000`) is the eight
physical voices in order, AUDIO IN L/R, MAIN L/R; the audio interrupt's copy
(`0x4010e944`) negates every word and reorders them into the buffer rytm-mods
documents at `0x80007440` (MAIN, voices by slot `0x401ef1d0`: 0 3 1 4 6 5 7 2,
AUDIO IN) [measured]. Until 2026-10-06 the model fed the documented order as if it
were raw; the pads' activity glow (`0x401214c4`, raw word v read as voice v) then
lit BD, BT and CH on an RS hit (MAIN L/R and RS in words 0, 1 and 4), which a real
Rytm does not do. Fixed: each strike now lights only its own pad.

**The model** (`src/analog.rs`): once per receive frame it reads the control frame
at `0x800063c0` from SRAM and renders the voices into the receive frame and a
speaker mix; the window adds the transmit frame's digital channels and returns.
`DTEMU_NO_ANALOG=1` turns it off (and silences the window, which now plays the model's mix),
`DTEMU_ANALOG_TRACE=1` prints its state.

**BD** (voice 0), first version: TUNE1/TUNE2 (+90/+92) as linear frequency (678 =
55 Hz [inferred]), LEVEL1/LEVEL2 (+94/+96), FM, ring mod and osc 1's wave switch,
and records 0 (pitch) and 2 (amplitude) at +360 played at the OS's own rate
decoding, restarted on the trig switch. With BD PLASTIC on steps 1/5/9/13 at 127 BPM
it gives a hit every 472 ms [measured on the emulator's output].

Scaling, from turning BD PLASTIC's knobs and reading the frame (`tools/bdprobe.sh`)
[measured]: SWD sets the pitch record's start level from 2 to 15, so 15 is a
full-depth sweep; SWT sets its rate from exponent 6 to 10, which by the OS's own
segment timing is a sweep of about 0.2 to 4.4 ms. The model takes the full depth
as 3 octaves and the amplitude level as logarithmic (6 dB per 16 levels, 127 full
scale) [both inferred]. The default BD PLASTIC hit then starts near 330 Hz, falls
to its 55 Hz fundamental within a few ms, and decays about 23 dB by 60 ms and 46 dB
by 100 ms. Waveforms are sines, the FM index and the
level scales are guesses, and the receive words' justification against what the
OS expects is not checked.

**SD and RS/CP** (2026-10-06): one dual-oscillator model at the three places the
frame holds them (BD +90/+360, SD +98/+392, RS/CP +106/+424, each with its switch
bits). Sampling the frame around a pad hit showed how the hardware's envelopes are
commanded [measured]: a record's first word is a command, 0x4000 to start the
segment at its start level and 0 to continue toward the target, written once when
a hit begins; and the records are filled a block or two after the trig bit, so
the model starts a segment when a record changes with the start command, not on
the trig. SD HARD's pitch record starts at 100 (BD PLASTIC at most 15), so the
sweep is taken as adding to the tune CV, linear in frequency, scaled so BD
PLASTIC's full sweep stays 3 octaves [inferred]. SD now starts near 600 Hz and
dies within about 20 ms, RS sounds about 200 ms; CP CLASSIC leaves the oscillators
silent (a clap is noise: its digital layer, which needs the analog filter that is
not modelled yet).

**Pads** [measured]: a pad is a pressure sensor, and the panel MCU reports it with
its own messages (parser `0x40082084`): `0xAp force` when pad p is struck (a
velocity curve above the threshold at `0x40285624`, 10, then a pad-hit event, type
3), `0xBp` on release (type 4), `0x9p` pressure (type 2), and `0xCp` the pad as a
button in groups 9..10. Pad p maps to the OS's pad index through `*0x40285608`
(identity). A press now sends the button bit plus `0xAp`/`0xBp` (force
`Machine::pad_force`, 100); BD, SD and CH each trigger, in dtboot and the window.

**Frame alignment (fixed 2026-10-06).** The transmit DMA reads 8 words per
request from an 8 KB ring at `0x80000000` (TCD54, SMOD 13); frame word 0 is at each
64-byte boundary. The emulator used to group words into frames from wherever capture
began, and the stream sat 8 words off: BD's sample showed up as "word 8" (the BT
exciter in the map above), and the window's mix subtracted the CH/OH..RS/CP digital
layers as if they were the FX returns. Now the eDMA engine marks a channel-54 read
that starts on a frame boundary and the STX capture drops any partial frame before
it; `dtboot` and the window take whole frames only. Checked: a BD strike with a
sample loaded plays on word 0 of the ring and of the capture [measured].

**Toms: blocked on calibration.** Without a calibration record the tom CVs stay
near 0 (BT +118/+120 at 0 after a hit, MT/HT +128 counting up by 1 per block), so
the tom circuits would not sound on a real unit with MISSING CAL either [measured
values; inference]. The synthetic record (`DTEMU_CAL=1`) still stops boot in the
fatal handler. Modelling the toms needs plausible calibration data first.

**Tom calibration, in progress (2026-10-06).** `0x4011b000` sets the toms up from
the record [measured]: uncalibrated (version 0) it writes fixed constants to
`0x42fb6b50..6f` (pairs 36688/4964404, 75759/4279691, 75620/555, 75620/555) and
leaves `0x42fb6b70..8f` zero. Calibrated, it reads nine period counts per tom at
+3126 + 36g + 4k (12.288 MHz ticks), fits 49152000/count against the x table
`0x401f7ecc` (362, 512, ... 4096; `0x401048d6`, n = 8) into `0x42fb6b50+8g`, and
fits log2(12288000/count) against block-2 values (+12500 + 36g + 4k) into
`0x42fb6b70+8g`. BT CLASSIC (`0x4011b278`) computes its decay CV (+120) from the
`6b70` pair, hence near-zero decay CVs uncalibrated, and its tune CV (+118) as
`(pitch << 8 - 6b54) * 6b50` clamped, pitch coming from `0x4011e814` /
`0x4011e86e` [measured]. With synthetic counts (pitch linear in x) `DTEMU_CAL=1`
now boots past the old divide-by-zero, offers "improved tuning" recalibration
(the record's version 2 is older than this OS prefers; NO skips it), and the decay
CVs take real values (BT 0x33ac). The linear fit comes out wrong (slopes of 1e9,
offsets about 163202 for every tom), so the tune CVs stay parked at -1: the
count format the fit expects is not yet understood.

**Toms working (2026-10-06).** The fit is ordinary least squares in 16.16 fixed
point (`0x401048d6`); the setup stores `2^43 / (slope >> 6)` and `intercept * 1024`.
The tom routines (BT: `0x4011b534`; XT CLASSIC for LT and MT/HT: `0x4011b7e4`) write
the tune CV as `(((pitch << 8) - offset) * inv_slope)^2 / 2` with the MAC in
fractional mode, clamped 0..65535 [measured]. Uncalibrated, the built-in constants
make that round to 0; with my first counts it clamped at 65535 (so `0xffff` there
was the clamp, not a park value). The record now describes one consistent made-up
tom: fit value y = 1142.8 f and f = 0.1 x, so f = 4.53 sqrt(CV) Hz [synthetic], and
the model (`Tom`) uses the same law, so BT's default TUN comes out at about 100 Hz
and MT/HT's tune falls over a hit (the firmware's pitch sweep). The exciter is DAC
word 8/9/10 (the last complete frame, `Ssi::tx_last`), ringing a 2-pole resonator
whose time constant follows the decay CV (30 ms .. 1.5 s) [inferred]. XT CLASSIC
keeps a tom with machine LEV 0 silent: in the test kit LT's first parameter word is
0, so LT stays silent there, as it would on a unit [measured]. The calibration
record is now on by default (`DTEMU_NO_CAL=1` for none); BD and SD are unchanged
by it. Its version word (2) makes the OS offer "improved tuning" recalibration at
boot; answer NO. `dtboot` now also stops at a breakpoint hit before a scheduled
event (it used to run past it).

**Voice chains, mix and effects (2026-10-06).** Each voice now runs its own chain
in the model (`Chain`): source plus its sample layer (DAC word v) -> overdrive
(+42+2v) -> multimode filter (type field, +58 frequency, +74 resonance, envelope
record +296 times its depth word) -> amp envelope (+168+8v) -> the receive-frame
word v -> level/pan (VCA words). Measured: the VCA words stay constant over a hit
(level x pan only; the amp envelope is in the records); filter type is a 3-bit code
per voice found by turning TYP on every track: LP2 0, LP1 6, BP 1, HP1 2, HP2 4,
BS 5, PK 3, fields at bytes 161/1, 161/5, 164/5, 160/1, 167/1, 163/5, 167/5, 163/1
(byte/lowest bit) for voices 0..7. The model mixes the voices with the FX returns
(DAC 12/13 before, 14/15 after the master distortion, +0) and the window plays
that mix instead of summing DAC words. Envelopes start a segment when their
record's start, target or rate changes. Curves are inferred: cutoff 20 Hz to
20 kHz over the word, a TPT state-variable filter, tanh overdrive and distortion.
Checked headless: every voice sounds through its chain (CP's noise now passes its
filter); BD with DEL at 127 repeats at about 0.7 s intervals, halving each time
(the firmware's delay, fed from the receive frame); CH with REV at 127 has a tail
past 2.5 s; SD with FRQ turned down falls from about 9000 to 1000 sign changes a
second.

**Compressor and routing (2026-10-06).** Turning each compressor knob and diffing
the control frame [measured]: THR -> word +2, RAT -> word +4 and byte 165,
MUP/MIX/VOL -> words +6/+8 (wet and dry gains), ATK/REL/SEQ -> byte 162 (bits 5-7,
0-2, 3-4; codes not in knob order, each value spans three detents). Rather than
decode those, the model (`Comp`) reads the FX track's live parameter block
(`0x8000fbce`: DIST, then THR ATK REL RAT SEQ MUP MIX VOL as 8.8) and follows
rytm-mods' hardware measurements of the real compressor: 0.9 dB per THR step and
none from THR 105, ratios 10/6/3.5/2.5:1, peak detector on max(|L|,|R|) through
the SEQ sidechain filter, one-pole attack at the label, release 0.45 x the label,
MUP/MIX/VOL tables; the absolute make-up is anchored so default MUP 47 is unity
wet [inferred]. Checked: default settings leave a BD hit at 3475 (4696 dry, VOL 96
about -2.6 dB); THR 0 crushes it to 986. ROUTE TO MAIN: byte +166 (bits MSB first
for voices 7,6,5,4,0,1,2,3) drops a voice from the main mix; toggling BD off in
GLOBAL > AUDIO ROUTING > ROUTE TO MAIN sets 0x08 and BD's hit leaves MAIN silent
while SD plays [measured]. A voice off main still feeds its receive word, so its
sends still work.

**rytm-mods builds (2026-10-06).** An image whose MAIN OS hash is unknown but
whose first 16 bytes at each of the 1.73 profile's code addresses (`AR_173_CODE`,
14 places: entry, flash read, RTOS calls, panel diff, abort loop, SysEx sender)
are stock runs on the 1.73 profile as "OS 1.73 (modified)". rytm-mods patches
caves and detours elsewhere, so its builds pass; a different OS does not.
Checked: rytm-mods `main` built with mods 0000-0013 (its `make verify` passing)
cold-boots to the main screen, and 0008-filter-hz's popup reads "7.46kHz" on the
FILTER page. Snapshots are per OS image, so a mods build needs its own. The
filter's cutoff law now follows 0008's hardware measurement too: one semitone a
FRQ step, 440 Hz at 60.

**OS 1.74 (2026-10-06).** `rytm_174` was ported from the 1.73 profile with
tools/xmatch.py: all 17 code addresses unique masked matches (the RTOS core
unchanged; panel, flash, display and MIDI code after 0x40080000 moved +0x6c) and
all 22 data addresses unanimous (only the framebuffer pointers moved, +0x360).
SRAM (control frame, FX parameters) did not move. Modified 1.74 images are
accepted by the same fingerprint check (`AR_174_CODE`). Checked: stock 1.74 and the
rytm-mods `port-1.74` build (mods 0000-0013) cold-boot to the main screen, the
build's 0008 popup reads "7.46kHz", and BD, CH and BT sound as on 1.73 (the
synthetic calibration works on 1.74 too).

**Inputs and pad pressure (2026-10-06).** AUDIO IN L/R (sampling, USB) goes in
the raw receive frame's words 8/9 only; EXT IN L/R joins the analog mix after the
distortion and before the compressor (rytm-mods measured EXT IN that way). Fed by
`dtboot --audio-in/--ext-in FILE.wav` or the window's `--audio-in audio|ext|both`
(the Mac's default input, resampled to 48 kHz; off unless asked, since it opens
the microphone). Checked headless with a 1 kHz tone: EXT IN reaches the mix and
not the firmware's AUDIO IN words; AUDIO IN reaches the firmware's processed buffer
(`0x80007440`, words 10/11) and not the mix [measured]. The live input path is not
yet run. Pads: the panel parser's type 9 (`0x9p value`, `0x40082276`) posts a
pad-pressure event with the raw value; the window strikes with a force from the
click height and streams pressure as the pointer moves. Checked: pressure comes
out as polyphonic aftertouch on MIDI out (`AD 00 20..7F`).

**USB MIDI (2026-10-06).** The general MIDI sender `0x4008575e(len, ptr,
interface)` sends interface 3 to DIN (`0x40001dea`), 2 to USB and 4 to both; the
USB device side is `0x400029e0(len, ptr)` (and `0x40002934` for up to 2 bytes),
which return at once with no host attached [measured]. Both are hooked: their
bytes go to `Io::usb_midi_out`. USB MIDI input: short messages are 4-byte slots
`[status, d1, d2, len]` in a 256-entry ring (`0x42f67c18`, index `0x41847e80`,
allocator `0x400812de`), SysEx the block chains as before; either goes into the
USB MIDI queue from the idle task. The window adds virtual ports "rytm-emu usb
in/out" (the Elektron protocol's replies go to both port pairs), and
`--midi-from NAME` connects an existing MIDI source (a real Rytm, a keyboard) to
the DIN input. Checked through CoreMIDI with mido: a note into "rytm-emu usb in"
plays SD; a BD press comes out of "rytm-emu usb out" and "rytm-emu out" (the
default output is MIDI+USB); a note and a CC from a stand-in source reach the
emulator through `--midi-from`. 1.74 has all four addresses unchanged.

**USB audio (2026-10-06).** The class-compliant stream is the processed receive
buffer verbatim (rytm-mods); the emulator hooks its copy routine (`0x4010e944`,
1.74 `0x4010e9b0`) and, with `--usb-wav FILE` (window or dtboot), collects each
block's 32 frames x 12 words: MAIN L/R, the voices by slot (BD SD RS BT LT MT CH
CY), AUDIO IN L/R. Checked: BD, SD and CH hits land on channels 2, 3 and 8 and on
MAIN. A live virtual audio device would need a macOS audio driver and is not
attempted; USB audio into the Rytm (USB IN routing) is not modelled.

**Digital engines, feasibility (2026-10-06).** See tools/dspbench/README.md:
Braids (MIT, fixed point) compiled for the MCF54418 with m68k-elf-gcc runs at 58
(FM) to 217 (HARMONICS) instructions per sample on the emulator's core; the
firmware leaves about 44% of the budget spare under heavy playing. `cfbench` runs
compiled code on the CPU core alone and counts instructions.

**Analog imperfections (2026-10-07).** After kitbasher's analog recreations for
the Machinedrum (github.com/etcetc000/kitbasher, GPL-3; ideas only, no code): each
hit draws a detune of up to +-3 cents per oscillator (dual oscillators, the hat
and CY/CB banks, the toms); the dual oscillators also drift (about 1.5 cents RMS,
0.5 s, independently); the hat and CY/CB banks start a hit after silence at random
phases. Depths are [inferred], not measured on a Rytm. `DTEMU_ANALOG_IDEAL=1`
turns it off, `DTEMU_ANALOG_SEED=N` changes the sequence. Checked headless (pad
strikes, rytm.snap): six CH hits differ by 0.19 relative RMS when ideal and 1.4
with random phases; BD and HT hits differ slightly (0.02..0.04); a tom's hit is
its exciter word going nonzero after 64 zero samples (HT shows it; this kit's BT
never drives DAC word 8). Kitbasher's own analog models are documented as
approximations from the manuals, not measurements, so they say nothing new about
the Rytm's circuits.

**Cymbal/cowbell** (`analog.rs`, `Cym`), traced [measured]:
- +640..+663: CY CLASSIC loads the hat's six 808 frequencies; CB CLASSIC only the
  first two, 23531 and 34751 units = 539.5 and 796.7 Hz at the hat's 43.62
  units/Hz, the TR-808 cowbell's 540 and 800 Hz. That confirms the scale. The
  fields' top bits go 0xc040 -> 0x4040 just after a CY hit (an oscillator reset?).
- +158 VCA: 0x14a4 CY, 0x09eb CB at the test kit's settings.
- Records +496..+559: a CY hit starts records 2, 3, 4 (mode 0xF; start levels 66,
  -93, 115; rates e9/m1fe8, ee/m8e8e, ef/m0e8e) and record 7 (start 127, held,
  then ef/m0e8e); a CB hit starts record 6 (20 -> -120 in about 6 ms) and record 7
  (127, then eb/m542a). CB TEST calls its envelopes 5ENV and 8ENV.
The model [inferred]: the running oscillators (squares) through one path per
record: 2 band-pass 3.4 kHz, 3 band-pass 7 kHz, 4 high-pass 9 kHz (all shifted by
the colour CV +142), 6 band-pass 1.2 kHz, 7 the body (cowbell: band-pass 1.2 kHz;
cymbal: high-pass 6 kHz); path gains are linear in the record level (under the
logarithmic mapping used elsewhere, CB's strike at 20 and a CY band at -93 would
be inaudible); summed, times the VCA. Headless: CY peaks 5949 and falls 20 dB in
0.54 s, CB peaks 963 and falls 20 dB in 0.06 s.

**Hihat** (`analog.rs`, `Hat`): traced with `DTEMU_FRAME_TRACE=OFF,...` [measured]:
- +616..+639: six 32-bit fields, low 22 bits 8956, 13281, 16165, 22806, 23592,
  34952 with CH CLASSIC: the TR-808's metal oscillator ratios to 0.3%, so taken as
  frequencies at 43.62 units/Hz (205.3..800 Hz) [inferred scale];
- +130 colour: 0 for CH, 0x8400 for OH at defaults; +140 VCA: 0x6333 CH, 0x1f6e OH;
- the path level words +132..+138 stay 0;
- records +456..+495: a CH hit puts mode 0xF in records 2 and 4, then decays at
  e=0xb m=0x21fd (about 90 ms); an OH hit uses records 3 and 4 at e=0xd m=0xb46d
  (about 0.5 s). After the 0xF the firmware rewrites the record every block with
  mode 9 then 0 at the same rate.
The model: six squares, a 2-pole high-pass set by the colour CV (6 kHz at 0, 1.5
octaves per full scale), record 4 as the envelope (starting on the 0xF edge,
silent when its gain word is 0), times the VCA word, then pan/level like the other
voices [all inferred]. Headless: CH sounds about 40 ms above -44 dB, OH quieter.

## The panel's identity and its LEDs (2026-10-06)

The OS asks the panel MCU what it is (`70 00`) and checks the answer twice: type 7,
or its self-test reports "WRONG UI CARD" (`0x40139d5a`, `0x4013bbde`) **[measured]**.
Answered as the Digitakt's card (type 4), the Rytm OS boots but sends no LED
updates after boot. With type 7 it also compares two UI firmware versions with the
ones it carries (5 and 5, `0x402c8758` / `0x402cb480`) and, if they differ,
re-flashes the panel MCU over a bootloader protocol (`0x4013fe9c`) the model does
not speak, stalling the boot about 30 s on timeouts. The Rytm card is therefore
`[7, 5, 5, 1]` (`panel::CARD_RYTM`), and the LEDs follow the OS.

The Rytm addresses up to 84 LEDs: selectors `0x2g` (groups 0..15) and `0x3g`
(16..31, a second bank the Digitakt never sends), and a 128-entry palette (it
animates colours by rewriting palette entries 0x74..0x78). Key -> LED
(`panel::RYTM_LEDS`), measured by pressing keys: trig k = LED k-1, BANK E..H 17..20,
RECORD 21, PLAY 22, STOP 23 [inferred], FILL 24, BANK A..D 25..28, CHAIN 29, SONG
30, FX 31, TRIG/SRC/SMPL/FLTR/AMP/LFO 38..43, FIX 45, PERF 46, SCNE 47, CHRO 48,
MUTE 49, PLAY MODE 50 [inferred], SAMPLING 52, GLOBAL 53, TRK 55 and RTRG 56
[inferred], pads 72..83 (CH OH CY CB BT LT MT HT BD SD RS CP).

**LED strips (2026-10-12) [measured on OS 1.73, card copy, `dtpeek leds`].** Two
strips of four red LEDs, which the window draws (`panel::RYTM_PAGE_LEDS` above PAGE,
`panel::RYTM_MODE_LEDS` beside it, labelled SEQ D.ST D.JMP T.JMP):
- PATTERN PAGE 64..67, pages 1..4: dim (131,0,0) for each page in the pattern
  (one at boot; all four after FUNC+PAGE, the LEVEL knob up to 64/64 and NO).
  In GRID RECORD the shown page is bright (255,0,0) and the others dimmer
  (65,0,0); each PAGE press steps it 64 -> 65 -> 66 -> 67 -> 64. While playing the
  played page lights bright (67 seen bright mid-pattern). Inside the scale menu
  they show the page count: PAGE there lights 64, 64..65, ... (all bright).
- PATTERN MODE 68..71 = SEQUENTIAL, DIRECT START, DIRECT JUMP, TEMP JUMP: FUNC +
  BANK A..D lights 68, 69, 70, 71; 68 lit at boot. With TEMP JUMP the previous
  mode's LED stays dim (68 at (131,0,0)), as on the A4.
No key or mode found that lights LEDs 16, 32..37, 44, 54 or 63 (57..62 are never
addressed); LED 51 is green from boot for a reason not found and is not drawn.

## DIN MIDI (2026-10-06)

UART9 is the Rytm's DIN MIDI port **[measured]**: the startup code sets it to 31250
baud (`0x4000201c` with `0x7a12`) and installs the MIDI byte handler `0x400813c0`.
Both directions are DMA-driven: receive on channel 36 (into a ring at
`0x4b800000`), transmit on channel 37. The emulator feeds MIDI in at the line's
rate (`Io::midi_send`, 3125 bytes/s), raises those channels' requests, and
collects MIDI out (`Io::midi_out`). Checked: note 0 on the auto channel (14)
triggers BD, as on the device; a BD pad press sends `9D 00 7F` / `8D 00 00`. The
window bridges the port to virtual CoreMIDI ports (`midir`), checked both ways
with mido. USB MIDI is not modelled.

**Transfer does not connect over it** [measured]. With `DTEMU_MIDI_LOG=FILE` the
window logs both directions; Transfer sends `F0 00 20 3C 10 00 00 00 03 00 00 01 F7`
(Elektron header, type 0x10, sequence 3, command 1: a ping in elektroid's
`src/connectors/elektron.c`) every 6 s and gets no answer. The OS's SysEx router
(`0x40083d6c`) does take it in from DIN and looks type 0x10 up in its table
(`0x4028585c`: types 0x00, 0x04, 0x07, 0x0c, 0x10, 0x17), but the type 0x10
handler (`0x400830f6`) only passes a message on to the RPC handler
(`Midi::handleSysexRpc`, `0x40085dd8`) when its input interface is 2 or 4, and
`handleSysexRpc` never runs for DIN input. So the file-transfer protocol is only
served on other interfaces, presumably USB [inferred]. A standard identity request
over DIN gets `F0 7E 00 7C 00 F7` (a "wait" handshake), not an identity.

**The emulator now feeds it in as USB.** The MIDI task (`0x40082cac`) reads three
queues: `0x418fd8bc` DIN (interface 8, or 0x10 on the auto channel), `0x418fd89c`
USB (2, or 4 on the auto channel) and `0x418fd87c`, all signalling one event,
`0x418fd874` [measured]. A USB item is a chain of blocks from the MIDI pool
(1024 x 160 bytes at `0x42f6a024`, `{len, next, data[152]}`; bitmap at
`0x42f67b98`, allocator `0x4008130a`, free `0x400811cc`); the task decodes it
(`0x40081248`, up to 64 KB) and hands it to the SysEx router with interface 2.

A first version relabelled the protocol's SysEx from DIN as interface 2 at the
router. Browsing worked, uploads did not: **DIN drops SysEx longer than 32767
bytes** (`0x40080eba`), and Transfer's upload block is 37481. Now the window
routes Elektron-protocol SysEx (`F0 00 20 3C 10`) past DIN entirely
(`Machine::midi_send`, `Io::usb_in`): in the idle loop the emulator builds the
block chain in the pool (setting the bitmap bits as the allocator would; no task
can be inside the allocator while the idle task runs) and has the idle task call
the firmware's own `queue_put` (`0x400013b2`) and then `sem_post(0x418fd874)`
(`0x40001078`), as the DIN receiver does (`0x40080f70`). The firmware is unchanged.

The reply goes through an output stream (`0x4190891c`, vtable `0x401d7cec`, built by
`0x40084fea(obj, 0, 2)`) whose flush calls `0x400859ac(buf, len, abort_flag,
interface)`; for interfaces 2 and 4 the emulator takes the bytes there into MIDI
out and returns success, skipping the per-interface writer (`0x4008575e`, 144-byte
chunks) and the USB controller behind it. Checked with `tools/ekproto.py`, which
frames messages as elektroid does [measured]:

- ping (1) -> 0x81: device id 0x10 (elektroid's "Analog Rytm MKII"), the list of
  supported commands, name "Analog Rytm";
- software version (2) -> 0x82: "0173", "1.73";
- device UID (3) -> 0x83: zero; storage info (5) -> 0x85: all zero headless,
  957.9 MB free in Transfer on the window's card;
- the injection also takes the USB producer's message credit (`0x42f67b90`, a
  counting semaphore of 128 that `0x400813d4` pends on and the free posts);
- a window session replayed headless from a clean boot (`dtboot --midi-file` per
  logged message, clock = start + 200M per second): three samples upload, every
  block acknowledged, credit back at 128 after. The same session replayed from a
  snapshot saved after the earlier DIN attempt fails with "Data checksum is not
  correct" and then a heap assertion (`0x40082984`): that snapshot's MIDI heap was
  already damaged, so such snapshots should be discarded;
- Transfer's own upload, replayed from the log: open sample writer (0x40) -> 0xc0
  id 4; the first 37481-byte block (0x46) -> 0xc6 status 1, 32768 bytes, within
  2 s of wall time headless.

## Samples (2026-10-06)

**Loading and playing work** [measured]. The route is the manual's: GLOBAL >
SAMPLES opens the +Drive browser; YES ticks a file, RIGHT opens the side menu,
LOAD TO PROJ, YES at "LOAD 1 SAMPLE?" gives "1 SAMPLE LOADED". The project's slot
list (FUNC+SMPL on a track) then shows `1:TESTSINE`, and choosing it sets SMP to 1.
The slot list itself only picks samples already in the project, which is why the
first attempt (below) stayed empty. `dtcard`'s sample format is accepted as is.

The loaded sample is in DDR at `0x43937290` (64-byte header, PCM from `0x439372d0`;
a copy is also at `0x402ffbe0`). On a BD strike the voice code (`0x4011c540`,
`0x4011c6b0`) reads it through the `0x4b......` alias of the same memory, and the
sample comes out on DAC frame word 8, once per strike, its length unchanged, at
about half its level. Open: the pitch measured about 242 Hz against the source's
218 Hz with a rough zero-crossing count; not checked more carefully. Tools added for
this: `dtboot --wav` writes one channel per SSI frame word (16 on the Rytm),
`dtboot --watch-read ADDR LEN` logs CPU reads as well as writes (interpreter only),
and `dtpeek SNAPSHOT find HEX` searches DDR and SRAM.

First attempt, kept for the record:

The +Drive written by upstream's `dtcard` (ekFS, the Digitakt's) works on the Rytm
as far as browsing [measured]: `dtcard IMAGE add X.wav` puts the file in
`incoming`, the OS creates its own `Factory` tree (CYMBALS, EFFECTS, EXTRA, HIHATS,
KICKS, PERCUSSION, SNARES, ...; empty, hence NO FACTORY SAMPLES), and FUNC+SMPL,
an empty slot, YES opens a browser that lists the file with its size. Loading it
into a slot does not complete yet: the slot stays empty after YES on the file (the
SMP parameter does point at the slot), and RIGHT there opens no LOAD TO PROJ menu.
Next: trace the load (the +Drive reads it issues, the sample header it expects),
and try the manual's route, GLOBAL SETTINGS > SAMPLES > LOAD TO PROJ.

## Work, in order

1. ~~**Rytm profile**~~ done for 1.73 (`src/symbols.rs`): the ~40 addresses upstream uses (RTOS task
   switch, semaphores and queues, main loop, panel diff, framebuffers, SD driver
   state, boot milestones). Plan: fetch Digitakt OS 1.53 from Elektron, check its
   hash, fingerprint each function there with absolute operands masked, and find the
   match in the Rytm image; verify every hit by its callers. Make the profile keyed
   by device and MAIN OS hash, and accept a patched image by checking the stock
   bytes at the profile's addresses rather than the whole-image hash, so rytm-mods
   builds can run.
2. ~~**First boot**~~ reaches the main screen. Next: the raw key map (trace or
   scan), the calibration data the OS looks for, and a 1.74 / patched-image profile.
   Was: **First boot**: run `dtboot` headless on the Rytm image, log every access to an
   unmodelled page, and stub until it reaches the main loop.
3. **Voice SPI and FPGA stubs** as above.
4. **Panel**: the Rytm's front panel (12 pressure pads, more keys, its screen)
   over the same UART protocol, with a Rytm key map and panel window.
5. **+Drive**: check that the Rytm's +Drive filesystem is upstream's `ekfs`.

Not planned: an analog voice model. The control frame is available to inspect, so
a crude one could come later.

**USB device controller and a simulated host (2026-10-08).** `src/usbdev.rs`
models what the MCF54418's USB OTG controller (0xFC0B0000, ChipIdea-style) does by
itself: USBCMD.RST, ENDPTPRIME and ENDPTFLUSH clear themselves, USBSTS / OTGSC /
ENDPTSETUPSTAT / ENDPTCOMPLETE bits clear on writing 1, priming sets ENDPTSTAT.
Before this, switching USB CONFIG hung forever in `0x40002640` waiting for a flush
to finish. `src/usbhost.rs` (`--usb-host`, dtboot and rytm; `DTEMU_USB_LOG=1`)
plays the host on a 125 us microframe clock (`Io::usb_uframe`): VBUS (OTGSC
BSV/BSVIS), bus reset, a high-speed port, enumeration as Overwitch does it
(configuration 1, interfaces 1 and 2 at alternate setting 3), then the
Overbridge stream on interrupt endpoint 3 [measured]:
- interrupt: INTC2 source 17 (vector 209, handler 0x400040ae), which defers to a
  forced INTC1 source 6 (0x400050dc);
- the setup buffer (dQH0 +0x28) holds two little-endian words (the firmware
  byte-reverses them, 0x400051ec); dQH/dTD fields are big-endian;
- OVERBRIDGE mode enumerates as 1935:0b48 with six interfaces; endpoint 0x83
  alt 3 has a 368-byte packet, bInterval 1;
- Rytm -> host blocks start 0x0700 + a frame counter (host -> Rytm 0x07ff); 7
  frames x 12 int32 BE after a 32-byte header;
- the firmware locks to the host: SOF timestamps per microframe
  (0x42f67a4c[uframe & 31]) and the 1 ms frame number the controller writes into
  completed dTDs (buffer pointer 1, low 11 bits); its sync needs completions one
  packet per microframe (a burst never syncs), then enables the OUT reader
  (0x402d73c4, reader 0x40005e9c). OUT dTDs have no IOC; the audio code polls them;
- the controller follows the current dTD's next pointer when the overlay's
  terminates, and un-primes on reaching an inactive dTD (the firmware reuses a
  ring of dTDs and re-primes when it sees the endpoint idle).
Checked: 48 kHz both ways (477008 frames each in 10 s); a sine on host channel 0
(MAIN L) reaches DAC word 15 (the post-FX return), on channel 2 (MAIN FX L) word
12 (the pre-FX return). The Rytm's 12 output channels go to the Overbridge link
as `R` messages (oblink.rs). Open: the MAIN return's level in the analog model
(a loop of MAIN came back about 18 dB down); the USB MIDI interface in
Overbridge mode is still the high-level hook.

## Overbridge link: hello, MIDI pair, voice frames (2026-10-14)

The TCP link (`--ob-link [ADDR]`, oblink.rs; the header has the full message list) gets an opt-in hello, for the
rytm-teensy Pi engine as host:

| message | direction | meaning |
|---|---|---|
| `H` + version 1 + flags | host -> emulator | bit 0 the MIDI pair, bit 1 no `R` frames, bit 2 `a` frames |
| `H` + 1 + granted | emulator -> host | the answer (unknown bits are not granted) |
| `M` + u16 LE len + bytes | both, after bit 0 | to the host: the Rytm's USB MIDI output as it comes (the same bytes as the window's "usb out" port); from the host: MIDI into the Rytm's USB MIDI input, a byte stream (a SysEx up to 64 KB, or a running status, may span `M` messages; a longer SysEx is dropped and counted) |
| `a` + u32 LE n + n x 8 x i16 LE | host -> emulator, after bit 2 | the 8 voices in physical order, voice v on channel `VOICE_CH[v]` = {4,7,5,8,10,9,11,6} as v << 16, the rest 0 |

- Old hosts never send `H` and get exactly the earlier stream (first `N` 512, `N`/`T`/`P`/`R`, `A`): obhost.py,
  teensy_bridge.py, the Teensy. An `M` or `a` without its grant, or an `A`/`a` of more than 65536 frames, drops the
  connection. Each connection has its own writer, so a new host gets every message and nothing queued for an
  earlier host (messages while no host is connected are dropped).
- `--ob-link 0.0.0.0:7301` serves a host on the LAN (e.g. the Pi on a direct cable).
- The newest host wins: a new connection closes the one before, so a stalled or dead host (a pulled cable, no FIN)
  never holds the link; the Pi engine reconnects every 2 s. A host that stops reading is dropped after 5 s of a
  blocked write (its writer closes the socket, so its reader ends too). No read timeout: a paused emulator asks for
  nothing, and a live host may then send nothing. The `H` answer is queued before the grant takes effect, so no `M`
  comes ahead of it; an `R` queued before a no-`R` hello may still arrive just after the answer.
- The host's MIDI in flight is bounded by bytes too (256 KB, besides 4096 batches), and machine.rs gives it to the
  Rytm only while the USB MIDI input (`io.usb_in`) holds less than 64 KB; past that it waits in the link, and what
  does not fit there is dropped and counted (`dropped: ... N midi msgs (B B)` in the link report).
- The link report: the window prints it every 10 s on stderr, dtboot at exit:
  `overbridge link: host connected, flags 0x7, frames short N, usb OUT frames short M, midi to host X B, from host Y B`.
- The underrun counters: `frames short` (`ObLink::underruns`) counts the frames a mix block needed from a
  connected host and did not have (the stand-in path, without a streaming USB host); `usb OUT frames short`
  (`UsbStats::out_underruns`, with `--usb-host`) the OUT frames sent as silence because the link had none.
Checked (2026-10-15, the mod build AR173_us_us1 cold-booted headless, rytm-teensy sim/emu_link.py): hello answered
0x7, the mods' 0x19 reports arrive as `M`, 919328 frames served in 20 s, frames short 0 while the host was
connected, `from host 7 B` for one message sent in; obhost.py (no hello) unchanged, flags 0x0.

## Strict memory and the real USB MIDI sender (2026-10-09)

For the rytm-mods pre-hardware review (rytm-mods docs/REVIEW-2026-10-09.md,
HIGH 1 and CRITICAL 1).

**Strict mode** (`DTEMU_STRICT=1`; `=log` lists only): an access to a page no
model claims, or to the SRAM mirror beyond the real 64 KB (0x80010000 ..
0x8BFFFFFF), is logged (`strict:` lines in dtboot, `Bus::strict_log`) and the
CPU takes an access error (vector 2) with the faulting instruction's PC in the
frame. It runs on the interpreter (no JIT) so the PC is exact. Allowed, as stock
1.73 touches them in every run [measured: cold boot, play, every track's machine
list, SAMPLES browsing, project save and load]: the unmodelled peripheral pages
0xEC070000-0xEC074FFF, 0xEC090000, 0xEC094000, 0xFC03C000, 0xFC040000,
0xFC05C000, 0xFC090000, 0xFC094000, 0xFC0B0000-0xFC0B3FFF (USB), 0xFC0C8000; and
page 0 only from stock's own instructions: 0x4012119a (`clr.w (a0)` with a0 = 0,
~500k a run - a NULL store in stock, harmless here, unknown on hardware),
0x4012185c (`move.b 0x0`), 0x400afe1e (`tst.b 0x8`) [measured] and the image's
other absolute references below 0x1000 in code [inferred from objdump]. So
stock reads and writes address 0 legitimately - whatever answers there on
hardware (FlexBus?) is not known. Stock 1.73 and rytm-mods lp3n both cold-boot
and run every scenario clean under `DTEMU_STRICT=1`; under `=log` the two
builds' sets of (pc, page, read/write) are identical (rytm-mods review, item 4). The page-0
list is per build (`Profile::strict_page0_pcs`): OS 1.74's is the same fifteen
instructions, found in that image at +0x6c (the first twelve) and +0x1cc (the
last three) [measured]; stock 1.74 cold-boots clean under `DTEMU_STRICT=1`.

**The real USB MIDI sender** (`dtboot --usb-midi-real`, `DTEMU_USB_MIDI_REAL=1`;
needs `--usb-host`): the sender (0x400029e0), short sender (0x40002934) and
sysex_send are no longer replaced; the simulated host reads the MIDI streaming
interface's bulk IN endpoint 0x81 (interface 5; 0x01 OUT) - up to 8 dTDs a
microframe - and decodes the USB-MIDI packets into the usual MIDI-out stream.
What the firmware needs from the controller for that [measured, OS 1.73]:
- the sender only sends with 0x413db254 == 1 and ENDPTCTRL1 set. 0x413db254 is
  set by 0x40002566, a callback of the controller's general-purpose timer 0
  (GPTIMER0LD 9999, GPTIMER0CTRL 0x81000000 = run, repeat: 10 ms; USBSTS bit
  24), once ENDPTNAK (0x178) bit 17 shows the host polled MIDI IN with nothing
  primed. The same callback turns the sender off and flushes endpoint 1 IN
  (0x40003c0a) when the oldest queued request has waited more than 400
  microframes (FRINDEX). ENDPTNAK is write-1-to-clear (usbdev.rs now); the GP
  timers and the NAK are modelled in this mode only (usbhost.rs), so the
  default mode runs as before;
- a send pops a 64-byte request (a dTD with the firmware's fields after it)
  from the pool at 0x80002200 (32, free list 0x413db390, next at +52; pop
  0x40003a10 with SR = 0x2700) and queues it on index 5 (0x40003d24: dQH at
  0x80002000 + table 0x401beee4, prime mask 0x401bef08); its completion (IOC)
  hands it back (0x40003b24 -> 0x40003938). On an empty pool 0x40003a64 pends on
  0x413db37c (sem_pend, trap #0);
- the USB interrupt is level 6 (INTC2 source 17); it defers the work to a
  forced lower-level interrupt (above).
`--usb-host-noread FROM[:TO]`: the host keeps the bus (SOF, Overbridge) but
stops reading MIDI IN. `--usb-host-sleep FROM[:TO]`: it suspends the bus (no
SOF, no transfers; PORTSC1.SUSP, USBSTS.SLI) - stock turns the sender off at
once (0x413db254 = 0). In this mode the machine also hooks sem_pend and lists
every wait that would block with the interrupt mask above 0 (`BLOCKING
sem_pend(...) at interrupt level N`).

Reproduced CRITICAL 1 with the deployed pre-fix build (rytm-mods db6eaab = the
then fw/AR173_looper.syx): 4 JUNO + 4 PLAITS voices with LFOs, all 8 pads hit
three times 20 ms apart, the host stops reading: within one burst the pool is
empty, `sem_pend(0x413db37c)` runs at interrupt level 5 (from 0x40003a9e, i.e.
inside ext_render's send), the task switch lands the CPU at address 0 (illegal
instruction, vector 4) and the OS's fatal handler halts (0x4007a444) - the
whole unit frozen. The fixed build (5e8f37a, lp3n) ran the same 4 minutes
(reading 20 s, not reading 80 s, suspended 60 s, reading 80 s) with no blocking
wait; the pool drained to empty while the host did not read and came back to
its 27 free when it read again (5 stay out: the receive requests 0x4000246e keeps queued [inferred]).

## Cache model (2026-10-16)

`DTEMU_CACHE=1` (`=log` prints stale fetches) models the V4 I- and D-caches on
the interpreter: copyback DDR per ACR0, I-cache filled from memory, `cpushl` by
set/way, CACR invalidates. A mod that writes code and runs it without push and
invalidate runs the old code there, as on the device. Stock 1.73 cold boot and
play are byte-identical to the interpreter with 0 stale fetches; about 55-62
MIPS. Details and the loader's flush loop: docs/CACHE.md.

## Power-off and the working state (2026-10-09)

The Rytm boots into its active working state (manual: "remembered even after
the power is turned off"). It is not on the +Drive filesystem and not written
while the machine runs: the firmware writes it when the supply goes, on a
supercap. Before this, every cold boot here came up in the defaults (the
working-state slots on the card were empty). OS 1.73 [measured: disassembly,
then the emulated run doing exactly this]:

- **The state**: 0x1ffbbb bytes at 0x405c8e10..0x407c89cb (DDR): header 'COKI'
  (0x434f4b49), +4 a checksum over the words from +8 (0x400fa420: sum of
  word[i] ^ (i + 1), (size + 8) / 4 words), +8 version 12, +0x0c size 100
  (<= 0xF0). Writes into it through 0x400fa520 re-stamp the header.
- **Where**: two raw eMMC slots outside the +Drive filesystem, sectors 0x7a000
  and 0x7c000 (table 0x401beaac). 0x40290de4 = the slot loaded at boot (-1:
  none), 0x40290de0 = the slot the next save goes to (the other one; set by
  0x401228da from the UI task's start, 0x400a4666).
- **Boot** (0x40122a3a): reads each slot's first 0x100 bytes and checks the
  header (0x400fa4b0); slot A wins if both are valid; loads the 2 MB from the
  valid slot; if neither is valid, looks for 'COKI' at SPI-flash 0x400000 /
  0x800000 / 0xc00000 (0x401228f4, an older home of the state, migrated to
  the eMMC slots) and otherwise starts from defaults (0x400fa44c; 0x40272514
  bit 2). A version <= 11 state is migrated (0x400fa3ea). It then registers
  the two power handlers (0x40000756 -> 0x413d83c4 = 0x40122266,
  0x4000074a -> 0x413d83c0 = 0x40121e5e).
- **Power sense**: PPDSDR_C bit 2 (0xEC09401A) high while the supply is up,
  on EPORT IRQ2 both edges (EPPAR bits 5:4 = 11, EPIER bit 2; vector 66,
  INTC0 ICR2 = 7, a non-maskable level). Its handler 0x400004b0 clears EPFR
  and reads the pin: low -> quiets the codec/SSI and GPIO, clears 0x4027250c
  and forces INTC2 source 30 (INTFRCL bit 30, level 5) -> 0x40000486 calls
  0x413d83c4; then parks the CPU at 0x40000684 (SR 0x2000, stack 0x413d83c0).
  High again (power back mid-shutdown) -> 0x4027250c = 1, forces source 29 ->
  0x40121e5e (re-initialise, then reset) [handler read, path not run].
- **Supercap**: PPDSDR_A bit 5 (0xEC094018) = charged. 0x40100a58 polls it on
  a timer until high, then sets 0x419cacc0 = 1; the handler saves only after
  that. The factory test's "SUPERCAP NEVER AT CHARGED STATE" reads the same
  pin. How long the real cap takes to charge is not known [not measured].
- **The save** (0x40122266, level 5, interrupts below masked; 0x419cacbc = 1
  while it runs): with 0x419cacc0 set and 0x40290de0 in {0, 1}, writes the
  2 MB to slot 0x40290de0 in 32 KB pieces (0x40122144: copy to 0x4f937440,
  eDMA channel 59 TCD at 0xfc045760, eSDHC polled with IRQSIGEN off: CMD23
  0x80000040 = reliable write of 64 blocks, then CMD25); then reads the first
  32 KB of the slot it booted from (CMD18), clears its first byte and writes
  it back - so only the new slot is valid; then CMD0 with argument 0xF0F0F0F0
  (GO_PRE_IDLE_STATE), a delay, and a reset (RCR 0x80) only if the power came
  back. In the emulator: 13M instructions (~65 ms emulated), 4096 + 64 sectors.
- **Before an OS upgrade** ("ERASING FLASH...", 0x4004db9c) the same write is
  made synchronously (0x4012285c): flashing an OS keeps the working state.
  So a Rytm flashed with a mod build boots the stock state, and going back to
  stock boots the mod's state - the HIGH 2 case [measured in the emulator,
  "OS upgrade over USB MIDI" below: the save lands before the first erase and
  the new OS loads it].
- Strings "MRAM STATE NOT WRITTEN", "MRAM WRITE IN PROGRESS" (0x41352d28,
  0x41352d24) belong to the factory test's status report; "SHUTDOWN" is a
  random-name word and "TURN POWER OFF" the end of the factory test - none
  is on the user's path.

**In rytm-emu**: `io.rs` models the power sense (`Io::power_lost`; PPDSDR_C
bit 2 reads 1 until then), the supercap (`Io::supercap_flat`; PPDSDR_A bit 5
reads 1: charged at once) and EPORT's flag for IRQ2 (`Io::epfr`, write 1 to
clear, INTC0 source 2's line; other EPFR bits stay plain memory). None of it is
in snapshots (old snapshots load; a resumed machine reads the cap as charged
on its next poll). `Machine::power_off(limit)` drops the sense, flags the edge
and runs until the eSDHC sees GO_PRE_IDLE (`Esdhc::pre_idle`) plus 2M, then
reports what it wrote and which slot holds a valid 'COKI' header. Nothing in
it knows a guest address: the firmware's own handler does the work, so any
build that keeps stock's handler behaves as on hardware.
- `dtboot ... --power-off AT`: at clock AT, power off; the run ends there and
  the card is written back to `--card`'s image (implies `--card-save`).
- `rytm ... --power-off`: closing the window (or the end of a `--headless`
  run) powers off and writes the card; the snapshot is not saved - start again
  without `--snapshot` for a cold boot into the saved state.

Checked (2026-10-09, `DTEMU_STRICT=1` unless noted; rytm-mods efm-173 at
7b94903: lp3n, hw1, hw1 with 0021 diag=1; scratch cards only):
- stock: a machine and knob changed on BD, power off, cold boot: BD comes up
  changed (the defaults otherwise).
- lp3n: 6-OP FM, JUNO, DIGIDRUM, PLAITS, DSP SAMPLE, DAISYSP on BD SD RS BT LT
  CH, every SRC and SMPL knob turned far up, four trigs each, BD's sound to the
  pool, LT's to the +Drive, kit saved (YES+PLAY MODE), SAVE PROJECT AS 003;
  power off. Then, each a cold boot + every track's page + PLAY 5 s + STOP +
  power off, on the same card: lp3n x3 (comes up on LT's SMPL page with all six
  machines and their knobs), stock x3 (the six tracks DISABLE, knobs greyed,
  names kept, PLAY/STOP fine), lp3n x2 (all six back, knobs unchanged after
  stock's saves), hw1 x2, hw1 diag=1 x1, lp3n again. Slots alternate A/B on
  every save; no fault, no strict access, no hang, no boot loop.
- Power off right after a JUNO list (16) or FM list (17) arrived while PAT
  turned (0, 1, 3, 10 ms later), while the sequencer plays (it boots stopped),
  inside TEENSY/Drums in the browser (listings injected), with the TEENSY
  listing request unanswered, during Overbridge streaming with the real USB
  MIDI sender while playing (earlier lp3n build's user9 snapshot), and with the
  JIT (not strict): each then cold-booted, played and powered off again clean.
- Not modelled: the supercap's charge time and running out of charge mid-save;
  the power coming back mid-save (0x40121e5e); brown-outs; the OS upgrade's
  synchronous save.
- GLOBAL settings are in the state (2026-10-09, lp3n at rytm-mods 4081a69):
  CLOCK SEND, TRANSPORT SEND and USB CONFIG = OVERBRIDGE set on a fresh card,
  power off, cold boot: all three on (lp3n, and hw1 on the same card); a fresh
  card boots with them off. The state with and without them differs in four
  bytes only: the checksum, 0x407c2d30 (clock send), 0x407c2d32 (transport
  send) and 0x405c8e4c (USB config, 1 = OVERBRIDGE) [measured; neighbours
  0x407c2d2f clock receive, 0x407c2d31 transport receive, by the menu's order
  and defaults - inferred]. tools/looper_base.sh therefore reads these after its
  cold boot and only switches on what is off: its YES presses toggle, and the
  old fixed sequence turned clock and transport send OFF on a card that had a
  power-off with them on [measured].

## Oscillator switches: FM, ring modulation, waveforms (2026-10-13)

Branch `osc-mod`. The dual-oscillator voices' seven switch bits (BD, SD, RS/CP; table
in `analog.rs` `DualSpec`) are decoded against the Syntakt, whose analog drum voices are
the same design (docs/SYNTAKT-PORT.md section 13), and FM and ring modulation now run
through a core shared with it (`src/vco.rs`).

**Decoding** [measured]: the machine set by CC 15 (Track Machine Type) on the track's
channel (BD channel 1, SD 2), its SYN knobs by CC 16..23 (a discrete parameter takes the
CC value as its index), a hit by a note on the auto channel (note 0 BD, 1 SD, 2 RS), and
`DTEMU_FRAME_TRACE` on the CV block and switch bytes. Every value of SY DUAL VCO's CFG
(80), SY RAW's two waveform parameters, BD SHARP's and BD ACOUSTIC's WAV, and BD FM, SD FM,
BD PLASTIC at their defaults:

- index 0 is the reset (pulsed on the trig only for the settings the manual marks
  with a small square; free-running settings leave it clear);
- index 1 is oscillator 1's waveform: **set = sine, clear = sinesaw** (the earlier model
  took set as sinesaw);
- indices 2, 3, 6 are the c, b, a bits of oscillator 2's waveform code, the Syntakt's
  code: CFG's five oscillator 2 waves are 101 sine, 100 skewed sine, 111 triangle, 001
  sinesaw, 011 saw in index order 2-3-6; BD SHARP's and BD ACOUSTIC's first five waves the
  same, their sixth the ring-modulation square below;
- index 4 is FM on, index 5 ring modulation (active low), as rytm-mods' TEST machines
  named them;
- the square of BD SHARP and BD ACOUSTIC is ring modulation with oscillator 1's tune
  word 0 and its level up (0x284e), oscillator 2's level 0; SY RAW's first waveform
  parameter drives oscillator 2's code (the Rytm's oscillator 1 has one waveform bit),
  and its fifth value sends code 000, which the Syntakt only sends with the oscillators
  off (open);
- **no sync**: the seven bits are all accounted for, and no machine offers it;
- the FM depth is LEVEL2 (+96 for BD): BD FM's FMA 0x0cc5 at its default, 0x32b0 at
  127, the same words as the Syntakt's BD FM; FMD sets oscillator 2's amp record (the
  pair's second record, +384 for BD). FM is on for BD FM, SD FM, BD PLASTIC and CFG's
  FM settings; ring for BD PLASTIC's second TYP, CFG's R settings, the squares above, and
  **the default kit's RS/CP machine** (ring on, oscillator 1 sinesaw, oscillator 2 sine).
  The default kit's BD is BD PLASTIC (FM on).

**Model** [inferred]: only when FM or ring modulation is on does a voice go through the
shared core (`vco::Pair`): the full waveform codes, FM linear through the core and through
zero, f1 x (1 + LEVEL2 / 4096 x osc 2 x the amp record's level / 127), oscillator 2 not
heard while it modulates; ring = oscillator 1 x the square of oscillator 2, oscillator 2
still mixed; a stopped oscillator gives +1. This replaces the phase modulation with index
8 x LEVEL2 / 32767 (independent of FMT) and the ring as a product of sines. The scale 4096
gives the same index as before at BD FM's default FMT (oscillator 2 about 1.1x oscillator
1). Voices with both switches off take the old code path unchanged, waveforms included, so
they sound exactly as before. A recording of BD FM on the user's Rytm at FMA 0 / 32 / 64 /
127 (FMT at a non-harmonic ratio, FMD up) would settle the scale, the zero crossing and the
depth's envelope.

**Measured on the emulator** (`--analog-wav`, `DTEMU_ANALOG_IDEAL=1`, filter FRQ 127):
- byte-identical to main: BD HARD, BD CLASSIC, BD SHARP (sine), SY DUAL VCO (CFG 0) on BD,
  the default kit's SD and CH hits;
- BD FM (FMT 70: f1 60.0 Hz, f2 240.1 Hz; FMD 127; 10..100 ms), f1 + f2 against the carrier:
  FMA 0 -46, 32 -21, 64 -16, 96 -13, 127 -10 dB (the old model: +9 dB at 64, its index not
  falling with FMT); peaks unchanged within 0.5 dB (-29 dBFS);
- ring (SY DUAL VCO CFG 20, 169.7 / 240.0 Hz): sum and difference at 0 dB, carrier -52 dB,
  3 f2 +- f1 at -9.3 dB (the square's third harmonic; -39 / -44 dB with the old sine product);
- BD SHARP's square: odd harmonics at -9.5 (3rd) and -14.0 dB (5th), even ones under -70 dB,
  peak -10.2 dBFS (the old model: -21.1, oscillator 1 at 0 Hz stayed silent);
- the default kit: BD PLASTIC peaks -8.9 dBFS (was -13.5), RS -14.1 (was -12.1);
- the end screens of all seven devices unchanged; the Rytm window holds SPEED 100 %.

## OS upgrade over USB MIDI (2026-10-22)

The OS upgrade runs end to end on the emulated Rytm MKII: the SPI flash takes
erase and program commands (`src/spiflash.rs`), and the reset at the end boots
the new OS from it. Measured with stock OS 1.73 receiving stock OS 1.74, the
requests as Transfer sends them (Knowledge Base, overbridge-transfer-protocols
section 3.6, encoded by elkflash), on a scratch card. Addresses are OS 1.74's
unless marked.

**The flash driver** [measured, Ghidra read-only and the emulator's trace]:
DSPI0 at 0xFC05C000, chip select PCS1, polled. Every routine clears the FIFOs
(MCR |= 0xC00), pushes words with CONT until the last byte and pops one POPR
word per push. Commands in the image: 06 WREN, 05 RDSR (bit 0 busy; 0x60 = an
erase/program error, then 30 CLSR), 03 READ (`flash_read` 0x40136482, which
the emulator still serves itself, from the same image), 02 page program
(0x401365aa, from 0x4013717a in page-sized pieces), D8 sector erase (0x401368a2),
9F ID, plus OTP/SFDP/protection reads (4B, 5A, 48, 42, 44, 2B, E0-E4) that the
upgrade path does not use.

**The part check** (0x40136b50, on the first five 9F bytes) sets the page and
sector size or fails every program and erase: Spansion 01 2018 (byte 4 = 0:
512-byte pages, 256 KB sectors; 1: 256 B, 64 KB), ISSI 9D 6017/6018, Macronix C2
2017/2018, Winbond EF 4014/7015/4017/4018, EON 1C 7015 (all 256 B, 64 KB). The
contributor's Rytm MKII reports Spansion 01 2018 with byte 4 = 00 (the factory-test
console's #STATUS, below) [measured]: an S25FL128S with uniform 256 KB sectors
and 512-byte pages, which the emulator now answers as (`spiflash::Part`;
`W25Q128` stays as a 64 KB-sector alternative). Before this model the 9F read returned junk, and that is why the
earlier test stopped on OS UPGRADE RECEIVING with no prompt: with a known ID
the prompt comes.

**The sequence** in the emulator (1.73 receiving 1.74, 1.88 MB `.syx`):
- start (0x50) -> status 0, OS UPGRADE RECEIVING. Writes in 32 KiB (58) or
  2 KiB (919) pieces, open loop or at fixed clocks, all answered in order; the
  last with status 1. Then UPGRADE OS NOW? CONFIRM YES/NO.
- YES -> ERASING FLASH...: 6 sectors of 256 KB from 0x80000 (0x4009f404:
  `(size + sector - 1) / sector` erases), then "Writing Flash": 2899 page
  programs of 512 bytes from 0x80000 (with the W25Q128: 23 x 64 KB, 5798 x 256), 1,484,288 bytes, which is the new `.syx`'s ELE3
  container byte for byte (`dtboot` compares them: no "different OS" note).
- "Success! Rebooting": a wait (0x40098c84(3000000): DTIM1 counts to its
  argument with DTMR 0x841b, a /133 prescaler on the internal bus clock, so
  about 1 us a count and 3 s here; 3.0 s in the emulator, 0.6G instructions
  between the last page and the reset [read from the code; measured]), 0 to
  0x48000000, then RCR (0xEC090000) = 0x80, a software reset
  (`move.b` at 0x4009f53a). `Io::reset_request` catches it and
  `Machine::run_until` returns `Stop::Reset` (1.73, YES at 18G: the reset at
  19.60G with the part's times, 18.68G with `DTEMU_SPI_INSTANT=1`; the
  difference, 0.92G instructions, is 4.6 s at 200 MHz: six 520 ms erases,
  2899 programs of 340 us and the OS's own polling waits).
- `Machine::reboot_from_flash` builds a new machine from the container at
  0x80000, keeping the flash and the card: it boots OS 1.74 to the same screen
  as a cold boot of the 1.74 `.syx` on that card.

**Timing and power loss.** Erase and program take the part's datasheet
typicals on the emulated clock (`spiflash::Part`, [inferred]): status reads
busy meanwhile, other commands are ignored, and the change lands at the end.
`dtboot --power-off AT` during an upgrade runs the OS's power-fail handler as
before, then cuts a flash operation still under way. Neither how far a part
gets with an interrupted operation nor how long the supercap keeps the flash
powered after the CPU parks is known, so both are settings:
`DTEMU_SPI_CUT=share|none|done` (the share of its bytes its elapsed time
covers, the default; nothing; all of it) and `DTEMU_SPI_HOLD_MS=N` (the flash
keeps power N ms after the handler; default 0).

**Sweep** (2026-10-22, from a snapshot just before YES; 1.73 receiving 1.74,
YES at 18G): the power off at 35 points from 18.00G to 19.70G (every 0.05G)
with each of the three cut settings, and at 18 points with holds of 50, 520
and 5000 ms; then a boot from the flash left. Result: the settings never
changed an outcome, and no flash operation was ever under way when the CPU
parked. Page programs (340 us) finish inside the handler's 3M instructions
(15 ms); during an erase the handler does not finish until the erase has
(44M-94M instructions) [measured]. Why: the power-fail save (1.73
`0x40122266`, called at level 5 from `0x40000486` after the level-7 handler
`0x400004b0` forces INTC2 source 30) first asks whether the flash driver's
lock is held (`0x40137124`: `*0x419f00d0 != 0`; every flash routine takes
it). If it is, a flash routine was interrupted: the save finishes the SPI
transfer under way (zeros, then an end-of-queue word, so the chip select
goes high and a started command executes), then polls RDSR (`0x80020005`,
`0x08020000`) until WIP clears (`0x401224a0`..`0x4012252c`), and only then
goes on to its card work and the CMD0 that parks the card [read from the
code, 1.73; 1.74's save `0x401222d2` has the same structure: the lock test
`0x401372f0`, the RDSR poll at `0x4012256a`]. The interrupted
code ran at SR 0x2000 (the erase routine's delay loop at 1.73 `0x40136730`),
so the interrupt itself is taken at once [measured]. So the OS completes any
erase or program it started before the board loses power, as long as the
supercap lasts the rest of it (at most one 256 KB erase: 520 ms typical, 2.6
s maximum on the S25FL128S); `DTEMU_SPI_HOLD_MS` and `DTEMU_SPI_CUT` matter
only if it does not. So the
outcome depends only on when the power goes:
- before the first erase (18.00G): the old OS is untouched and boots (1.73);
- from the first erase to the last page (18.05G to 18.95G: 256 KB to 1.5 MB
  erased, then 0 to 1.27 MB written): no bootable OS (the container's header
  without its MAIN OS, or nothing at all);
- after the last page (19.00G on, including the 3 s before the reset): the
  new OS boots (1.74). For example, 1.73 receiving
1.74 with YES at 18G and the power off at 18.85G leaves six sectors erased and
828,416 of 1,484,288 bytes written; booting that flash (`dtboot --flash F`, no
`.syx`) finds the container's header but a MAIN OS no profile knows, and
stops, saying so. A unit would go on into its bootstrap, whose check failing
opens its STARTUP MENU with OS UPGRADE (0x8000356c; 0x8000214c inferred to be
that check), the recovery path; the emulator does not run the bootstrap.

**The state save before the erase** [measured, 2026-10-22, 1.73 receiving
1.74 from a snapshot just before YES at 18G]: the project name in the working
state changed in RAM (0x405c8e2c "Analog" -> "Znalog", header checksum at
0x405c8e14 recomputed: the sum of word[i] ^ (i + 1) over (size + 8) / 4 words
from +8) before YES. After YES the OS writes the 2 MB state to the other
eMMC slot (0x7c000 here) and clears the first byte of the old one (0x7a000),
as the power-off save does; the write is complete between 18.030G and
18.035G, and the first erase starts between 18.035G and 18.040G, so the save
comes first. After the reset, OS 1.74 boots with "Znalog Rytm" at the same
address: the new OS loads the state the old one saved. With the checksum
left stale, the slot is written but the new OS rejects it and starts from
defaults (the old slot having been cleared).

**Snapshots carry the flash** (a `FLSH` trailer after the main body and the
Syntakt's DSP: the contents, LZ4, about 3 MB; WEL; an erase or program under
way with its clock span; the counters). Older snapshots load as before and
leave the flash alone. A snapshot taken mid-erase (18.402G) and resumed
reaches the reset at the same clock as an uninterrupted run (19.6035G) and
writes the same container. With `--flash FILE` the snapshot's flash runs and
is written back to FILE; dtboot notes when the two differed.

Booting from the flash (`--reboot`, `--flash` with no `.syx`, the window)
checks every packed section's byte sum first (`Firmware::from_flash`): a
container cut short anywhere, not only in MAIN OS, does not boot, as a unit's
bootstrap checks it before starting the OS ("CRC CHECK" among its strings)
[inferred]. The A4 MKII has its own flash part (docs/A4-PORT.md section 14).

Not modelled: the bootstrap that a unit runs first (the emulator loads MAIN OS
from the container as it does from a `.syx`, so only builds `symbols.rs` knows
boot, and the recovery after an interrupted upgrade cannot be followed).

The memory note that open-loop feeds lost chunks around 17G could not be
reproduced: the same 2 KiB and 32 KiB streams in one feed, and the earlier
per-file schedule (one file of ten writes a second of emulated time), arrived
in full on this build and on main (c5732f0) alike [measured, 2026-10-22].

Tools: `dtboot --flash FILE` (load if present, write back at the end; with no
`.syx`, boot the flash's OS), `dtboot --reboot` (continue across the reset;
events not yet due run on the new boot's clock), `rytm --flash FILE` (the
window reboots by itself and saves the flash with the card),
`DTEMU_SPI_TRACE=1` (each erase and program; `=all` every command),
`DTEMU_SPI_INSTANT=1` (no erase and program times).

## The factory-test console (2026-10-22)

Boot flag 0x40 (bit 6 of the entry's argument, kept at 0x40272874 on OS 1.74)
starts a text console beside the normal UI: 0x401006ee registers its input
parser (0x400816c4) and task (0x4013c164 -> 0x4013a234) and sets USB up in
mode 4, a CDC ACM serial device (1935:ffff; data interface 1, bulk 0x02 OUT
and 0x82 IN) [measured, OS 1.74]. Lines start with `#` and end with a newline
(`!` and `$$$$` start binary blocks).

Where the flag comes from: the bootstrap (OS 1.74's, 0x80000ac4, Ghidra
read-only; the emulator does not run it) sets 0x40 in three cases: FUNC,
GLOBAL, SAMPLER and CHRO held at power-on with PLAYMODE, MUTE, SCNE, PERF and
QPER up (and the OS check 0x8000214c passing); a software reset with
0xB0B0DADA at 0x48000000 (the console's #REBOOT and
#REBOOT_INTO_MAINTENANCE_MODE both write it, then RCR = 0x80, 1.74
`0x4013a234` [read from the code]; the OS upgrade writes 0 there before its
reset); or no panel answer
(0xC0, which also parks the UI). FUNC alone opens the STARTUP MENU (0x8000356c),
whose 1 ... TEST MODE is the bootstrap's own key/encoder/pad test, not this
console [read from the code; key codes named by panel::RYTM_KEYS, which the
bootstrap shares with the OS - inferred; not tried on a unit].

Commands (strings in 0x4013a234): #HELLO, #STATUS, #READ_SERIAL, #READ_TEMP,
#DUMP_CALIBRATION and other reads; also #UPGRADE (writes the bootstrap),
#FULL_UPGRADE, #FORMAT_FS, #WRITE_SERIAL and more that write the flash or the
card. #STATUS prints a block per part; its FLASH line is the first bytes of
the 9F ID (`01 2018 00 ...` on a contributor's unit and the emulator alike; the
words after it differ per unit and are not recorded here).

In the emulator: `dtboot FW --test-mode --usb-serial "#STATUS@3.5G"`
(`--test-mode` ORs the device's flag into the boot flags, here 0x00140040,
`Device::test_mode_flag`; `--usb-serial` turns on the USB host as a serial
driver, `UsbHost::cdc`, and prints what comes back). The A4 MKII has the same
console with flag 0x20 (docs/A4-PORT.md section 14). Measured on 1.74:
#HELLO -> HOW DO YOU DO?; #STATUS -> START, VERSION 1, OS 0176 1.74, PLATFORM,
FLASH 01 2018 (EF 4018 before the part was changed), DRAM, FPGA, SUPERCAP, UI (WRONG UI CARD), MMC, CODEC (FAIL).

`usbdev.rs` now keeps OTGSC's line-state bits (8..14) read-only: the
firmware's USB setup write (0x08000000) cleared the host's VBUS before, and a
cold boot with `--usb-host` stayed at "VBUS on" in every USB mode; it now
enumerates (USB MIDI as 1935:0011 on 1.73, the serial device in test mode).

**Checked later (2026-10-23):**
- *Downgrade.* OS 1.74 receiving 1.73 (32 KiB chunks, YES at 18G) accepts it,
  erases six sectors, writes 1,483,776 bytes and resets (19.603G); the
  emulator reboots into 1.73. The OS does not refuse an older image; the
  bootstrap's "DOWNGRADE NOT POSSIBLE" belongs to its own SysEx upgrade
  [measured; the bootstrap path not run].
- *Maintenance reboot.* `Machine::reboot_from_flash` keeps the bootstrap's
  rule for 0xB0B0DADA at 0x48000000 and boots with flag 0x40. In dtboot, test
  mode, #HELLO, #REBOOT (a reset 3.5k instructions later), then #HELLO on the
  new boot: HOW DO YOU DO? [measured]. dtboot re-attaches its USB host after
  a `--reboot`.
- *Transfer.* Not run against the window. It pairs MIDI ports by product name
  and otherwise asks the user to pick them ("Select MIDI ports for your
  device", overbridge-transfer-protocols 3.5): choose input `rytm-emu usb out`
  and output `rytm-emu usb in` [inferred].
