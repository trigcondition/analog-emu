# Porting to the Analog Four MKII (survey)

2026-10-07. Analog Four MKII OS 1.56 (`fw/Analog-Four_MKII_OS1.56.syx`, file sha256
`5aea489d...`), compared against Analog Rytm MKII OS 1.73/1.74. Everything here is
static analysis (disassembly, strings, `tools/xmatch.py`). Nothing has been booted yet.
Each claim is tagged **[measured]** (read directly from the image, the code or a tool's
output) or **[inferred]**.

## Summary

The A4 MKII is the same platform as the Rytm MKII. It uses the same CPU and memory map,
the same RTOS and OS framework, the same container and the same panel MCU firmware.
The RTOS and middleware addresses in the profile port almost entirely by
`xmatch.py`. The device-specific differences are:

- the panel card type is 6;
- the voice control frame is 100 x 16-bit;
- an SSI frame is 8 words;
- the calibration record is 3670 B;
- the panel key map is different.

`dtboot` refuses the image today for one reason only: no profile matches its hash.

## 1. Container [measured]

The emulator's `fwinfo` and the vendored `elektron-firmware-tool` agree on every
section hash.

- SysEx device byte 0x0B ("Analog Four MKII"). 10716 data packets, every per-packet
  checksum ok. ELE3 container: build `G0204`, version `1.56`, content checksum
  `0x10869c3d` ok.

| id | role | stored | expanded | dest | sha256 (prefix) |
|---|---|---|---|---|---|
| 5 | META (build stamp "260930 10:41:11") | raw 15 | 15 | 0 | `69ad30aa` |
| 2 | bootstrap | packed 16712 | 31438 | `0x06010000` (version word; Rytm `0x07020000`) | `838ef96e` |
| 1 | FPGA bitstream | packed 17480 | 54664 | 0 | `0617bef6` |
| 3 | MAIN OS | packed 944480 | 2500408 | **`0x40000400`** | `748fec15ddc61bc3...a474c` |
| 8 | panel MCU firmware | packed 103416 | 159948 | 0 | `6943b2f2` |

- Compression is the same aPLib-style LZ as the Rytm, with the same `[len][sum]`
  headers. Every stream-sum is ok. Nothing is signed.
- Section 8 is **byte-identical** to the Rytm's (1.73/1.74) and the Digitakt's.
- Section 1 starts with `FF..FF AA99 30A1 0007`: a 16-bit Xilinx configuration
  stream (Spartan-family) **[inferred]**. The Rytm's stream has the same header and
  is 149516 B, against 54664 B here.
- MAIN OS word 0 is `0x40001130`, which is the OS entry point (`xmatch` agrees).
- MAIN OS reads its container header at flash `0x80000`: `flash_read(0x80000, 0x20)`
  at `0x40113e54`, as on the Rytm (`FLASH_SLOT`).

## 2. CPU and peripherals

**The CPU is the same MCF5441x ColdFire V4e [measured].** Details:

- **Entry code** (`0x40001130`): sets the stack to `0x48000000` and writes CACR
  `0xa50ce100` and ACR0 `0x4007e020`. Both values are identical to the Rytm's
  (`0x400008de`).
- **Context switch** at `0x40000410`: the same code as the Rytm's, apart from the TCB
  address.
- **EMAC** is used heavily: about 2700 `mac`/`msac` and about 3300 accumulator/MACSR
  operands, against about 3450/4570 on the Rytm 1.73. The audio ISR saves and restores
  the accumulators and MACSR.
- **No FPU code.** The disassembly has no `movec` to MMU registers: only CACR, ACR0
  and VBR.

**Peripheral references** (32-bit operands per 16 KB block, A4 vs Rytm 1.74)
**[measured]**. These blocks have near-identical counts:

| block | A4 | Rytm |
|---|---|---|
| eDMA (`0xFC044000`) | 251 | 263 |
| INTC0 | 17 | 17 |
| INTC1 | 39 | 45 |
| INTC2 | 30 | 31 |
| DSPI0 (`0xFC05C000`) | 312 | 312 |
| DSPI1 (`0xFC03C000`) | 31 | 62 |
| PIT0..3 | 42 | 58 |
| DTIM0..3 | 26 | 24 |
| USB OTG (`0xFC0B0000`) | 147 | 149 |
| eSDHC | 128 | 131 |
| SSI1 (`0xFC0C8000`) | 12 | 29 |
| UART8 | 26 | 21 |
| UART9 | 21 | 22 |
| GPIO (`0xEC094000`) | 209 | 236 |

**DIN MIDI is on UART9**, set to 31250 baud at `0x401060b8` through `0x40002bb4`
(handler `0x4006400c`) **[measured]**.

**There is no DSP.** No DSP56k, "DSP" or second-CPU strings were found, and there
is no loader for a DSP. The effects and the digital parts of the audio are presumably
computed on the ColdFire with EMAC **[inferred]**.

**Classes** (Itanium RTTI names; 308 type names, about 1200 demangled lambdas)
**[measured]**:

- Framework: `AnalogFour::*` storage structs (`sound_t`, `kit_struct`, `pattern_t`,
  `project_t`), `SMVoiceStates`, `SMNoteStates`, `AnalogHandler`, `AnalogEvent`.
- UI and tools: `SynthCalibrationMenuView`, `ScaleMenuView` (pattern length and
  speed), `NoteSetupMenuView`, `CvConfigMenuView`.
- Other: `BreakOutBox*`/`BOB` (CV breakout box settings), `Ob*` (Overbridge),
  `elektron::MidiRpc*` (the Transfer protocol), `Lz4Stream*`.
- Only one source path survives: `lib/shared/sm/fade.c`.

## 3. Voices

**Control frame [measured, code reading]:**

- The voice engines fill **100 x 16-bit words at `0x80003d6c`**, with engine state
  around `0x80003e38`.
