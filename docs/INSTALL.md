# Installing rytm-emu, step by step

This guide assumes no programming experience. You type a few commands, wait a few minutes, and the emulator window opens.

Supported devices: Analog Rytm MKI and MKII, Analog Four MKI (and Analog Keys) and MKII, Syntakt, and the Digitakt MK1. The emulator runs the device's own OS, so you need the OS file for your device (step 4). It is not included.

Commands go into a **terminal**:
- macOS: open **Terminal** (Cmd+Space, type "Terminal", press Enter).
- Linux: your terminal app.
- Windows: see the Windows section first.

Type or paste each command, press Enter, and wait until it finishes before the next one.

---

## 1. Install the tools (once)

### macOS

1. Install Apple's developer tools:

       xcode-select --install

   A window pops up. Click **Install** and wait until it says done. If it says they are already installed, that's fine.

2. Install Rust (the programming language the emulator is written in):

       curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

   When it asks, press **Enter** for the default installation.

3. **Close the terminal window and open a new one**, so it finds the new tools. Check with:

       cargo --version

   You should see something like `cargo 1.9x.x`. If you see "command not found", close and reopen the terminal again.

### Linux (Ubuntu / Debian)

    sudo apt-get update
    sudo apt-get install -y build-essential pkg-config libasound2-dev libx11-dev libxcursor-dev libxkbcommon-dev curl
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

Press Enter for the default installation, then close and reopen the terminal.

### Windows

The emulator does not build directly on Windows yet: it uses virtual MIDI ports that Windows does not offer. Use **WSL** (Linux inside Windows), which can also show the window:

1. Open **PowerShell as administrator** and run `wsl --install`. Restart when asked, and pick a username and password for Ubuntu.
2. Open **Ubuntu** from the Start menu and follow the **Linux** steps above.
3. In the steps below, your Windows files are under `/mnt/c/Users/<your name>/` (Downloads, for example, is `/mnt/c/Users/<your name>/Downloads`).

Sound under WSL depends on your Windows version (WSLg); the window and screen work in any case.

---

## 2. Get the emulator

Download the code as a zip:
- from GitHub: open the project page, click the green **Code** button, then **Download ZIP**;
- or use the zip shared on Discord.

Unzip it, for example into your home folder, so you have a folder `rytm-emu`. Then go into it in the terminal:

    cd ~/rytm-emu

(If you unzipped it somewhere else, type `cd ` with a space and drag the folder onto the terminal window, then press Enter.)

---

## 3. Build it (once, and again after each update)

    cargo build --release

The first time this downloads its libraries and takes a few minutes. It's done when you see `Finished`. Warnings in yellow are fine. Red `error` lines are not; see Troubleshooting.

---

## 4. Add your device's OS file

1. Download the OS for your device from Elektron's support page (the "OS" or "firmware" download, a zip).
2. Unzip it. Inside is a `.syx` file.
3. Put that `.syx` file in the `fw` folder inside `rytm-emu`, so it sits at `rytm-emu/fw/`. Create the `fw` folder if it isn't there.

Versions the emulator knows:

| Device | OS version | File name in `fw/` |
|---|---|---|
| Analog Rytm MKII | 1.73 | `Analog-Rytm_MKII_OS1.73.syx` |
| Analog Four MKII | 1.56 | `Analog-Four_MKII_OS1.56.syx` |
| Analog Rytm MKI | 1.74 | `Analog-Rytm_OS1.74.syx` |
| Analog Four MKI / Analog Keys | 1.56 | `Analog-Four_Analog-Keys_OS1.56.syx` |
| Syntakt | 1.42 | `Syntakt_OS1.42.syx` |
| Digitakt MK1 | 1.53 | `Digitakt_OS1.53.syx` |
| Digitakt MK1 | 1.54 | `Digitakt_OS1.54.syx` |

Another OS version won't start: each version needs its own support in the emulator.

---

## 5. Make a memory card (once per device)

The emulator keeps the device's +Drive (projects, sounds, samples) in a file. Create one, named after your device:

    ./target/release/dtcard rytm.img format

Use a different name per device (`a4.img`, `syntakt.img`, ...); cards of different devices don't mix.

---

## 6. Start it

Analog Rytm MKII:

    ./target/release/rytm fw/Analog-Rytm_MKII_OS1.73.syx --card rytm.img

Analog Four MKII (shortcut, uses `fw/Analog-Four_MKII_OS1.56.syx` and `a4.img`):

    ./target/release/a4

Any other device: the same `rytm` command with that device's file and card, for example:

    ./target/release/rytm fw/Syntakt_OS1.42.syx --card syntakt.img

The window opens and the device boots like the real one. Some first boots show a question or a popup; answer it with the YES/NO keys in the window.

### Controls

- **Click** a key to press it. **Shift-click** keeps it held.
- **Drag** a knob up/right (or scroll over it) to turn it. **Click** a knob to push it, **Ctrl-drag** to push and turn.
- On the Rytm, a pad's **velocity** is where you click it: higher is harder.
- **Esc** releases all held keys.
- The emulator creates MIDI ports (e.g. "rytm-emu in"/"rytm-emu out") that your DAW or a keyboard can use.
- `--latency 20` after the command gives a snappier response; leave it out if the sound crackles.

Close the window to quit. Nothing is written to your real device: the emulator never touches hardware.

---

## Troubleshooting

- **`command not found: cargo`**: close the terminal and open a new one. Still not found: repeat the Rust install in step 1.
- **`xcrun: error` or `linker 'cc' not found` (macOS)**: run `xcode-select --install` again and let it finish.
- **`No such file or directory` for the `.syx`**: the file isn't in `fw/` or its name differs. Check with `ls fw` and compare with the table.
- **It stops right away with an error about the firmware or its profile**: the emulator doesn't support that OS version (or the file is damaged). Use the version in the table, downloaded again.
- **No sound**: check your Mac/PC output device and volume. Try without `--latency`.
- **Something else**: copy the red error text and ask on the Discord.

To update later: download the new zip, unzip it over the old folder (keep your `fw/` folder and `.img` cards), and run `cargo build --release` again.
