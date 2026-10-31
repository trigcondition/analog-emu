//! The MCF54418's USB OTG controller in device mode (0xFC0B_0000), as far as the
//! Rytm's firmware needs it to work without a USB host attached (and, later, with
//! one simulated). ChipIdea-style register set (MCF5441x RM, USB OTG):
//!   0x140 USBCMD   bit 1 RST clears itself
//!   0x144 USBSTS   write 1 to clear
//!   0x1AC EPSETUPSR, 0x1BC EPCOMPLETE   write 1 to clear
//!   0x1B0 EPPRIME  priming sets the endpoints' EPSR bits; reads back 0
//!   0x1B4 EPFLUSH  flushing clears them; reads back 0 (the firmware waits for
//!                  that: 0x40002640, switching USB CONFIG)
//!   0x1B8 EPSR     primed endpoints
//!   0x178 EPNAK    write 1 to clear (the host sets a bit by polling an IN
//!                  endpoint with nothing primed: usbhost.rs, MIDI IN)
//! Register values live in the bus's plain storage (and so in snapshots); this
//! only adds what the hardware does by itself.

pub const BASE: u32 = 0xFC0B_0000;
const USBCMD: u32 = 0x140;
const USBSTS: u32 = 0x144;
const EPSETUPSR: u32 = 0x1AC;
const EPPRIME: u32 = 0x1B0;
const EPFLUSH: u32 = 0x1B4;
const EPSR: u32 = 0x1B8;
const EPCOMPLETE: u32 = 0x1BC;
const OTGSC: u32 = 0x1A4;
const EPNAK: u32 = 0x178;

pub fn is_usb(a: u32) -> bool {
    a & 0xFFFF_C000 == BASE
}

/// A read the model answers itself (32-bit accesses only), or None.
pub fn read(off: u32, size: u32, plain: impl Fn(u32) -> u32) -> Option<u32> {
    if size != 4 {
        return None;
    }
    match off {
        EPPRIME | EPFLUSH => Some(0),
        USBCMD => Some(plain(USBCMD) & !2),
        _ => None,
    }
}

/// A write: Some((offset, value)) pairs to store instead of the plain write,
/// or None for a plain write.
pub fn write(off: u32, size: u32, v: u32, plain: impl Fn(u32) -> u32) -> Option<Vec<(u32, u32)>> {
    if size != 4 {
        return None;
    }
    match off {
        USBSTS | EPSETUPSR | EPCOMPLETE | EPNAK => Some(vec![(off, plain(off) & !v)]),
        // OTGSC: status bits 16..22 clear when written with 1; the line states
        // in bits 8..14 (ID, AVV, ASV, BSV, BSE, 1MST, DPS) are read-only, so a
        // host's VBUS survives the firmware's setup write (OS 1.74 writes
        // 0x08000000, 0x40004ea4); the rest is plain.
        OTGSC => {
            let keep = plain(OTGSC);
            Some(vec![(OTGSC, (v & !0x007F_7F00) | (keep & 0x0000_7F00) | (keep & 0x007F_0000 & !v))])
        }
        EPFLUSH => {
            if std::env::var_os("DTEMU_USB_LOG").is_some() {
                eprintln!("usb dev: flush {v:08x} (EPSR was {:08x})", plain(EPSR));
            }
            Some(vec![(EPFLUSH, 0), (EPSR, plain(EPSR) & !v)])
        }
        EPPRIME => {
            if v & !0x0001_0001 != 0 && std::env::var_os("DTEMU_USB_LOG").is_some() {
                eprintln!("usb dev: prime {v:08x} (EPSR was {:08x})", plain(EPSR));
            }
            Some(vec![(EPPRIME, 0), (EPSR, plain(EPSR) | v)])
        }
        USBCMD => Some(vec![(USBCMD, v & !2)]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The firmware's setup write must not take the host's VBUS away (the
    /// Digitakt's and the Rytm's USB stacks start from BSV on a cold boot).
    #[test]
    fn otgsc_line_state_is_read_only() {
        let bsv_and_bsvis = (1 << 11) | (1 << 19);
        let w = write(OTGSC, 4, 0x0800_0000, |_| bsv_and_bsvis).unwrap();
        assert_eq!(w, vec![(OTGSC, 0x0800_0000 | bsv_and_bsvis)]);
        // Writing 1 clears a status bit; the line state stays.
        let w = write(OTGSC, 4, 1 << 19, |_| bsv_and_bsvis).unwrap();
        assert_eq!(w, vec![(OTGSC, 1 << 11)]);
    }
}