- The builder `0x400639a4` packs the words into one of two buffers at
  `0x80002800 + 496*k`. Each buffer holds:
  - at +12, the marker `0x9000AAAA`;
  - then 100 words, each `0x9001wwww` (DSPI PUSHR format);
  - then the end marker `0x18005555`, with EOQ set.
- `0x40063a2e` starts **eDMA channel 15** (TCD `0xfc0451e0`) into **DSPI1 PUSHR
  `0xfc03c034`**. The SPI and channel are the same as the Rytm's 492-word frame.
- `0x400638e4` initialises DSPI1 and the TCD.
- The callers are the audio ISRs:
  - `0x400fc0xx` / `0x401013be` (v191; installed at `0x40104f72`);
  - `0x401162a4`, an alternative v191 handler and probably the calibration mode
    **[inferred]**.
- The voice engines are around `0x400fa000..0x40106000` **[inferred, from the frame
  references]**.

**Calibration [measured]:**

- Calibrated quantities: OSC1VOL, OSC2VOL, PULPWM, TRPPWM and FILTER, per voice
  (strings near `0x401d93af`, "RUNNING ANALOG CALIBRATION").
- The record is read from SPI flash **`0x300000`, 0xE56 = 3670 B**
  (`0x400f18e8`).
- Its format is the Rytm's: magic `RBOW`, size at +4, and at +12 a checksum of bytes
  from +16 (sum of byte_i XOR i; `0x400f1884`, validated by `0x400f18ac`).
- "MISSING CAL" is shown from `0x40099902`.

**Codec link [measured]:**

- SSI1 is in synchronous mode with STCCR |= `0x56700` (`0x400013cc..ea`). That gives
  **8 words of 24 bits per frame** (the Rytm has 16), assuming STCCR's reset value
  is 0 **[inferred]**.
- Transmit: eDMA channel 54 from a 2 KB ring at `0x80002000`, 32 B per request.
- Receive: eDMA channel 52 into a 4 KB ring at `0x80001000`, 32 B per request.
- So there is one request per frame each way. `Io::ssi_requests` already derives this
  from STCCR.

**Scales [measured]:**

- The manual (OS 1.56, section 10.9.3) has a per-track **KEY SCALE**, **KEY NOTE** and
  **TRANSPOSABLE** in NOTES SETUP. These quantize and transpose the sequencer's notes.
  The manual names only MAJ and MIN as examples. Its "SCALE menu" is pattern
  length and speed, not pitch.
- The firmware has **36 scales**:
  - long names at `0x40214854`: Chromatic, Ionian ... Locr. bb3 bb7;
  - short names at `0x402148e4`: CHRO, ION, DOR ...
  - pitch-class sets at `0x4019ed5c`: 8 ints per scale, -1 terminated, index = scale-1;
  - a 16-entry key layout per scale at `0x4019f1dc` (Chromatic is `8..15,0..7`).
- There are also 8 chord names at `0x40214834` ("1-3-5" ... "1-4-7").
- Getter functions: `0x400814b6`, `0x400814c6`, `0x400814d6`, `0x400814e6`,
  `0x400814fc`. Their callers have not been traced.
- The Rytm has the same scale-name strings, but uses them as a "PAD SCALE" for the
  pads (`ScalePadToNote.cpp`, `__KB_SCALE_NUM`).

## 4. Profile (`src/symbols.rs`) for A4 OS 1.56

Every field needs an A4 value. The candidates below come from `xmatch.py` run
against Rytm 1.74, unless noted otherwise **[measured: unique masked code match /
data votes]**. Data blocks whose relative offsets match the Rytm's, such as the SD
driver block and the TCB/prio pair, are a consistency check.

**RTOS**

| field | A4 value | evidence |
|---|---|---|
| `entry` | `0x40001130` | = image word 0 |
| `ctx_switch` | `0x40000410` | |
| `task_create` | `0x4000197c` | 15 call sites, args `(tcb, entry, prio, stack, size)` |
| `sem_pend` | `0x40001ace` | |
| `pend_b` | `0x40001a5a` | |
| `queue_send` | `0x40001f4a` | |
| `queue_recv` | `0x40001ffa` | |
| `sem_post` | `0x40001c10` | |
| `current_tcb` | `0x4287c5a4` | 17 votes; written by the RTOS start `0x40142e1a` |
| `prio_heads` | `0x4287c564` | cleared by the RTOS start |

**Flash, panel and display**

| field | A4 value | evidence |
|---|---|---|
| `flash_read` | `0x401148ae` | `0x401155a2` branches to it, as `0x40136faa` does on 1.73 |
| `panel_diff` | `0x40075756` | |
| `pend_call` | `0x400780f0` | |
| `depack_copy` | `0x4007889c` | |
| `abort_loop` | `0x4007d9aa` | |
| `fb_front` / `fb_back` | `0x40214310` / `0x40214314` | |
| `display_sem` | `0x41809cd0` | |
| `completion_sem` | `0x4182a6c0` | |
| `progress_total` / `progress_done` | `0x4180a154` / `0x4180a158` | |

**SD driver and panel UART**

| field | A4 value | evidence |
|---|---|---|
| `sd_flag` | `0x41809674` | |
| `sd_capacity` | `0x41809698` | |
| `sd_status` | `0x418096a4` | |
| `sd_dma_sem` | `0x418096b0` | |
| `sd_data_sem` | `0x418096b8` | |
| `sd_cmd_sem` | `0x418096c0` | |
| `uart8_ring_ptr` / `uart8_consume_idx` | `0x4133cfb4` / `0x4133cfd4` | 1-2 votes each, weak |

**MIDI and USB**

| field | A4 value | evidence |
|---|---|---|
| `usb_midi_queue` | `0x417eb018` | 1 vote |
| `midi_event` | `0x417eaff0` | **[inferred]**: the Rytm's offset, and it is initialised next to the MIDI task's creation at `0x40067388` |
| `midi_pool` | `0x4287eca4` | |
| `midi_pool_map` | `0x4287c818` | |
| `midi_pool_sem` | `0x4287c810` | |
| `midi_short_ring` | `0x4287c898` | 1 vote |
| `midi_short_idx` | `0x417357f8` | |
| `usb_midi_send` / `usb_midi_send_short` | `0x40003578` / `0x400034cc` | |
| `sysex_send` | `0x40068bd8` | |

