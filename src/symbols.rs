//! Guest addresses, per MAIN OS image.
//!
//! Digitakt OS 1.53's were resolved by the reference project's signature rules
//! (docs/mk1/06-symbols.md there). Analog Rytm MKII OS 1.73's were matched from
//! those by `tools/xmatch.py` (docs/RYTM-PORT.md has the evidence). Each table is
//! valid only for the image whose MAIN OS hash it carries; `Profile::for_image`
//! refuses anything else, so a different build fails loudly instead of hooking
//! the wrong code. 0 means "not found in this image": the hook is not installed.

pub const DT_153_SHA256: &str = "4b47a9507758ca5669ca02ab2c0374d2c04c98aece445408295cc1dcb265c5df";
pub const AR_174_SHA256: &str = "34cd0c4da8e4b810d1b25284475a99ea53821e2ab7d30a978bf15dc578251f3b";
pub const AR_173_SHA256: &str = "28d9ef40c895e03f17dfce096543109adaf708947392db72818fc2a412d685d6";
pub const A4_156_SHA256: &str = "748fec15ddc61bc3ff66047facba74328934c5fb59f427ddaf34a4e2629a474c";
/// The MKI units' MAIN OS (ELE2 files, depacked to the end of input).
pub const A4MK1_156_SHA256: &str = "cc19347b11d220aba73e4fd7203b2ce4e6d6d9553308d713648885682c714c26";
pub const ARMK1_174_SHA256: &str = "6d021e22e8912cfc1ee84e619c35db0174455cbc42bf64dbec1f1d06a5ab811d";
/// Syntakt OS 1.42 (two ColdFires; this is CPU1's MAIN OS, section 3).
pub const ST_142_SHA256: &str = "1a9723b19fb54d0566c1f690379d2e6dfe43227f77e170ad731dd029430eb3e7";
pub const MAIN_LOAD: u32 = 0x4000_0400;

/// Which unit an OS image is for: it selects the board around the CPU (memory
/// size, flash, panel, voices), not just addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    Digitakt,
    RytmMk2,
    A4Mk2,
    /// Analog Rytm MKI: parallel NOR flash, 122x32 LCD, 1-bit LEDs.
    RytmMk1,
    /// Analog Four MKI / Analog Keys: as the Rytm MKI, with 64 MB of DDR.
    A4Mk1,
    /// Syntakt: a Digitakt-style main CPU plus a second ColdFire ("DSP") for
    /// the digital voices behind an FPGA mailbox (docs/SYNTAKT-PORT.md).
    Syntakt,
}

impl Device {
    /// An MKI unit (parallel NOR flash and CS1 memory, 122x32 LCD).
    pub fn mk1(self) -> bool {
        matches!(self, Device::RytmMk1 | Device::A4Mk1)
    }

