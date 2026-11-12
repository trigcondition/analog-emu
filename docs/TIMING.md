# Unit timing

`DTEMU_TIMING=unit` makes the emulator's clock count ColdFire core cycles instead of instructions, so the
CPU load the firmware measures (analoghealth's CPU % and DSP %, the UI tick rate) follows the unit's. Without
it nothing changes: one tick per instruction at 200 M a second.

| variable | values |
|---|---|
| `DTEMU_TIMING` | `unit` (sampled, about real time) or `exact` (every instruction measured, about 3x slower) |
| `DTEMU_TIMING_PARAMS` | `key=value,...` overrides of the parameters below; `emmc_*` keys belong to the eMMC model |
| `DTEMU_TIMING_FN` | a function address (hex): its calls and average cycles, callees included, in the summary |

dtboot prints `busy=X%` on each report line (1 minus the idle time skipped in that window) and a summary at
the end: cycles by cause, cache hit rates and the busiest peripheral pages. The JIT is off under timing.

## Model and sources

All instruction costs are from the MCF54418 Reference Manual (NXP MCF54418RM, Rev. 8), section 3.3.5,
tables 3-13 to 3-20. src/timing.rs lists them with the table each comes from.

- Clock: 264 MHz, twice the 132 MHz bus clock the PIT rates establish (the MCF5441x runs its bus at fsys/2).
- Instructions: table costs; the load-use stall of 3.3.5.1 (2 or 3 cycles); EMAC accumulator stalls.
- Branches: 8-entry branch cache plus 128-entry prediction table with 2-bit counters (3.1.1.1), mispredict 8;
  RTS through the 4-entry return stack.
- Caches: tag-only 8 KB 4-way I and D (section 6.2), following CACR and ACR0-3, CPUSHL and INTOUCH.
- Memory: the manual assumes zero-wait memory, so DDR fills, write-backs, uncached accesses and peripheral
  accesses cost the fitted numbers below.
- Exceptions and interrupts: 17 extra (TRAP's 18).

Not modelled: MOVE folding (`fold` is a fitted fraction, 0 by default), IFP/OEP decoupling, DDR banks and
refresh, eDMA bus contention. Cycle exactness is not the aim; the load within a few points is.

## Parameters

| key | default | meaning |
|---|---|---|
| `hz` | 264e6 | core clock |
| `exc` | 17 | exception or interrupt entry |
| `mispredict` | 8 | branch misprediction (manual) |
| `dmiss` | 42 | D-cache line fill from DDR (fitted) |
| `wb` | 4 | dirty line push, mostly hidden by the push buffer |
| `imiss`, `imiss_seq` | 42, 42 | I-cache line fill after a change of flow / of the next line (fitted) |
| `unc_rd`, `unc_wr` | 38, 8 | DDR through the uncached alias (fitted / estimated) |
| `sram`, `io` | 0, 10 | internal SRAM, peripheral register |
| `fold` | 0 | share of MOVEs issued free with the instruction before |
| `window_us`, `period_us` | 2000, 20000 | sampled mode: measure this long of each period (gaps jittered 0.5-1.5x) |
| `warm` | 0.5 | share of each window spent re-warming the cache tags before learning |

## Sampled mode

Between windows the cache tags and branch tables are not updated, and each predecoded block is charged its
learned cost (a running average of its measured runs; blocks not seen in a window yet get the average of
first runs, which run cold). Windows re-warm the tags for `warm` of their length before learning.

On AH10 (stock 1.74 + analoghealth, boot, three NO taps, 8-12 G cycles) sampled mode reads about 1.5 points
below exact mode (busy 73.5 vs 75.1 %, analoghealth CPU 72.5 vs 73.9 %). It runs a 12 G cycle boot in about
50 s with four runs sharing the machine (45 s of emulated time).

## Calibration so far (2026-11-11)

Unit figures are analoghealth stats from the hardware test log (T138 AH10, T175 builds). Emulator figures are
analoghealth's own stats read over USB MIDI in dtboot (`--usb-midi` with the stats request,
`DTBOOT_USB_OUT`), exact mode unless noted, bare boot with no project and no Sidecar traffic.

| build | unit CPU % | emulator, exact |
|---|---|---|
| AH10 (stock + analoghealth) | 75.3-75.6 (DSP 70.8) | analoghealth 73.9 (DSP 68.8), busy 75.1 |
| CTL1 (stock + core 0.10 + analoghealth) | 81 | busy 75.1 |
| C11F, C11N, CH01, CH03 (core 0.11 variants) | 85-90 | busy 75.3-75.4 |
| L10L (core 0.10 + lock-check) | 90 | busy 88.4 |
| V11A, V11F, V11P (0.11 + lock-check) | 95-100 | busy 88.4-88.8 |

On V11P, analoghealth's own CPU figure (sampled, about 77 %) sits well under busy (82 % sampled): with
lock-check, time outside the idle task's loop counts as busy to one and not the other. Which matches the
unit's reading better is still open.

Lock-check's cost shows in the emulator (+13 points against about +9 on the unit). Core 0.10 and 0.11's costs
(about +5 and +9 on the unit) do not show on a bare boot: they come from hooks driven by the unit's
conditions at the time (Sidecar USB traffic, a loaded project), not from the CPU model. The next calibration
step is running the same elkbench plans on the emulator with timing on, so both sides do the same work.

## With the Sidecar's Overbridge stream (2026-11-11)

The unit's readings were all taken behind the Sidecar Pi in usb=mirror mode, and the Pi runs the
Overbridge stream both ways from the moment it sets the Rytm up (about 6857 blocks of 368 bytes a second
each way), PC or no PC. core 0.10/0.11's wrappers around the OS's memcpy/memset run for every caller,
the stream's handlers included, so most of the per-build load only appears with the stream on.

To reproduce it in dtboot: `--usb-host`, dismiss the boot notices (NO at 8.01, 8.21, 8.41, 8.61, 8.81,
9.01 G cycles), set GLOBAL > SYSTEM > USB CONFIG to OVERBRIDGE (keys from 9.5 G, 100 M apart: GLOBAL,
DOWN x7, YES, DOWN, YES, UP, YES, NO x3), then `--usb-host-gone 11.3G:11.5G` so the host enumerates
again in the new mode; read stats from about 15 G. The emulator's card has no project of the unit's.

In rytm (window or `--headless`): `--usb-overbridge [SECS]` does the same without keys. At SECS of emulated
time (default 9) it sets what the menu's OVERBRIDGE item sets on OS 1.74 (0x4006c3be: USB CONFIG
0x405c8e4c = 1, then 0x40004e68(6): USB mode 0x413db2d4 = 6 and its descriptor table 0x413db2d0 =
0x402728d8 + 6 x 0x28), and the simulated host leaves for 0.2 s and enumerates again (263-byte
configuration) and streams both ways. It implies `--usb-host`, works beside `--usb-midi-real`, and leaves a
build alone (with a message) whose USB mode words do not look like OS 1.74's. Setting 0x405c8e4c alone is not
enough: the descriptors stay USB MIDI's (101 bytes) and nothing streams. CTL1 under `DTEMU_TIMING=unit` with
it: 80.8-81.2 % / DSP 75.7-75.9 % at s81-90 (unit 80.6-81.2 / 76.0).