The general MIDI sender is at `0x400687aa`.

**Not found yet:**

- `mounted`: 40 references, no agreement.
- `usb_audio_copy`: the A4's USB audio layout differs.
- `panel_button_names`.
- `mainloop`, the intro fields, `frame_sem`, `transport_state`, `first_boot_error`:
  these are 0 on the Rytm too.

## 5. Likely first boot obstacles

1. **`Profile::for_image` refuses the hash** **[measured]** (`dtboot` panics with "no
   symbol table"). Fix: add an `A4_156_SHA256` profile.
2. **`Machine::is_rytm()` gates the bootstrap state**:
   - `dtin_hz`: the A4's main task sets DTIM2 DTMR=7 itself (`0x40106018`) and reads
     the counter at `0x40004c54`, `0x400ff1c8` and `0x40115f4a`, so DTIN must count.
   - The panel card and the button groups.
   - The analog model.

   The A4 needs its own branch. **The Rytm analog model and the Rytm calibration
   record must stay off**:
   - `analog.rs` reads the Rytm's `0x800063c0`.
   - `rytm_calibration` writes a 110706 B Rytm record at `0x300000` and `0x340000`.
     On the A4 this would fail validation or mislead the A4 engines. Boot
     uncalibrated first.
3. **The UI card**:
   - The OS wants **type 6** (`0x401195f4`, `0x4011ad78`; "WRONG UI CARD" otherwise)
     **[measured]**.
   - The UI firmware versions it carries are not traced. The panel-update path is
     around `0x40118360..0x40118470`, over the same UART8 bootloader protocol
     **[inferred]**. Section 8 is identical to the Rytm's, so try `[6, 5, 5, 1]`
     first **[inferred]**.
   - The button group count and the key/LED maps are new (keyboard, OCT keys, CV/FX
     tracks).
4. Rytm-specific capture code may assume 16-word SSI frames: the frame alignment, the
   `--wav` channel count and the window mix. Check these once audio runs.
5. **SPI flash blobs**: `0x40106fa4` probes for `COKI` at flash `0x400000`, `0x800000`
   and `0xC00000`, then copies about 2 MB to the +Drive. This is probably the factory
   content **[inferred]**. With a blank flash the probe fails and is skipped. The
   emulator's 16 MB flash covers these addresses.

## 6. Idle and scheduler [measured]

- The RTOS start (`0x40142e1a`):
  1. clears the ready lists;
  2. creates the root task (`0x40106388`: TCB `0x41865ea8`, entry `0x4010630a`,
     prio 0);
  3. starts the scheduler with `trap #0`.
- The root task creates the prio-1 main task `0x40105fb6` and then spins at
  **`0x40106338` (`bra.b *`): this is the idle task**.
- The scheduler's own no-task spin is at `0x40000fb0`, entered with SR=`0x2000` and the
  stack at `0x4133aec0`.
- The main task ends in a loop at `0x401062fa..08` that calls `0x400672fe` and
  `0x4009900c`.
- There are 11 `bra.b *` sites in total. The profile's automatic `idle_spins` scan
  picks them all up.
- Spare CPU has not been measured.

## 7. First boot (2026-10-07) [measured]

`dtboot fw/Analog-Four_MKII_OS1.56.syx --instr 3G` reaches the main UI: kit
"K001:FYRAN", 80 BPM, track 1's oscillator page (WAV SUB PW SPD PWM), with the
popup "MISSING CAL: AUD CV KBD" (no calibration record is supplied). Changes:

- `symbols.rs`: profile `a4_156` (the survey's addresses; `mounted`,
  `usb_audio_copy`, `panel_button_names` still 0).
- `machine.rs`: `is_a4()` and `a4_bootstrap_state()`: DTIN at 12.288 MHz, UI card
  `CARD_A4`; no Rytm calibration and no Rytm analog model.
- `panel.rs`: `CARD_A4 = [6, 10, 10, 1]`. The OS asks `60 02 70 00` over the
  polled panel UART (0x40118518), expects `70 06 v ..`, and when v is below its
  bundled panel firmware's version (*0x4024d9a0 = 10; the 60064 B image follows
  at 0x4024d99c) it re-flashes the panel (`80 42`, then 5-byte frames). So the
  A4 carries its own panel MCU firmware, version 10 (not section 8's).
- `dtboot`: `DTBOOT_END_PNG=FILE` saves the final screen.

Boot timeline: 13 tasks by 2.0 G instructions; +Drive read with CMD18; SSI and
eDMA running; DIN MIDI notes on channels 1..4 reach the voices (the 100-word frame
at 0x80003d6c changes: VCA words 0x7fff -> 0xffc4, pitch words move).

Spare CPU (emulator, 200 M instructions per emulated second, the RTOS idle spins
counted as idle): 50.8% idle at rest on the main page, 50.5% with four held notes.
The voices are analog, so the load is the OS, the sequencer and the digital
effects and mix, nearly constant. On hardware the CPU runs at 250 MHz with more
than one cycle per instruction, so the absolute figure is uncertain; the share is
a guide.

Open:
- The panel sends unparsed `a0..a7 xx` messages to the emulated MCU (the A4's
  extra panel hardware: keyboard? encoders?); the key/LED map is the Rytm's or
  Digitakt's, not the A4's. Map the A4's keys next.
- Where a digital voice could join: the analog voices are digitised by the codec
  for the effects and the main mix, so a digital machine would mix in after the
  analog filter (or use a CV/audio path), not through it [inferred].
- Scales: trace the KEY SCALE parameter's range and the 36-entry tables.

## 8. Panel keys and the window (2026-10-07)

- Keys: `panel::A4_KEYS`, from the OS's slot table at 0x40212158 (12 groups of 8,
  slot = 8 group + bit, indexed by the parser at 0x40064582; 0x86 = empty)
  [measured]. 89 keys. Checked on screen by pressing each (dtboot
  DTBOOT_STEPS): OSC1 OSC2 FILTERS AMP ENV LFO (0x33..0x38), ARP 0x31, NOTE 0x32,
  PERF 0x30, KIT 0x90, SND 0x91, TRK 0x92, PTN 0x93, SONG 0x94, MIX 0xb3, track
  keys 0x20..0x25 (T1..T4, FX, CV), QPER 0xb1, NO. The keyboard (0x10..0x1c, C1..C2;
  white and black keys in separate wire groups) plays: KB_G1 changes the voice
  frame. Named but not yet checked: TRNS 0x85, OCT-/+ 0xa4/0xa5, GLOBAL, TEMPO.