    /// The boot flag that starts the factory-test console (a USB serial
    /// device beside the normal UI), as the bootstrap sets it for a power-on
    /// key combo or a maintenance reboot: 0x40 on the Rytm MKII (1.74 main
    /// `0x401006ee`, bootstrap `0x80000ac4`), 0x20 on the A4 MKII (1.56 main
    /// `0x40105fb6`, where 0x40 means no UI; bootstrap offset 0x832) [read
    /// from the code]. None where it was not looked for.
    pub fn test_mode_flag(self) -> Option<u32> {
        match self {
            Device::RytmMk2 => Some(0x40),
            Device::A4Mk2 => Some(0x20),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Profile {
    /// Which image this table is for, e.g. "Digitakt OS 1.53".
    pub name: &'static str,
    pub device: Device,
    pub sha256: String,
    pub entry: u32,
    pub flash_read: u32,
    pub ctx_switch: u32,
    pub current_tcb: u32,
    pub prio_heads: u32,
    pub task_create: u32,
    pub sem_pend: u32,
    pub pend_b: u32,
    pub queue_send: u32,
    pub queue_recv: u32,
    pub mainloop: u32,
    pub panel_diff: u32,
    pub fb_front: u32,
    pub fb_back: u32,
    pub intro_pit3_isr: u32,
    pub intro_done: u32,
    pub intro_park: u32,
    pub frame_sem: u32,
    pub display_sem: u32,
    pub completion_sem: u32,
    pub pend_call: u32,
    pub depack_copy: u32,
    pub sd_flag: u32,
    pub sd_status: u32,
    pub sd_cmd_sem: u32,
    pub sd_data_sem: u32,
    pub sd_dma_sem: u32,
    pub sd_capacity: u32,
    pub uart8_ring_ptr: u32,
    pub uart8_consume_idx: u32,
    pub transport_state: u32,
    pub panel_button_names: u32,
    pub mounted: u32,
    pub first_boot_error: u32,
    pub progress_done: u32,
    pub progress_total: u32,
    pub abort_loop: u32,
    /// The MIDI task's USB MIDI input queue (`queue_send`'s queue; interface
    /// 2, or 4 on the auto channel). Its items are chains of `midi_pool` blocks.
    pub usb_midi_queue: u32,
    /// The event the MIDI task waits on, posted with `sem_post` after an item
    /// is queued (as the DIN receiver does, `0x40080f7a`).
    pub midi_event: u32,
    pub sem_post: u32,
    /// The MIDI block pool: 1024 blocks of 160 bytes, `{len, next, data[152]}`,
    /// at `midi_pool`; bit n of word w of `midi_pool_map` set = block 32w+n used.
    pub midi_pool: u32,
    pub midi_pool_map: u32,
    /// Counting semaphore (count at +0) of messages the USB producer may have
    /// in flight, 128; taken per message, given back when the task frees it.
    pub midi_pool_sem: u32,
    /// The USB MIDI device-side senders, `(len, ptr)`: any length, and up to 2
    /// bytes (both return at once when no host is attached). The general MIDI
    /// sender `0x4008575e(len, ptr, interface)` calls them for interfaces 2
    /// (USB) and 4 (DIN and USB) [measured].
    pub usb_midi_send: u32,
    pub usb_midi_send_short: u32,
    /// Short MIDI messages for the MIDI task: 256 four-byte slots `[status,
    /// d1, d2, len]` at `midi_short_ring`, next index at `midi_short_idx`
    /// (allocator `0x400812de`); a queue item is a slot's address.
    pub midi_short_ring: u32,
    pub midi_short_idx: u32,
    /// The receive-buffer copy, `(block, dest)`: 32 frames of 12 words into
    /// the buffer the class-compliant USB audio stream sends verbatim (MAIN L/R,
    /// voices by slot, AUDIO IN L/R) [rytm-mods compressor-hardware.md].
    pub usb_audio_copy: u32,
    /// Sends a SysEx stream to an interface, `(buf, len, abort_flag, interface)`
    /// -> true when sent.
    pub sysex_send: u32,
    /// `voice_output_mix`: builds each voice's digital-layer DAC output every
    /// block, adding the Overbridge input channel of each voice from the double
    /// buffer `ob_in_buf` (2 x 32 frames x 12 x i32; the half read is
    /// `*ob_in_index`) [measured, rytm-teensy docs/overbridge-voice-inputs.md].
    pub voice_output_mix: u32,
    /// The voice-board frame sender's PC just after its copy loop (`transport`, called with the 984-byte control
    /// frame 0x800063c0 from every block path after the filter and amp consumers wrote it): the analog model takes
    /// the frame as sent there, not the live one that later writers (a mod's hook in the sender) have not finished
    /// [code: Rytm 1.73 0x4007a538, its copy loop ends at 0x4007a598; rytm-teensy docs/POLYPHONY.md 5.1]. 0: the live
    /// frame (other builds: not traced).
    pub voice_frame_sent: u32,
    pub ob_in_buf: u32,
    pub ob_in_index: u32,
    /// MKI: the bootstrap MAIN OS carries for the flash's boot block,
    /// [start, end) (programmed at flash 0), and its 8-byte version record
    /// (programmed at the boot block's last 8 bytes, `boot_ver_flash`).
    pub boot_blob: (u32, u32),
    pub boot_ver: u32,
    pub boot_ver_flash: u32,
    /// Every `bra.b *` in the image: the RTOS idle points.
    pub idle_spins: Vec<u32>,
}

/// Stock OS 1.73's first 16 bytes at each code address the Rytm profile hooks or
/// calls. An image whose hash is not known but which matches all of them is a
/// modified 1.73 (rytm-mods builds patch elsewhere: caves, detours) and runs on
/// the 1.73 profile.
const AR_173_CODE: &[(u32, [u8; 16])] = &[
    (0x40000870, [0x41, 0xef, 0x00, 0x04, 0x23, 0xd0, 0x40, 0x27, 0x25, 0x14, 0x2e, 0x7c, 0x48, 0x00, 0x00, 0x00]), // entry
    (0x401362B6, [0x4f, 0xef, 0xff, 0xf4, 0x48, 0xd7, 0x04, 0x0c, 0x24, 0x2f, 0x00, 0x10, 0x26, 0x2f, 0x00, 0x14]), // flash_read
    (0x40000410, [0x46, 0xfc, 0x27, 0x00, 0x2f, 0x48, 0xff, 0xfc, 0x20, 0x79, 0x42, 0xf6, 0x79, 0x24, 0x48, 0xe8]), // ctx_switch
    (0x40000DE4, [0x20, 0x2f, 0x00, 0x0c, 0x72, 0xfc, 0xc2, 0xaf, 0x00, 0x14, 0x20, 0x6f, 0x00, 0x04, 0xe5, 0x88]), // task_create
    (0x40000F36, [0x22, 0x6f, 0x00, 0x04, 0x40, 0xc0, 0x46, 0xfc, 0x27, 0x00, 0x4a, 0x91, 0x6f, 0x04, 0x42, 0x91]), // sem_pend
    (0x40000EC2, [0x22, 0x6f, 0x00, 0x04, 0x40, 0xc1, 0x46, 0xfc, 0x27, 0x00, 0x20, 0x11, 0x6f, 0x06, 0x53, 0x80]), // pend_b
    (0x400013B2, [0x2f, 0x0a, 0x2f, 0x02, 0x20, 0x6f, 0x00, 0x0c, 0x40, 0xc2, 0x46, 0xfc, 0x27, 0x00, 0x45, 0xef]), // queue_send
    (0x40001462, [0x58, 0x8f, 0x60, 0xf2, 0x40, 0xc2, 0x46, 0xfc, 0x27, 0x00, 0x20, 0x2a, 0x00, 0x1c, 0x22, 0x00]), // queue_recv
    (0x400959F2, [0x4f, 0xef, 0xff, 0xd8, 0x48, 0xd7, 0x1c, 0x7c, 0x24, 0x79, 0x40, 0x28, 0x78, 0x08, 0x42, 0x83]), // panel_diff
    (0x40098CA4, [0x4e, 0xb9, 0x40, 0x00, 0x0f, 0x36, 0x50, 0x8f, 0x20, 0x3c, 0x41, 0x93, 0xbf, 0xdc, 0x2f, 0x40]), // pend_call
    (0x40099450, [0x12, 0xda, 0x12, 0xda, 0x53, 0x82, 0x66, 0x00, 0xff, 0xfa, 0x60, 0x00, 0xfe, 0xc4, 0x20, 0x09]), // depack_copy
    (0x4009F306, [0x60, 0xfe, 0x70, 0xff, 0x4e, 0x75, 0x70, 0x0c, 0x23, 0xc0, 0x42, 0xf9, 0x26, 0x38, 0x70, 0xff]), // abort_loop
    (0x40001078, [0x2f, 0x02, 0x40, 0xc2, 0x46, 0xfc, 0x27, 0x00, 0x2f, 0x2f, 0x00, 0x08, 0x4e, 0xba, 0xfc, 0x84]), // sem_post
    (0x400859AC, [0x42, 0x00, 0x4f, 0xef, 0xff, 0xe8, 0x48, 0xd7, 0x0c, 0x1c, 0x28, 0x2f, 0x00, 0x1c, 0x24, 0x2f]), // sysex_send
];

/// Stock OS 1.74's bytes at the 1.74 profile's code addresses (as `AR_173_CODE`).
const AR_174_CODE: &[(u32, [u8; 16])] = &[
    (0x40000870, [0x41, 0xef, 0x00, 0x04, 0x23, 0xd0, 0x40, 0x27, 0x28, 0x74, 0x2e, 0x7c, 0x48, 0x00, 0x00, 0x00]),
    (0x40136482, [0x4f, 0xef, 0xff, 0xf4, 0x48, 0xd7, 0x04, 0x0c, 0x24, 0x2f, 0x00, 0x10, 0x26, 0x2f, 0x00, 0x14]),
    (0x40000410, [0x46, 0xfc, 0x27, 0x00, 0x2f, 0x48, 0xff, 0xfc, 0x20, 0x79, 0x42, 0xf6, 0x79, 0x24, 0x48, 0xe8]),
    (0x40000DE4, [0x20, 0x2f, 0x00, 0x0c, 0x72, 0xfc, 0xc2, 0xaf, 0x00, 0x14, 0x20, 0x6f, 0x00, 0x04, 0xe5, 0x88]),
    (0x40000F36, [0x22, 0x6f, 0x00, 0x04, 0x40, 0xc0, 0x46, 0xfc, 0x27, 0x00, 0x4a, 0x91, 0x6f, 0x04, 0x42, 0x91]),
    (0x40000EC2, [0x22, 0x6f, 0x00, 0x04, 0x40, 0xc1, 0x46, 0xfc, 0x27, 0x00, 0x20, 0x11, 0x6f, 0x06, 0x53, 0x80]),
    (0x400013B2, [0x2f, 0x0a, 0x2f, 0x02, 0x20, 0x6f, 0x00, 0x0c, 0x40, 0xc2, 0x46, 0xfc, 0x27, 0x00, 0x45, 0xef]),
    (0x40001462, [0x58, 0x8f, 0x60, 0xf2, 0x40, 0xc2, 0x46, 0xfc, 0x27, 0x00, 0x20, 0x2a, 0x00, 0x1c, 0x22, 0x00]),
    (0x40095A5E, [0x4f, 0xef, 0xff, 0xd8, 0x48, 0xd7, 0x1c, 0x7c, 0x24, 0x79, 0x40, 0x28, 0x7b, 0x68, 0x42, 0x83]),
    (0x40098D10, [0x4e, 0xb9, 0x40, 0x00, 0x0f, 0x36, 0x50, 0x8f, 0x20, 0x3c, 0x41, 0x93, 0xbf, 0xdc, 0x2f, 0x40]),
    (0x400994BC, [0x12, 0xda, 0x12, 0xda, 0x53, 0x82, 0x66, 0x00, 0xff, 0xfa, 0x60, 0x00, 0xfe, 0xc4, 0x20, 0x09]),
    (0x4009F372, [0x60, 0xfe, 0x70, 0xff, 0x4e, 0x75, 0x70, 0x0c, 0x23, 0xc0, 0x42, 0xf9, 0x26, 0x38, 0x70, 0xff]),
    (0x40001078, [0x2f, 0x02, 0x40, 0xc2, 0x46, 0xfc, 0x27, 0x00, 0x2f, 0x2f, 0x00, 0x08, 0x4e, 0xba, 0xfc, 0x84]),
    (0x40085A18, [0x42, 0x00, 0x4f, 0xef, 0xff, 0xe8, 0x48, 0xd7, 0x0c, 0x1c, 0x28, 0x2f, 0x00, 0x1c, 0x24, 0x2f]),
];

/// Stock Analog Four MKII OS 1.56's bytes at the A4 profile's code addresses (as
/// `AR_173_CODE`): an elekloader build for the A4 keeps them and runs on that
/// profile, after an OS upgrade too (`reboot_from_flash`).
const A4_156_CODE: &[(u32, [u8; 16])] = &[
    (0x40001130, [0x41, 0xef, 0x00, 0x04, 0x23, 0xd0, 0x40, 0x1f, 0xf5, 0x4c, 0x2e, 0x7c, 0x48, 0x00, 0x00, 0x00]), // entry
    (0x401148AE, [0x4f, 0xef, 0xff, 0xf4, 0x48, 0xd7, 0x04, 0x0c, 0x24, 0x2f, 0x00, 0x10, 0x26, 0x2f, 0x00, 0x14]), // flash_read
    (0x40000410, [0x46, 0xfc, 0x27, 0x00, 0x2f, 0x48, 0xff, 0xfc, 0x20, 0x79, 0x42, 0x87, 0xc5, 0xa4, 0x48, 0xe8]), // ctx_switch
    (0x4000197C, [0x20, 0x2f, 0x00, 0x0c, 0x72, 0xfc, 0xc2, 0xaf, 0x00, 0x14, 0x20, 0x6f, 0x00, 0x04, 0xe5, 0x88]), // task_create
    (0x40001ACE, [0x22, 0x6f, 0x00, 0x04, 0x40, 0xc0, 0x46, 0xfc, 0x27, 0x00, 0x4a, 0x91, 0x6f, 0x04, 0x42, 0x91]), // sem_pend
    (0x40001A5A, [0x22, 0x6f, 0x00, 0x04, 0x40, 0xc1, 0x46, 0xfc, 0x27, 0x00, 0x20, 0x11, 0x6f, 0x06, 0x53, 0x80]), // pend_b
    (0x40001F4A, [0x2f, 0x0a, 0x2f, 0x02, 0x20, 0x6f, 0x00, 0x0c, 0x40, 0xc2, 0x46, 0xfc, 0x27, 0x00, 0x45, 0xef]), // queue_send
    (0x40001FFA, [0x58, 0x8f, 0x60, 0xf2, 0x40, 0xc2, 0x46, 0xfc, 0x27, 0x00, 0x20, 0x2a, 0x00, 0x1c, 0x22, 0x00]), // queue_recv
    (0x40075756, [0x4f, 0xef, 0xff, 0xd8, 0x48, 0xd7, 0x1c, 0x7c, 0x24, 0x79, 0x40, 0x21, 0x43, 0x10, 0x42, 0x83]), // panel_diff
    (0x400780F0, [0x4e, 0xb9, 0x40, 0x00, 0x1a, 0xce, 0x50, 0x8f, 0x20, 0x3c, 0x41, 0x82, 0xa6, 0xc8, 0x2f, 0x40]), // pend_call
    (0x4007889C, [0x12, 0xda, 0x12, 0xda, 0x53, 0x82, 0x66, 0x00, 0xff, 0xfa, 0x60, 0x00, 0xfe, 0xc4, 0x20, 0x09]), // depack_copy
    (0x4007D9AA, [0x60, 0xfe, 0x70, 0xff, 0x4e, 0x75, 0x70, 0x0c, 0x23, 0xc0, 0x42, 0x8a, 0x7b, 0x10, 0x70, 0xff]), // abort_loop
    (0x40001C10, [0x2f, 0x02, 0x40, 0xc2, 0x46, 0xfc, 0x27, 0x00, 0x2f, 0x2f, 0x00, 0x08, 0x4e, 0xba, 0xfc, 0x84]), // sem_post
    (0x40003578, [0x4f, 0xef, 0xff, 0xe4, 0x72, 0x01, 0x48, 0xd7, 0x0c, 0x7c, 0x20, 0x2f, 0x00, 0x20, 0x22, 0x6f]), // usb_midi_send
    (0x400034CC, [0x22, 0x6f, 0x00, 0x08, 0x70, 0x01, 0xb0, 0xb9, 0x41, 0x33, 0xdb, 0x58, 0x66, 0x00, 0x00, 0x9c]), // usb_midi_send_short
    (0x40068BD8, [0x42, 0x00, 0x4f, 0xef, 0xff, 0xe8, 0x48, 0xd7, 0x0c, 0x1c, 0x28, 0x2f, 0x00, 0x1c, 0x24, 0x2f]), // sysex_send
];

impl Profile {
    pub fn for_image(img: &[u8]) -> Result<Profile, String> {
        use sha2::{Digest, Sha256};
        let h: String = Sha256::digest(img).iter().map(|b| format!("{b:02x}")).collect();
        let idle_spins = (0..img.len() - 1)
            .step_by(2)
            .filter(|&o| img[o] == 0x60 && img[o + 1] == 0xFE)
            .map(|o| MAIN_LOAD + o as u32)
            .collect();
        match h.as_str() {
            DT_153_SHA256 => Ok(Self::digitakt_153(h, idle_spins)),
            AR_173_SHA256 => Ok(Self::rytm_173(h, idle_spins)),
            AR_174_SHA256 => Ok(Self::rytm_174(h, idle_spins)),
            A4_156_SHA256 => Ok(Self::a4_156(h, idle_spins)),
            A4MK1_156_SHA256 => Ok(Self::a4mk1_156(h, idle_spins)),
            ARMK1_174_SHA256 => Ok(Self::rytm_mk1_174(h, idle_spins)),
            ST_142_SHA256 => Ok(Self::syntakt_142(h, idle_spins)),
            _ => {
                // A modified image (rytm-mods build): stock code at every
                // address the profile hooks.
                let at = |a: u32| (a - MAIN_LOAD) as usize;
                let differ = |code: &[(u32, [u8; 16])]| -> Vec<String> {
                    code.iter().filter(|(a, b)| img.get(at(*a)..at(*a) + 16) != Some(&b[..])).map(|(a, _)| format!("0x{a:08x}")).collect()
                };
                let (d173, d174) = (differ(AR_173_CODE), differ(AR_174_CODE));
                if d173.is_empty() {
                    let mut p = Self::rytm_173(h, idle_spins);
                    p.name = "Analog Rytm MKII OS 1.73 (modified)";
                    return Ok(p);
                }
                if d174.is_empty() {
                    let mut p = Self::rytm_174(h, idle_spins);
                    p.name = "Analog Rytm MKII OS 1.74 (modified)";
                    return Ok(p);
                }
                let da4 = differ(A4_156_CODE);
                if da4.is_empty() {
                    let mut p = Self::a4_156(h, idle_spins);
                    p.name = "Analog Four MKII OS 1.56 (modified)";
                    return Ok(p);
                }
                Err(format!(
                    "MAIN OS sha256 {h} has no symbol table here; known: Digitakt OS 1.53, Analog Rytm MKII OS 1.73 and 1.74, Analog Four MKII OS 1.56, and images that keep one of those Rytm or A4 OSes' code at the profile's addresses (vs Rytm 1.73 this one differs at {}; vs Rytm 1.74 at {}; vs A4 1.56 at {})",
                    d173.join(", "), d174.join(", "), da4.join(", ")
                ))
            }
        }
    }

    fn digitakt_153(sha256: String, idle_spins: Vec<u32>) -> Profile {
        Profile {
            name: "Digitakt OS 1.53",
            device: Device::Digitakt,
            sha256,
            entry: 0x4000_04E8,
            flash_read: 0x400E_94F2,
            ctx_switch: 0x4000_0410,
            current_tcb: 0x4399_D798,
            prio_heads: 0x4399_D758,
            task_create: 0x4000_15AC,
            sem_pend: 0x4000_16FE,
            pend_b: 0x4000_168A,
            queue_send: 0x4000_1B7A,
            queue_recv: 0x4000_1C2A,
            mainloop: 0x4000_B6E4,
            panel_diff: 0x400E_60E2,
            fb_front: 0x4020_D8F8,
            fb_back: 0x4020_D8FC,
            intro_pit3_isr: 0x4006_C154,
            intro_done: 0x4006_CB92,
            intro_park: 0x4006_CBC0,
            frame_sem: 0x4198_8BE4,
            display_sem: 0x421C_D06C,
            completion_sem: 0x421E_D5C8,
            pend_call: 0x400E_8AFC,
            depack_copy: 0x400E_AAAC,
            sd_flag: 0x421C_CA10,
            sd_status: 0x421C_CA40,
            sd_cmd_sem: 0x421C_CA5C,
            sd_data_sem: 0x421C_CA54,
            sd_dma_sem: 0x421C_CA4C,
            sd_capacity: 0x421C_CA34,
            uart8_ring_ptr: 0x4060_E820,
            uart8_consume_idx: 0x4060_E840,
            transport_state: 0x4199_DC2C,
            panel_button_names: 0x4018_E254,
            mounted: 0x420E_DC50,
            first_boot_error: 0x4064_80E0,
            progress_done: 0x421C_D4F4,
            progress_total: 0x421C_D4F0,
            abort_loop: 0x400E_E3F2,
            usb_midi_queue: 0,
            midi_event: 0,
            sem_post: 0,
            midi_pool: 0,
            midi_pool_map: 0,
            midi_pool_sem: 0,
            usb_midi_send: 0,
            usb_midi_send_short: 0,
            midi_short_ring: 0,
            midi_short_idx: 0,
            usb_audio_copy: 0,
            sysex_send: 0,
            voice_output_mix: 0,
            voice_frame_sent: 0,
            ob_in_buf: 0,
            ob_in_index: 0,
            boot_blob: (0, 0),
            boot_ver: 0,
            boot_ver_flash: 0,
            idle_spins,
        }
    }

    /// Matched from Digitakt OS 1.53 by tools/xmatch.py, 2026-10-06. Code: a
    /// unique masked match of the function's start. Data: unanimous votes of the
    /// references, with the SD driver's, display and progress variables keeping
    /// their relative offsets. Not found (0): the Digitakt's main loop and intro
    /// hooks, frame_sem, transport_state, first_boot_error; panel_button_names
    /// was rejected (one vote, odd address).
    fn rytm_173(sha256: String, idle_spins: Vec<u32>) -> Profile {
        Profile {
            name: "Analog Rytm MKII OS 1.73",
            device: Device::RytmMk2,
            sha256,
            entry: 0x4000_0870,
            flash_read: 0x4013_62B6,
            ctx_switch: 0x4000_0410,
            current_tcb: 0x42F6_7924,
            prio_heads: 0x42F6_78E4,
            task_create: 0x4000_0DE4,
            sem_pend: 0x4000_0F36,
            pend_b: 0x4000_0EC2,
            queue_send: 0x4000_13B2,
            queue_recv: 0x4000_1462,
            mainloop: 0,
            panel_diff: 0x4009_59F2,
            fb_front: 0x4028_7808,
            fb_back: 0x4028_780C,
            intro_pit3_isr: 0,
            intro_done: 0,
            intro_park: 0,
            frame_sem: 0,
            display_sem: 0x4191_B6E8,
            completion_sem: 0x4193_BFD4,
            pend_call: 0x4009_8CA4,
            depack_copy: 0x4009_9450,
            sd_flag: 0x4191_B08C,
            sd_status: 0x4191_B0BC,
            sd_cmd_sem: 0x4191_B0D8,
            sd_data_sem: 0x4191_B0D0,
            sd_dma_sem: 0x4191_B0C8,
            sd_capacity: 0x4191_B0B0,
            uart8_ring_ptr: 0x413D_A6B0,
            uart8_consume_idx: 0x413D_A6D0,
            transport_state: 0,
            panel_button_names: 0,
            mounted: 0x4184_3C70,
            first_boot_error: 0,
            progress_done: 0x4191_BB70,
            progress_total: 0x4191_BB6C,
            abort_loop: 0x4009_F306,
            // Traced by hand, 2026-10-06 (docs/RYTM-PORT.md, DIN MIDI).
            usb_midi_queue: 0x418F_D89C,
            midi_event: 0x418F_D874,
            sem_post: 0x4000_1078,
            midi_pool: 0x42F6_A024,
            midi_pool_map: 0x42F6_7B98,
            midi_pool_sem: 0x42F6_7B90,
            usb_midi_send: 0x4000_29E0,
            usb_midi_send_short: 0x4000_2934,
            midi_short_ring: 0x42F6_7C18,
            midi_short_idx: 0x4184_7E80,
            usb_audio_copy: 0x4010_E944,
            sysex_send: 0x4008_59AC,
            voice_output_mix: 0x4010_EA2C,
            voice_frame_sent: 0x4007_A598,
            ob_in_buf: 0x42AC_6F30,
            ob_in_index: 0x8000_FEF4,
            boot_blob: (0, 0),
            boot_ver: 0,
            boot_ver_flash: 0,
            idle_spins,
        }
    }

    /// OS 1.74, ported from the 1.73 profile by tools/xmatch.py, 2026-10-06:
    /// every code address a unique masked match, every data address unanimous;
    /// only the panel/flash/display functions after 0x40080000 moved (+0x6c)
    /// and the framebuffer pointers (+0x360).
    fn rytm_174(sha256: String, idle_spins: Vec<u32>) -> Profile {
        Profile {
            name: "Analog Rytm MKII OS 1.74",
            device: Device::RytmMk2,
            sha256,
            entry: 0x4000_0870,
            flash_read: 0x4013_6482,
            ctx_switch: 0x4000_0410,
            current_tcb: 0x42F6_7924,
            prio_heads: 0x42F6_78E4,
            task_create: 0x4000_0DE4,
            sem_pend: 0x4000_0F36,
            pend_b: 0x4000_0EC2,
            queue_send: 0x4000_13B2,
            queue_recv: 0x4000_1462,
            mainloop: 0,
            panel_diff: 0x4009_5A5E,
            fb_front: 0x4028_7B68,
            fb_back: 0x4028_7B6C,
            intro_pit3_isr: 0,
            intro_done: 0,
            intro_park: 0,
            frame_sem: 0,
            display_sem: 0x4191_B6E8,
            completion_sem: 0x4193_BFD4,
            pend_call: 0x4009_8D10,
            depack_copy: 0x4009_94BC,
            sd_flag: 0x4191_B08C,
            sd_status: 0x4191_B0BC,
            sd_cmd_sem: 0x4191_B0D8,
            sd_data_sem: 0x4191_B0D0,
            sd_dma_sem: 0x4191_B0C8,
            sd_capacity: 0x4191_B0B0,
            uart8_ring_ptr: 0x413D_A6B0,
            uart8_consume_idx: 0x413D_A6D0,
            transport_state: 0,
            panel_button_names: 0,
            mounted: 0x4184_3C70,
            first_boot_error: 0,
            progress_done: 0x4191_BB70,
            progress_total: 0x4191_BB6C,
            abort_loop: 0x4009_F372,
            // Traced by hand, 2026-10-06 (docs/RYTM-PORT.md, DIN MIDI).
            usb_midi_queue: 0x418F_D89C,
            midi_event: 0x418F_D874,
            sem_post: 0x4000_1078,
            midi_pool: 0x42F6_A024,
            midi_pool_map: 0x42F6_7B98,
            midi_pool_sem: 0x42F6_7B90,
            usb_midi_send: 0x4000_29E0,
            usb_midi_send_short: 0x4000_2934,
            midi_short_ring: 0x42F6_7C18,
            midi_short_idx: 0x4184_7E80,
            usb_audio_copy: 0x4010_E9B0,
            sysex_send: 0x4008_5A18,
            // 1.74: the code +0x6c, the data unchanged (same reference counts).
            voice_output_mix: 0x4010_EA98,
            voice_frame_sent: 0,
            ob_in_buf: 0x42AC_6F30,
            ob_in_index: 0x8000_FEF4,
            boot_blob: (0, 0),
            boot_ver: 0,
            boot_ver_flash: 0,
            idle_spins,
        }
    }

    /// Analog Four MKII OS 1.56, matched from Rytm 1.74 by tools/xmatch.py,
    /// 2026-10-07 (docs/A4-PORT.md): code by unique masked match, data by the
    /// references' votes. Weak (one or two votes): uart8_*, usb_midi_queue,
    /// midi_short_ring; midi_event [inferred, the Rytm's offset]. Not found (0):
    /// mounted, usb_audio_copy, panel_button_names and what the Rytm lacks too.
    fn a4_156(sha256: String, idle_spins: Vec<u32>) -> Profile {
        Profile {
            name: "Analog Four MKII OS 1.56",
            device: Device::A4Mk2,
            sha256,
            entry: 0x4000_1130,
            flash_read: 0x4011_48AE,
            ctx_switch: 0x4000_0410,
            current_tcb: 0x4287_C5A4,
            prio_heads: 0x4287_C564,
            task_create: 0x4000_197C,
            sem_pend: 0x4000_1ACE,
            pend_b: 0x4000_1A5A,
            queue_send: 0x4000_1F4A,
            queue_recv: 0x4000_1FFA,
            mainloop: 0,
            panel_diff: 0x4007_5756,
            fb_front: 0x4021_4310,
            fb_back: 0x4021_4314,
            intro_pit3_isr: 0,
            intro_done: 0,
            intro_park: 0,
            frame_sem: 0,
            display_sem: 0x4180_9CD0,
            completion_sem: 0x4182_A6C0,
            pend_call: 0x4007_80F0,
            depack_copy: 0x4007_889C,
            sd_flag: 0x4180_9674,
            sd_status: 0x4180_96A4,
            sd_cmd_sem: 0x4180_96C0,
            sd_data_sem: 0x4180_96B8,
            sd_dma_sem: 0x4180_96B0,
            sd_capacity: 0x4180_9698,
            uart8_ring_ptr: 0x4133_CFB4,
            uart8_consume_idx: 0x4133_CFD4,
            transport_state: 0,
            panel_button_names: 0,
            mounted: 0,
            first_boot_error: 0,
            progress_done: 0x4180_A158,
            progress_total: 0x4180_A154,
            abort_loop: 0x4007_D9AA,
            usb_midi_queue: 0x417E_B018,
            midi_event: 0x417E_AFF0,
            sem_post: 0x4000_1C10,
            midi_pool: 0x4287_ECA4,
            midi_pool_map: 0x4287_C818,
            midi_pool_sem: 0x4287_C810,
            usb_midi_send: 0x4000_3578,
            usb_midi_send_short: 0x4000_34CC,
            midi_short_ring: 0x4287_C898,
            midi_short_idx: 0x4173_57F8,
            usb_audio_copy: 0,
            sysex_send: 0x4006_8BD8,
            // Rytm-only (its digital voice layer and Overbridge input).
            voice_output_mix: 0,
            voice_frame_sent: 0,
            ob_in_buf: 0,
            ob_in_index: 0,
            boot_blob: (0, 0),
            boot_ver: 0,
            boot_ver_flash: 0,
            idle_spins,
        }
    }

    /// Analog Four MKI OS 1.56 (ELE2), matched from the A4 MKII OS 1.56 by
    /// tools/xmatch.py, 2026-10-12 (docs/MK1-PORT.md section 6): code by a
    /// unique masked match, data by the references' votes [measured]. Weak (one
    /// or two votes): fb_*, display_sem, sd_flag, sd_capacity, progress_done,
    /// uart8_*, midi_short_ring. panel_diff is read by hand (`0x4007333a`: the
    /// 4-page tile loop over fb_front / fb_back, then the swap) [measured].
    /// flash_read is none: the NOR flash is plain memory. Not found (0):
    /// mounted, usb_audio_copy and what the A4 MKII lacks too.
    fn a4mk1_156(sha256: String, idle_spins: Vec<u32>) -> Profile {
        Profile {
            name: "Analog Four MKI OS 1.56",
            device: Device::A4Mk1,
            sha256,
            entry: 0x4000_0E52,
            flash_read: 0,
            ctx_switch: 0x4000_0410,
            current_tcb: 0x425D_5BE4,
            prio_heads: 0x425D_5BA4,
            task_create: 0x4000_18E8,
            sem_pend: 0x4000_1A3A,
            pend_b: 0x4000_19C6,
            queue_send: 0x4000_1EB6,
            queue_recv: 0x4000_1F66,
            mainloop: 0,
            panel_diff: 0x4007_333A,
            fb_front: 0x401E_D954,
            fb_back: 0x401E_D958,
            intro_pit3_isr: 0,
            intro_done: 0,
            intro_park: 0,
            frame_sem: 0,
            display_sem: 0x4090_DD54,
            completion_sem: 0x4092_E120,
            pend_call: 0x4007_5C08,
            depack_copy: 0x4007_63B0,
            sd_flag: 0x4090_D6F8,
            sd_status: 0x4090_D728,
            sd_cmd_sem: 0x4090_D744,
            sd_data_sem: 0x4090_D73C,
            sd_dma_sem: 0x4090_D734,
            sd_capacity: 0x4090_D71C,
            uart8_ring_ptr: 0x4044_14D4,
            uart8_consume_idx: 0x4044_14F4,
            transport_state: 0,
            panel_button_names: 0,
            mounted: 0,
            first_boot_error: 0,
            progress_done: 0x4090_DFC4,
            progress_total: 0x4090_DFC0,
            abort_loop: 0x4007_B4CA,
            usb_midi_queue: 0x408E_F09C,
            midi_event: 0x408E_F074,
            sem_post: 0x4000_1B7C,
            midi_pool: 0x425D_8228,
            midi_pool_map: 0x425D_5D9C,
            midi_pool_sem: 0x425D_5D94,
            usb_midi_send: 0x4000_34BC,
            usb_midi_send_short: 0x4000_3410,
            midi_short_ring: 0x425D_5E1C,
            midi_short_idx: 0x4083_9CA0,
            usb_audio_copy: 0,
            sysex_send: 0x4006_6C68,
            voice_output_mix: 0,
            voice_frame_sent: 0,
            ob_in_buf: 0,
            ob_in_index: 0,
            // The entry compares flash 0xfff8 with the halfword at 0x40226ee8;
            // the bootstrap upgrade (0x40100b70) programs 0x40226ef0..0x40233fc0
            // at flash 0 and the four halfwords from 0x40226ee8 at 0xfff8
            // [measured].
            boot_blob: (0x4022_6EF0, 0x4023_3FC0),
            boot_ver: 0x4022_6EE8,
            boot_ver_flash: 0xFFF8,
            idle_spins,
        }
    }

    /// Analog Rytm MKI OS 1.74 (ELE2), matched from the Rytm MKII OS 1.74 by
    /// tools/xmatch.py, 2026-10-12 (docs/MK1-PORT.md section 6) [measured]; the
    /// display functions and framebuffer pointers from the A4 MKI's, which
    /// share their code (panel_diff `0x40093bb6`, fb 7 and 5 votes). Weak (one
    /// or two votes): display_sem, completion_sem, sd_flag, sd_capacity,
    /// progress_done, uart8_*, midi_*. The bootstrap record from the code: the entry compares flash 0x1fff8 with the halfword at
    /// 0x4028c768 (`0x0602`), and the bootstrap upgrade (0x4011a7a4) programs
    /// 0x4028c770..0x402a1b84 [measured].
    fn rytm_mk1_174(sha256: String, idle_spins: Vec<u32>) -> Profile {
        Profile {
            name: "Analog Rytm MKI OS 1.74",
            device: Device::RytmMk1,
            sha256,
            entry: 0x4000_0592,
            flash_read: 0,
            ctx_switch: 0x4000_0410,
            current_tcb: 0x4275_5268,
            prio_heads: 0x4275_5228,
            task_create: 0x4000_0C68,
            sem_pend: 0x4000_0DBA,
            pend_b: 0x4000_0D46,
            queue_send: 0x4000_1236,
            queue_recv: 0x4000_12E6,
            mainloop: 0,
            panel_diff: 0x4009_3BB6,
            fb_front: 0x4024_BBB4,
            fb_back: 0x4024_BBB8,
            intro_pit3_isr: 0,
            intro_done: 0,
            intro_park: 0,
            frame_sem: 0,
            display_sem: 0x40AE_8910,
            completion_sem: 0x40B0_8BC4,
            pend_call: 0x4009_6D28,
            depack_copy: 0x4009_74D0,
            sd_flag: 0x40AE_82B4,
            sd_status: 0x40AE_82E4,
            sd_cmd_sem: 0x40AE_8300,
            sd_data_sem: 0x40AE_82F8,
            sd_dma_sem: 0x40AE_82F0,
            sd_capacity: 0x40AE_82D8,
            uart8_ring_ptr: 0x405F_E9D8,
            uart8_consume_idx: 0x405F_E9F8,
            transport_state: 0,
            panel_button_names: 0,
            mounted: 0x40A1_3358,
            first_boot_error: 0,
            progress_done: 0x40AE_8B80,
            progress_total: 0x40AE_8B7C,
            abort_loop: 0x4009_D3AA,
            usb_midi_queue: 0x40AC_AAC4,
            midi_event: 0x40AC_AA9C,
            sem_post: 0x4000_0EFC,
            midi_pool: 0x4275_7894,
            midi_pool_map: 0x4275_5408,
            midi_pool_sem: 0x4275_5400,
            usb_midi_send: 0x4000_283C,
            usb_midi_send_short: 0x4000_2790,
            midi_short_ring: 0x4275_5488,
            midi_short_idx: 0x40A1_5568,
            usb_audio_copy: 0,
            sysex_send: 0x4008_3D44,
            voice_output_mix: 0,
            voice_frame_sent: 0,
            ob_in_buf: 0,
            ob_in_index: 0,
            boot_blob: (0x4028_C770, 0x402A_1B84),
            boot_ver: 0x4028_C768,
            boot_ver_flash: 0x1_FFF8,
            idle_spins,
        }
    }

    /// Syntakt OS 1.42 (CPU1's MAIN OS), matched from Digitakt OS 1.53 (and
    /// the Rytm MKII 1.73 for the MIDI fields) by tools/xmatch.py, 2026-10-13
    /// (docs/SYNTAKT-PORT.md section 5): code by unique masked match, data by
    /// the references' votes [measured]. Weak (one vote): usb_midi_queue,
    /// midi_short_ring; uart8_* weak.
    fn syntakt_142(sha256: String, idle_spins: Vec<u32>) -> Profile {
        Profile {
            name: "Syntakt OS 1.42",
            device: Device::Syntakt,
            sha256,
            entry: 0x4000_04E8,
            flash_read: 0x4011_6D7A,
            ctx_switch: 0x4000_0410,
            current_tcb: 0x454E_49A8,
            prio_heads: 0x454E_4968,
            task_create: 0x4000_1640,
            sem_pend: 0x4000_1792,
            pend_b: 0x4000_171E,
            queue_send: 0x4000_1C0E,
            queue_recv: 0x4000_1CBE,
            mainloop: 0,
            panel_diff: 0x4011_391A,
            fb_front: 0x402D_7110,
            fb_back: 0x402D_7114,
            intro_pit3_isr: 0,
            intro_done: 0,
            intro_park: 0,
            frame_sem: 0x43BB_F678,
            display_sem: 0x444B_AD58,
            completion_sem: 0x444D_B2B4,
            pend_call: 0x4011_6384,
            depack_copy: 0x4011_8334,
            sd_flag: 0x444B_A6FC,
            sd_status: 0x444B_A72C,
            sd_cmd_sem: 0x444B_A748,
            sd_data_sem: 0x444B_A740,
            sd_dma_sem: 0x444B_A738,
            sd_capacity: 0x444B_A720,
            uart8_ring_ptr: 0x405D_37C8,
            uart8_consume_idx: 0x405D_37E8,
            transport_state: 0x43BD_8424,
            panel_button_names: 0x401D_7400,
            mounted: 0,
            first_boot_error: 0x4065_33FC,
            progress_done: 0x444B_B1E0,
            progress_total: 0x444B_B1DC,
            abort_loop: 0x4011_AA42,
            usb_midi_queue: 0x4449_1554,
            // The MIDI task (0x40103e0a) pends on it; the Rytm's offset from
            // the queue (-0x28) [measured: the task's pend in a booted run].
            midi_event: 0x4449_152C,
            sem_post: 0x4000_18D4,
            midi_pool: 0x4551_46F8,
            midi_pool_map: 0x4551_226C,
            midi_pool_sem: 0x4551_2264,
            usb_midi_send: 0x4000_4A20,
            usb_midi_send_short: 0x4000_4974,
            midi_short_ring: 0x4551_22EC,
            midi_short_idx: 0x443D_B264,
            usb_audio_copy: 0,
            sysex_send: 0x4010_6976,
            voice_output_mix: 0,
            voice_frame_sent: 0,
            ob_in_buf: 0,
            ob_in_index: 0,
            boot_blob: (0, 0),
            boot_ver: 0,
            boot_ver_flash: 0,
            idle_spins,
        }
    }
}
