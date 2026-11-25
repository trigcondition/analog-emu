//! Inspect a snapshot: registers, TCDs, memory.
//!
//!     dtpeek SNAPSHOT ssi | tcd N | mem ADDR LEN | find HEX | leds | uart [FROM] | uart9 [FROM] | replay | fb
//!
//! The firmware is fw/Digitakt_OS1.53.syx, or the file named by DTEMU_FW.

use dtemu::firmware::Firmware;
use dtemu::machine::Machine;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 2 || a[0] == "-h" || a[0] == "--help" {
        println!("usage: dtpeek SNAPSHOT ssi | tcd N | mem ADDR LEN | find HEX | leds | uart [FROM] | uart9 [FROM] | replay | fb");
        return;
    }
    let fw = Firmware::from_file(std::path::Path::new(&std::env::var("DTEMU_FW").unwrap_or_else(|_| "fw/Digitakt_OS1.53.syx".into()))).unwrap();
    let mut m = Machine::new(&fw, 128).unwrap();
    dtemu::snapshot::load(&mut m, std::path::Path::new(&a[0])).unwrap();
    match a[1].as_str() {
        "ssi" => {
            // SSI1's registers as written (STX0 STX1 SRX0 SRX1 SCR SISR SIER STCR
            // SRCR STCCR SRCCR SFCSR ...), and the audio DMA channels.
            let s = &m.cpu.bus.io.ssi;
            for (i, r) in s.regs.iter().enumerate() {
                println!("SSI +0x{:02x} = {r:08x}", i * 4);
            }
            for n in [dtemu::io::SSI_RX_CHAN, dtemu::io::SSI_TX_CHAN] {
                println!("TCD{n}: {:x?}", m.cpu.bus.tcd(n));
            }
        }
        "tcd" => {
            let n: usize = a[2].parse().unwrap();
            let t = m.cpu.bus.tcd(n);
            println!("TCD{n}: {t:x?}");
            println!("erq={} int={}", m.cpu.bus.io.edma.erq >> n & 1, m.cpu.bus.io.edma.int >> n & 1);
        }
        "mem" => {
            let addr = u32::from_str_radix(a[2].trim_start_matches("0x"), 16).unwrap();
            let len: u32 = a[3].parse().unwrap();
            for row in (0..len).step_by(16) {
                let bytes: Vec<String> = (0..16.min(len - row)).map(|i| format!("{:02x}", m.cpu.bus.peek8(addr + row + i))).collect();
                println!("{:08x}: {}", addr + row, bytes.join(" "));
            }
        }
        "save" => {
            // save ADDR LEN FILE, the bytes as the bus reads them, raw
            let addr = u32::from_str_radix(a[2].trim_start_matches("0x"), 16).unwrap();
            let len: u32 = a[3].parse().unwrap();
            let v: Vec<u8> = (0..len).map(|i| m.cpu.bus.peek8(addr + i)).collect();
            std::fs::write(&a[4], v).unwrap();
        }
        "find" => {
            // Every place in DDR and SRAM holding these bytes (at most 32).
            let pat: Vec<u8> = (0..a[2].len()).step_by(2).map(|i| u8::from_str_radix(&a[2][i..i + 2], 16).unwrap()).collect();
            let bus = &m.cpu.bus;
            let regions = [(dtemu::bus::DDR_BASE, &bus.ddr[..]), (dtemu::bus::SRAM_BASE, &bus.sram[..])];
            let mut n = 0;
            for (base, mem) in regions {
                for (i, w) in mem.windows(pat.len()).enumerate() {
                    if w == &pat[..] && n < 32 {
                        println!("{:08x}", base + i as u32);
                        n += 1;
                    }
                }
            }
            println!("{n} match(es)");
        }
        "leds" => {
            println!("led rows: {}", m.cpu.bus.io.panel.led_rows.iter().enumerate().filter_map(|(g, v)| v.map(|v| format!("a{g:x}={v:02x}"))).collect::<Vec<_>>().join(" "));
            let p = &m.cpu.bus.io.panel;
            println!("selectors: {:02x?}", p.selectors);
            println!("palette: {:?}", p.palette.iter().enumerate().filter(|x| x.1.is_some()).map(|(i, c)| (i, c.unwrap())).collect::<Vec<_>>());
            for led in 0..dtemu::panel::LEDS {
                println!("led {led}: slots {:02x?} -> {:?}", p.slots[led], p.led_rgb(led));
            }
            println!("oled frames {} unknown {}", p.oled_frames, p.unknown);
            for u in &p.unknown_log {
                println!("unknown {:02x?}", u);
            }
        }
        "uart9" => {
            let tx = &m.cpu.bus.io.uart9.tx;
            println!("{} bytes", tx.len());
            let from: usize = a.get(2).map(|x| x.parse().unwrap()).unwrap_or(0);
            let s: Vec<String> = tx.iter().skip(from).take(400).map(|b| format!("{b:02x}")).collect();
            println!("{}", s.join(" "));
        }
        "uart" => {
            let tx = &m.cpu.bus.io.uart8.tx;
            println!("{} bytes", tx.len());
            let from: usize = a.get(2).map(|x| x.parse().unwrap()).unwrap_or(0);
            for row in tx[from.min(tx.len())..].chunks(32).take(40) {
                println!("{}", row.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
            }
        }
        "replay" => {
            let mut p = dtemu::panel::PanelMcu::default();
            let mut r = Vec::new();
            let tx = &m.cpu.bus.io.uart8.tx;
            let mut i = 0;
            let mut sels = 0;
            while i < tx.len() {
                let h = tx[i];
                let n = match h >> 4 { 0 => 1, 1 => 10, 0xB => match h { 0xB0..=0xB3 | 0xB6 => 3, 0xB4 => 6, 0xB5 => 5, 0xB7 => 2, 0xB8 => 1, _ => 2 }, _ => 2 };
                if h >> 4 == 2 && sels < 40 {
                    println!("@{i}: {:02x?}", &tx[i..(i + n).min(tx.len())]);
                    sels += 1;
                }
                i += n;
            }
            for &b in tx.iter() {
                p.from_host(b, &mut r);
            }
            println!("selectors {:02x?} unknown {} frames {}", p.selectors, p.unknown, p.oled_frames);
            println!("live selectors {:02x?}", m.cpu.bus.io.panel.selectors);
        }
        "fb" => {
            if let Some(f) = &m.frame {
                print!("{}", Machine::frame_ascii(f));
            }
        }
        _ => eprintln!("unknown command"),
    }
}