- Checked 2026-10-12 (each pressed alone from the booted snapshot, or in the
  combination named; screen held and after release) [measured]: TRNS held shows
  "TRANSPOSE: +0"; **0xa4 is OCT+ and 0xa5 OCT-** (the names were swapped: 0xa4
  shows "OCTAVE: 2" and moves KB C1 from note 72 to 84, 0xa5 "OCTAVE: 0" and 60;
  fixed in `A4_KEYS`); GLOBAL opens GLOBAL SETTINGS on release; TEMPO the tempo
  screen; BANK A/E "BANK A/E : CHOOSE PTN"; RECORD, PLAY, STOP the transport icon;
  LEFT/RIGHT nudge the tempo while held (79.2 / 80.8 BPM); in GLOBAL, DOWN, UP move
  the cursor, YES enters PROJECT, NO leaves; FUNC + PAGE the SCALE menu (so FUNC and
  PAGE), FUNC + SONG MODE the SONG EDIT menu, FUNC + ARP the ARP setup; PUSH_A, E,
  F, J show their knob's parameter (COARSE TUNE, LEVEL, WAVEFORM, PWM DEPTH);
  RECORD then T5 enters a trig. All 13 keyboard keys play notes 72..84 in order
  (octave 1). No visible reaction (left as named): CHAIN, FUNC + CHAIN, FILL while
  playing, FUNC + TEMPO (one tap), PUSH_LEVEL, PAGE alone, the trig keys outside
  GRID RECORD (their LEDs are in section 9).
- Window: the A4's DATA ENTRY knob I was labelled LEVEL/DATA (the Digitakt's
  label); now I.
- Encoders: wire 0..9 are DATA ENTRY A..J (each moves its own knob on screen),
  10 is TRACK LEVEL ("TRK LEVEL 64"); the OS maps 11 to nothing and 12..15 to 0
  (table 0x402122f8). QUICK PERF AMOUNT is one of two absolute inputs (pots,
  0x428a7530), not modelled.
- The panel has 12 button groups. The A4 sends `a0..a7 xx` messages to the panel
  MCU that the emulated MCU ignores (LED rows or the keyboard's lights?).
- LEDs: see section 9; the window draws them (keys glow, and LED strips for
  OCTAVE, PATTERN PAGE and PATTERN MODE).
- Window: `./target/release/rytm fw/Analog-Four_MKII_OS1.56.syx` draws the A4's
  panel (`gui::layout_a4`), MIDI ports "a4-emu in/out". No sound: the voices are
  analog and the emulator has no A4 voice model yet.

## 9. LEDs (2026-10-07) [measured in the emulator]

Two kinds, found by pressing each key from the same state and diffing the panel
MCU's LED state (`dtpeek leds`):
- Colour LEDs, the Rytm's protocol (selectors `0x2g`, slots `Bs`, palette `B5`):
  trigs 0..15, BANK E..H 17..20, RECORD 21, PLAY 22 (green while playing), STOP 23
  (white while stopped), BANK A..D 33..36, SONG MODE 38, CV 44 and FX 56 (green
  audible, red active), PERF 45, TRK 46, PTN 47, SONG 48, KIT 58, MIX 60, ARP 63.
- Row LEDs, `0xAg vv`: two bits per key, active low; a PARAMETER key's first page
  is 2 and its second page 1 (OSC2 pressed twice: `10` -> `01`), matching the
  manual's white / dimmed white. NOTE 0.0, OSC2 0.2, FILTERS 0.3, AMP 1.0, ENV 1.1,
  LFO 1.2, TRACK4 1.3, TRACK3 2.3, TRACK2 3.0; PLAY also clears row 6 bit 0.
  Not decoded: OSC1 and TRACK1 (all rows read 0xff on the OSC1 page with T1
  active, so their lit state is not "2"), key 0.1 (lit on every page but OSC1),
  rows 3..5 under OCT-/+ (the OCTAVE LEDs?), SND, CHAIN, the arrows.

**Decoded (2026-10-12) [measured].** The rows are not on/off bits: `0xAg vv` is
the A4's counterpart of the Rytm's `0x3g` selector, choosing the slot of LEDs
64 + 4g .. 64 + 4g + 3 (two bits each), whose colour is the slot's palette entry
(`Bs id vv`, `B5`), exactly as for LEDs 0..63. The "2 = lit, 3 = off" reading
above was the OS alternating slots: it writes the new colour into the unused slot
and then flips the selector. The OS keeps one shadow of all 32 selectors
(`0x4180eb64`, rows at +16; sent by `0x40075900` XOR a mask at `0x4180e9b4`, all
zero here; `0x400758ac` sends index > 15 as `0xA0 | (i - 16)`, else `0x20 | i`), and
an LED id -> LED table at `0x4180eb88` (written by `0x4007586c`): ids 0..15 trigs
0..15; 16..28 the keyboard C1..C2 (white keys 24..31, black keys 39..43); 32..37
T1 77, T2 76, T3 75, T4 71, FX 56, CV 44; 48..56 PERF 45, ARP 63, NOTE 64, OSC1 65,
OSC2 66, FILTERS 67, AMP 68, ENV 69, LFO 70; 64..70 the seven OCTAVE LEDs 78..84.
With that decode, by pressing keys from the booted snapshot and diffing colours:
- PARAMETER keys: the open page's key red on page 1, green on page 2 (OSC1 red on
  OSC1, green on NOISE); others off. Track keys: the active track red, the others
  green (audible).
