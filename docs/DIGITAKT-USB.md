# The Digitakt's USB in the emulator

OS 1.53 and 1.54 run their own USB stack here: `dtboot FW.syx --usb-host` (also the window's `--usb-host`).
Nothing of it is proved on a Digitakt; tags as in `RYTM-PORT.md`.

## Why it did not start

The stack waits for VBUS: `0x40005f2c` (USBCMD.RS) runs from the OTGSC.BSV interrupt, or at the mode change if BSV is
already set. `usbhost.rs` raised BSV on its first microframe, and on a cold boot the firmware's setup then wrote OTGSC =
0x08000000 (`0x40006f44`), which the model stored whole. BSV is read-only on the part, so the fix is in `usbdev.rs`. A cold-booted Rytm stopped in the same place [measured]; the Rytm's runs attach the host to a snapshot
taken after the setup write, so it never showed. No device model, profile gate, peripheral or hash lock was involved.

## What the firmware offers [code: the table at 0x401f5650, 40 bytes a mode; measured for 2, 5 is read only, 6]

| USB CONFIG | mode | id | interfaces |
|---|---|---|---|
| USB MIDI (a fresh card) | 2 | 1935:000d | audio control 0, MIDI 1: bulk 0x01 / 0x81, 512 bytes |
| OVERBRIDGE | 6 | 1935:0b2c | vendor 0..3: 0x83 368 bytes (alt 3), 0x03 88 bytes (alt 3), 0x02 bulk, 0x82 interrupt; MIDI 5 |
| USB AUDIO/MIDI | 5 | 1935:102c | class-compliant isochronous on 1 and 2, MIDI 4. Not driven: the host never sets the alternates and reads no isochronous endpoint |

Modes 1, 3 and 4 are the full-speed MIDI pair and the serial test device; nothing here selects them.

## What the host does on a Digitakt

`UsbHost::device_stack`, set by `Machine::usb_microframes` on `Device::Digitakt` only:

- reads the configuration descriptor and takes each vendor interface's largest alternate, so nothing is set in USB MIDI mode
- follows the firmware off the bus: RS off (a USB CONFIG change) is a disconnect, RS on a new enumeration
- MIDI into the device goes on bulk OUT 0x01 as USB-MIDI packets (`Machine::usb_midi_send`, `--usb-midi`, the window's "usb in"). There
  is no OS queue stand-in on the Digitakt
- the OUT stream takes the descriptor's packet: 88 bytes, 2 channels (the first two of `Machine::ob_in`)
- reads the vendor interrupt IN 0x82 (`stats.vendor_in_bytes`; it stayed 0 in every run)
- the firmware's own USB MIDI sender runs (`set_usb_midi_real`). In a patched image the stand-in at 0x40004990 only takes over
  without a host

The Rytm's host is the old one, line for line, with `device_stack` false.

For tests: `--usb-host-ack N` answers every SysEx with an N-byte one, `--usb-host-midi-every N` reads MIDI IN one dTD each N
microframes, `DTEMU_USB_SEND_LOG=1` logs each message as the host reads it.

## Measured

- Cold boot on 1.53 and 1.54: enumerates in USB MIDI mode. OVERBRIDGE selected on the panel: leaves the bus, comes back as 1935:0b2c.
- A CC from the host on bulk OUT reaches the OS (`B0 69 11` sets LFO DEST to TRX-B2:PTCH), a device inquiry is answered on bulk IN.
- Overbridge, 1.53: 7 frames every block both ways, no bad block, 47880 frames/s in 25 emulated seconds, MIDI alongside. A sine on
  host channel 0 reaches the DAC's left word at -18 dB (USB TO MAIN 0 dB).

## Not modelled

- The stream IN's content is unchecked: a blank card has no sample to play, and the Digitakt's model feeds nothing into the SSI
  receive (`--audio-in` is the Rytm's), so all 12 channels are zero. Only the framing is verified.
- The Overbridge plug-in's vendor protocol on 0x02 / 0x82: the host sends nothing there.
- USB AUDIO/MIDI's isochronous streams. The host always reports high speed.
- Bus timing: the CPU has no wait states, and a dTD completes in the microframe the host takes it.
- `--ob-link` audio and MIDI: its poll sits in the Rytm's `voice_output_mix` hook, which the Digitakt's profile lacks.