Sampled mode, defaults, idle on the card's own project, analoghealth CPU / DSP % (4 readings, 15-19.8 G):

| build | emulator | unit (T175, T138) | CPU difference |
|---|---|---|---|
| AH10 | 80.4-80.9 / 76.0 | 75.3-75.6 / 70.8 | +5.3 (T138 ran on another day and setup; same mods as CTL1) |
| CTL1 | 80.5-81.0 / 76.0 | 80.6-81.2 / 76.0 | 0 |
| C11F | 83.0-83.3 / 78.4 | 85.4-85.8 / 80.2 | -2.4 |
| C11N | 88.6-89.3 / 83.9 | 90.0-91.2 / 84.7 | -1.5 |
| CH01 | 88.8-89.3 / 84.0 | 90.1-90.3 / 84.7 | -1.2 |
| CH03 | 83.1-83.3 / 78.4 | 85.0-85.3 / 80.0 | -1.9 |
| L10L | 85.6-86.1 / 78.2 | 88.7-90.5 / 78.3 | -3.6 |
| V11A | 96.6-97.1 / 85.6 | 93.5-95.6 / 80.8 | +2.4 |
| V11F | 87.9-88.6 / 80.0 | 100 / 82.8 | -12 (the unit's heap was falling: worker starvation, T150-T152) |
| V11P | 96.2-97.6 / 85.6 | 100 / 86.5 | -3 (saturated on the unit) |
| V11L | 83.8-84.3 / 78.5 | 87.1-88.1 / 80.8 | -3.6 |

Without the stream every build read 72.5-78 %. With it, nine of eleven builds are within about 3.6 points
and the order of the builds matches the unit's except V11F.

## Notes playing (T138, AH10)

T138's plan B played bursts of 8 notes on channel 14 (notes 0, 1, 2, 4, 5, 6, 8, 10, velocity 100) about every
0.2 s. Replayed in dtboot as a USB MIDI file every 52.8 M cycles from 15 G (sampled mode, CPU / DSP %):

| AH10 | emulator | unit (T138) |
|---|---|---|
| idle, stream off | 75.1 / 70.2 | 75.3-75.6 / 70.8 |
| notes, stream off | 93.5 / 86.1 | 90.4-90.8 / 84.2-84.3 |
| idle, stream on | 80.7 / 76.0 | - |
| notes, stream on | 99.0 / 92.2 | - |

T138's idle figure matches the stream-off column, so that run most likely had no Overbridge stream (inferred).
With notes the emulator reads about 3 points high; part of it is the replay's fixed 0.2 s period (the plan's
note steps take longer) and the card's own kit instead of a new project at LEV 127.