- OCTAVE: one LED lit red: 82 at octave 1 (the default), 81, 80, 79, 78 at 0..-3,
  83, 84 at 2, 3; past the ends the end LED dims (78 at -4: (197,0,0), -5:
  (131,0,0); 84 at 4: (197,0,0)). Each track keeps its own octave (T2 shows 81).
- PATTERN PAGE 88..91: dim red for the pattern's pages, bright for the active one
  (RECORD then PAGE moves it from 88 to 89); PATTERN MODE 92..95: 95 lit at boot.
  FUNC + BANK A..D (manual 10.2: SEQUENTIAL, DIRECT START, DIRECT JUMP, TEMP
  JUMP) lights 95, 94, 93, 92 (2026-10-12), so the LEDs run in reverse id order:
  SEQUENTIAL 95 (the default), DIRECT START 94, DIRECT JUMP 93, TEMP JUMP 92.
  With TEMP JUMP chosen 92 is bright and 95 stays dim (131,0,0), steady over
  0.75 s with no pattern queued (the manual's flashing is when a jump is armed;
  not tried). `panel::A4_MODE_LEDS` lists them in panel order.
- FILL 32 (red while held, playing), CHAIN 37, TRNS 55 (held), SND 59, GLOBAL 72
  (red in the menu). SND held lights the trigs; TRNS held lights the track keys.
- No LED change: FUNC, the arrows, TEMPO, QPER, YES/NO, a held keyboard key.
  LED 73 is green from boot for a reason not found; 74 and 85..87 unused here.
`panel::A4_LEDS`/`A4_ROW_LEDS` name them; `led_rgb` takes LEDs 64..127's slots
from the rows; the window draws OCTAVE, PATTERN PAGE and PATTERN MODE as strips
(PATTERN MODE labelled SEQ D.ST D.JMP T.JMP, left to right).

## 10. Voice model, first sound (2026-10-07)

Architecture (manual 4.1 and appendix E): per voice two analog oscillators with
sub oscillators, a digital noise generator, ladder filter -> overdrive ->
multimode filter -> VCA -> pan, all analog; envelopes and LFOs digital (the OS
moves the control words). The dry path to the outputs is analog; the chorus,
delay and reverb are digital send effects whose returns join before the output
stage. So the codec carries the sends and returns, not the dry sound.

The voice frame (100 words at 0x80003d6c), from turning each knob of OSC2 and
FILTERS on T1 and playing notes [measured]:
- 13 words per voice at 13v: osc1/osc2 pitch trims (small, grow with pitch),
  osc1/osc2 shape (PW knob), osc1/osc2 level (inverted: 0x7fff off; OSC2 LEV
  turned up 0x7fff -> 0x5fca), ladder cutoff (0x0605 at rest, ~0x47c2 with the
  filter envelope open), ladder resonance, overdrive (0x15b4 = 0), multimode
  cutoff, multimode resonance, VCA L, VCA R (0x7fff closed; a note ramps them to
  about 0: 0x32, 0x26, 0x0d, 0xffc3 over 50 ms).
- w52..55: 0x7fff each (noise levels, inferred).
- w64+v switches: osc1 waveform bits 0-1, osc2 bits 4-5 (WAV), multimode type
  bits 8-10 (TYP: 4 -> 5, the Rytm's HP2 -> BS order; the A4 manual lists the
  same seven types as the Rytm's multimode filter).
- w68 + 4g, g = 2v + osc: 32-bit frequency word, linear in Hz, doubling per
  octave, then 0x8000, then the sub oscillator (SUB: 0 -> 1). (The first reading,
  176.53 units per Hz, was wrong; see Pitch below.)
- TX (8 words, the effect returns): only word 3 nonzero at rest (rms 708).
- Filter and drive words against the FILTERS knobs (T1, each knob turned to its
  ends and to values read off the screen; 2026-10-12) [measured]: ladder cutoff
  w6 = 257.8 x (FRQ - 20.15), clamped at 0 (0x6c9b at 127.99); multimode cutoff
  w9 inverted, 0x4a38 - 144.5 x FRQ (0x01f6 at 127.99); ladder RES w7 0x1580..0xde90;
  multimode RES w10 0..0x3f80; the "unknown" w8 is OVERDRIVE, 0x15b4..0xea4b.
  The model had the multimode cutoff the wrong way round (an LP closed as FRQ
  rose) and both resonances at half range; fixed. Cutoffs in Hz stay [inferred]
  (one knob unit a semitone, the top about 18-20 kHz).

`src/a4voice.rs` models it: polyBLEP saw/pulse, a transistor pulse as pulse plus
saw, triangle, sub squares, noise; a 4-pole ladder with tanh feedback, an
overdrive, a state-variable multimode with the Rytm's type codes; smoothed VCA L/R.
The curves are inferred. Not modelled: AM, sync, the oscillator trims, the
neighbour links in appendix E.

**Pitch (2026-10-12).** The frame builder is `0x400fee68` (copies to the frame at
`0x400ff0ce`/`0x400ff0dc`) [measured, code reading]. Per oscillator: the pitch
(signed octave in the high half, 1024-step table `0x40218fa8` with 6-bit
interpolation in the low half) -> exponential, times (1 + global tune) and shifted
by the octave; plus an LFO term (8 sines from the phases at `0x80005b28`, Q31 gain
2932031); plus 4 x (`0x80005b78 + 2v + 8o` - 0x4000) and 4 x (`0x80005b98 + 2v` -
0x4000) (both 0x4000 here), clamped to 0..0x3fffff. The frame gets the previous
block's word (one block late). The **trims** w0/w1 are
`-(((-(c0 x (f << 8)) >> 31) - c1 + 128) >> 8)`, c0/c1 the oscillator's pair in
`0x40218f68 + 8k` (the AUD calibration's first table, section 11; here 0xfe48590e,
0xffffe100): trim = -0.01341 f - 31, a linear function of the frequency word that
carries no pitch information of its own. [inferred] The A4's oscillators are reset
by an NCO and the trim is the ramp-amplitude CV that keeps the level constant as
the pitch changes (the calibration strings OSC1VOL/OSC2VOL); not modelled.

The 17 cents came from the scale: averaged over 1000 status lines a note (the
default sound's LFO moves the word by 1.3% sd), notes 24..84 give 174.72..174.79
units per Hz on both oscillators [measured]; 2^32 / 24.576 MHz (512 x 48 kHz) =
174.763: the word is a 32-bit NCO increment at the codec's master clock
[inferred, fits within 0.4 cents]. `a4voice` now uses it. Rendered pitch
[measured, headless, `--analog-wav`, spectral centroid of the fundamental over
1.0 s, note on by MIDI on T1]: OSC1 (default sound) notes 24/36/48/60/69/72/84:
+0.65 +0.14 +0.29 +0.08 -0.30 -0.73 -1.04 cents; OSC2 alone (OSC1 LEV 0, OSC2 LEV
127) notes 24..84 by octaves: -0.59 -0.33 +0.11 +0.17 -0.29 -0.12 cents. The knobs
[measured from the frequency word, C4]: TUN +10 / -10 -> +1000.00 / -999.98 cents
(a semitone a step); FIN +33 / -33 -> +25.80 / -25.78 cents (100/128 cents a step:
a semitone per 128, as the manual says).

## 11. Calibration record (2026-10-12)

Code reading in OS 1.56 [measured unless tagged]:

- Read: `0x400f18e8` reads 0xE56 = 3670 B from SPI flash `0x300000` into the buffer
  `*0x40214c10` and validates it (`0x400f18ac`): magic `RBOW`, size at +4 (size - 16
  at most 0xffef), checksum at +12 of the size - 16 bytes from +16 (sum of
  byte_i XOR i, i from 1; `0x400f1884`). One copy only (the Rytm keeps two).
  `0x400f19f2` copies it to the working record `0x448b5eed`; an invalid record
  becomes the header and zeros, format 2 (`0x400f19b0`); format 1 is upgraded to 2
  (`0x400f1a38`: the KBD fields below get their defaults).
- +8 format: 2 is current.
- The popup (`0x40099880..0x40099916`, "MISSING CAL: %s%s%s") names the parts
  whose flags are zero: **AUD** = u32 +0x44 (`0x400f1ad4`), **CV** = u32 +0x10
  (`0x400f1ab6`), **KBD** = u16 +0x24c with format > 1 (`0x400f1af2`), KBD only
  when `*0x401ff54c & 0x100000` (the boot flags' Analog Keys bit; the A4 MKII's
  bootstrap never sets it, `Device::boot_flags`).
- Boot (`0x40106206`): a valid record -> `0x40104148` (AUD) and, on a keyboard
  model, `0x400649d2` (KBD). CV is applied by `0x400fc9a2`.
- **AUD** (`0x40104148`, when +0x44 != 0): for voice v and oscillator k = 2v + o,
  +0x48 + 8k (2 words) are copied to `0x40218f68 + 8k`; the pairs +0x88/+0x8c + 8k
  and +0xc8/+0xcc + 8k become (D, S) at `0x40218f28 + 8k` and `0x40218ee8 + 8k`,
  with lo' = lo - 2000, hi' = hi + 2000 (the first clamped to 0x7fff),
  S = (lo' + hi') << 15, D = ((hi' - lo') << 15) / 63 << 6; +0x13c/+0x14c + 4v give
  `0x40218ea8 + 8v` = (0xb8cfe d) >> 16 and +4 = lo - 8d (d = hi - lo);
  +0x1dc/+0x1ec + 4v give `0x40218e48 + 8v` = (0xb8cfe d) >> 16 (signed) and
  +4 = lo - 9d; +0x16c + 4v -> `0x40218ed8 + 4v`, +0x1fc + 4v -> `0x40218ec8 + 4v`.
  Uncalibrated, those tables keep their initial values from the image. The writer is
  the calibration routine `0x400fc30a` (it sets +0x44 = 1 and saves through
  `0x400f190e`); its working values are at `0x428ab694..0x428abb0c`. Which analog
  quantity each table trims (OSC1VOL, OSC2VOL, PULPWM, TRPPWM, FILTER) is
  [inferred] from the strings, not traced.
- **CV** (`0x400fc9a2`, when +0x10 != 0), CV out c = 0..3: offset = +0x14 + 4c,
  gains = +0x24 + 4c and +0x34 + 4c, each minus a trim from `0x4045c888 + 6c`
  (zero at boot) -> `0x41864a64/54/44 + 4c`. Uncalibrated all three are 0. The CV
  outputs are voice frame words 56..59 (`0x80003ddc + 2c`; section 15) = offset +
  (gain x value) >> 8 (EMAC, clamped; `0x400fc8e6..0x400fc972`).
- **KBD** (when u16 +0x24c != 0, format 2): six 256-entry u16 tables at +0x24e
  (format-1 default: 0, 10 .. 2550) and four u16 at +0xe4e (default 150), which
  `0x400649d2` turns into gain 0x800000 / (v - 34) at `0x428a7550 + 4i` and offset
  -32 x gain at `0x428a6d1c + 4i`, for four absolute inputs (the pots near
  `0x428a7530`? [inferred]). Uncalibrated the gains are 0.

**Synthetic record** (`Machine::a4_calibration`, on by default; `DTEMU_NO_CAL=1`
for none): format 2, all three flags set. AUD values are solved from the OS's
initial tables (read from the loaded image) so the tables come out as uncalibrated:
measured after boot, every word equal except the D spans (+0.002%) and
`0x40218e48` (-18499 for -18500; the step is 11.6). KBD: the OS's own format-1
defaults. CV: offsets 0, gains 0x40000000 [synthetic; the CV outputs are not
modelled]. Result [measured]: the A4 boots to the main page without the popup; a
note (C4, T1) renders the same pitch (259 Hz before the scale fix of section 10) and level (rms within 0.6%) as
uncalibrated.

## 12. Codec slots and the digital effects (2026-10-12)

The SSI frame is 8 words each way; the RX ring is `0x80001000` (4 x 32 frames x 32 B),
the TX ring `0x80002000` (2 x 32 frames). The audio ISR (`0x401013be`) picks the
finished RX block from eDMA channel 52's DADDR and runs, per 32-frame block
(`0x40103db4..0x40103df4`): the voice-frame builder, `0x400feb10` (input stage),
`0x400fe73c` (effects, into `0x80007e68`), `0x400fedc4` -> `0x400febf0` (TX block).

**Input stage [measured]** (`0x400feb10`, found with an SRAM read watch and a probe
signal in one RX slot at a time, buffer `0x428789a0`, 8 words a frame): the OS reads
every RX word from byte offset 1 (`0x80001001 + 32n + 4j`, 24-bit sample << 8) with
the EMAC. RX 0..3 are multiplied by four gains, ramped over the block from
`0x41864b30` to targets in the ring `0x80006f28` (set by `0x400fa28a` from the voice
level code at `0x400fa654..`, index = voice), into buffer words 2..5; RX 4, 5, 6, 7
are copied unscaled into words 0, 1, 7, 6. The gains are 0 with no note and about
0.24 (Q31 0x1f0e8280) while a note on T1 sustains, unchanged by the send levels: a
digital copy of each voice's VCA level. So RX 0..3 = voice 1..4's signal before its
VCA [inferred from the gain], and RX 4/5 are EXT IN L/R [inferred: the only other
stereo audio source; not probed through the FX track's EXT IN sends]; 6/7 unknown.

**Output [measured]**: TX 4/5 carry the effects' return L/R: with a probe in RX 0, a
held note on T1 and DEL send 127 they fill with echoes; zero otherwise. TX 3 is a
noise signal (rms 708 at rest, ducked at note-on and slowly restored: the digital
noise generator that feeds the voices [inferred]); TX 2 shows a short blip at a
note-on; TX 0, 1, 6, 7 stay 0 here.

**Model** (`bus.rs`, `a4voice::Out`): each voice's modelled signal before the VCA
goes to RX 0..3 at half scale (`RX_SCALE`), `ext_in` to RX 4/5, and TX 4/5 (the
previous DAC frame) are added to the dry mix at unity (`RETURN_GAIN`). Both levels
are [inferred]. The window and `dtboot --analog-wav` play it.

Checked [measured, headless; T1 note C4 for 60 ms, sends by CC 91/92/93 on ch 1]:
- DEL 127: echoes at 0.5621 s spacing, each about 6 dB below the last (rms
  3168 dry, then 432, 213, 105, 51, 25). The kit's delay TIM is 24 (1/8 dotted):
  at 80 BPM 24/128 x 4 x 0.75 s = 0.5625 s. With TIM set by NRPN 2:50 = 32 on
  ch 5 (TIM 33; the NRPN value is TIM - 1): 0.7731 s, expected 0.7734 s.
- REV 127: a tail after the note (window rms 695, 447, 451, 285 .. decaying to
  silence in about 1.2 s); dry alone stops within 100 ms.

Open: TX 0..2, 6, 7 and RX 6/7; whether the real ADC taps the voice before or after
the overdrive/multimode; the return's level against the dry path; using TX 3 as
the voices' noise source.

## 13. Window latency (2026-10-12)

The window (`src/bin/rytm.rs`) took one model frame per 16 codec words, the Rytm's
frame size; the A4's frame is 8 words, so the voice queue drained at half rate, grew
to its 1 s cap and played every other frame, and the emulator ran at 200% to keep the
ring full. Fixed by chunking by the frame size. Key press to audio onset [measured,
`rytm --headless --audio-null --press KB_C1 --wav`, onset at 5% of peak]:
before 522 ms 2.5 s after the snapshot (growing to the 1 s cap), after 28 ms at
`--latency 20` and 72 ms at `--latency 60`; the Rytm's BD pad in the same path: 26 and
70 ms. Headless (dtboot), the OS opens the VCA words within one 5 ms status step of
the key, and the modelled audio starts about 1 ms after the press: the rest is the
window's audio ring.

## 14. OS upgrade, power cuts and the test-mode console (2026-10-23)

The Rytm MKII work in docs/RYTM-PORT.md ("OS upgrade over USB MIDI", "The
factory-test console") applies to the A4 MKII unchanged in the emulator's code:
the same SPI flash model (spiflash.rs), reset, reboot from the flash,
snapshots with the flash, power-cut settings and serial console. Measured on
OS 1.56 (the only A4 MKII OS file to hand, so the upgrade reinstalls 1.56):

- **The flash part**: a contributor's A4 MKII reports `EF 4017 00` on the
  factory console's #STATUS (2026-10-23): a Winbond W25Q64, 8 MB, which the
  OS's part check (0x40114f7c, the Rytm's list) takes with 256-byte pages and
  64 KB sectors [measured]. The emulator gives the A4 MKII that part
  (`machine::flash_part`, `spiflash::W25Q64`; addresses wrap at 8 MB as on the
  part); the Rytm keeps its S25FL128S.
- **Upgrade** (`dtboot ... --flash F --reboot`, the 1.56 `.syx` as Transfer
  sends it in 32 KiB chunks from 5G, YES at 18G): UPGRADE OS NOW?, then 17
  erases of 64 KB from 0x80000 and 4228 page programs of 256 bytes, 1,082,368
  bytes, the file's ELE3 container byte for byte; RCR software reset at
  19.724G (from 0x4007db72); the emulator boots 1.56 from the flash
  [measured]. Through the window on Windows (loopMIDI `a4-emu usb in/out`,
  elkflash): 42 chunks, 0 resends; after YES the window rebooted from its
  flash [measured].
- **State save before the erase**: as on the Rytm. The working state is the
  'COKI' block at 0x4045c870 (version 0x0b, header size 0x78, project name
  at +0x30, the Rytm's checksum rule). Renamed "Analog Keys" to "Znalog Keys"
  with the checksum fixed before YES: after YES the state is written to the
  eMMC slot at 0x7c000 and the slot at 0x7a000 is invalidated, and after the
  reset 1.56 boots with "Znalog Keys" [measured].
- **Power cuts** (with the W25Q64: 63 runs from a snapshot just before YES,
  every 0.1G from 18.0G to 19.7G with the three `DTEMU_SPI_CUT` settings,
  plus holds of 50, 520 and 5000 ms at 18.2, 18.6 and 19.0G): the settings
  never changed an outcome and no flash operation was under way when the CPU
  parked. During an erase the power-fail handler took 12M-32M instructions
  (it waits for the erase; 64 KB erases are shorter), otherwise 3M. Before the
  first erase the old OS boots; from the first erase to the last page (18.1G
  to 19.1G, 1,045,504 of 1,082,368 bytes written at 19.1G) the flash has no
  bootable OS; from 19.2G on the new OS boots. The A4's power-fail save has
  the Rytm's RDSR wait (0x40106ba6) [measured; the poll located by its DSPI0
  words]. At 19.1G MAIN OS was whole and only the panel firmware section was
  short: booting from the flash now checks every section's byte sum
  (`Firmware::from_flash`), as a unit's bootstrap checks the container before
  it starts the OS [inferred], and refuses that image.
- **Boot flags** (`Device::boot_flags`, 0 here; the Rytm MKII's 0x140000 was used
  before): the A4 MKII bootstrap (`0x80000be8..0x80000c4c`) starts them as
  PLL timeout << 17 | FPGA load status (0-5) << 12 and ORs 0x200000 when pin
  bit 5 of `0xec094018` reads low, then the test-mode and no-UI bits. It never
  sets bit 20, which 1.56 reads as an Analog Keys in 31 places: with it the
  default unit name (`0x400f6d80`, kept in the global settings and reported
  by RPC Ping) is "Analog Keys" and MISSING CAL checks KBD. A unit names
  itself "Analog Four" [read from the code; the name measured on a unit and
  in the emulator]. The name lives in the saved settings, so a card made
  before this change keeps "Analog Keys" until its settings are reset.
- **Test-mode console**: boot flag **0x20** on the A4 (0x40 there skips the
  UI: 1.56 main `0x40105fb6` tests 0x40 for the UI and 0x20 for the console
  task `0x4011b116` and USB mode 5; the bootstrap ORs 0x20 for 0xB0B0DADA at
  0x48000000 after a software reset, offset 0x832 of the bootstrap section)
  [read from the code]. `dtboot --test-mode` uses it: the CDC device answers
  #HELLO and #STATUS (VERSION 2, OS 0204 1.56, FLASH EF 4017 00, SUPERCAP
  FAIL "NEVER AT DISCHARGED STATE": the emulator's supercap is always
  charged), and #REBOOT comes back in test mode [measured]. The power-on key
  combination (bootstrap `0x80000ac2`): PERF, TRK, PTN, SONG and [TRIG 4] held,
  with FUNC, KIT, SND, MIX, [TRIG 3] and [TRIG 5] up (codes 0x30, 0x92, 0x93,
  0x94, 0x03 held; 0x80, 0x90, 0x91, 0xB3, 0x02, 0x04 up; named by
  `panel::A4_KEYS`, which the bootstrap shares with the OS - inferred, as on
  the Rytm, where the same reading proved right on a unit) [read from the
  code; not tried on an A4].
- **The window**: `a4` opens `a4-emu in` / `a4-emu out` / `a4-emu usb in` /
  `a4-emu usb out`; on Windows create those in loopMIDI.

## 15. CV outputs and CONTROL IN (2026-11-06)

**CV OUT A..D [measured]** are voice frame words 56..59 (`0x80003ddc + 2c`), not
60..63 as section 11 had it: CV A set to VALUE LIN 0.00..5.00 V, its VAL at 0, 64
and 127 sent word 56 = 0, 0x13ff and 0x27af; words 57..59 stayed 0. With the
emulator's calibration record (gains 0x40000000, offsets 0) a word is
5000 mV / 10159. How a unit's calibrated words map to its DAC's volts is not
modelled; the emulator keeps the four words (`Io::cv_out`).

**CONTROL IN 1 and 2 [traced, measured]** (EXP/CV IN 1, 2) are the ColdFire
ADC's channels 8 and 9: `0x4011806c` reads ADRSLT8/9 (`0xFC09_4012/14`), filters
them into `0x4186f474 + 4p` (`>> 19` is the 12-bit reading), and per its port
setting (`0x8000609c + 0x28p`: mode 0 OFF, 1 CV, 2 EXPR; levels from
`0x40100352`, which converts CV ZERO and MAX LEVEL in mV to ADC units with
`0x401182b0`: units = off + mV x 32000 / gain, uncalibrated off 16295 and gain
15676 at `0x40219fd4`/`0x40219fdc`) writes the input's value to `0x80005b04 + 2p`
(CV: 0x4000 at 0 V, bipolar). The emulator answers the two registers when an
input is plugged in (`--cv-in` / `DTEMU_CV_IN`: `1=A` patches CV OUT A into
CONTROL IN 1, `2=2500` holds CONTROL IN 2 at 2.5 V), with the units the OS's own
conversion expects for that voltage, so CV ZERO 0 and MAX 5000 read 0 V as no
modulation and 5 V as full. Unplugged, the registers read back as memory, as
before. Checked: CV A VALUE LIN 0..5 V looped into CONTROL IN 1 (CV, 0..5000),
CONTROL IN 1 MOD at full depth on a parameter: VAL 0, 64, 127 gave 0x3ff4,
0x6033, 0x7ff3 at `0x80005b04` and the parameter's value moved from its base to
half and to the top.
