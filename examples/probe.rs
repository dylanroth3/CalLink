//! Hardware probe: send one hand-written virtual packet and print every reply.
//! Usage: cargo run --release --example probe -- <vtype hex> <payload hex> [--list-first]
use std::time::Duration;

use callink::dusb::Dusb;
use callink::usb::UsbTransport;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let vtype = u16::from_str_radix(&args[1], 16)?;
    let hex: String = args[2].chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let payload: Vec<u8> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
    let mut d = Dusb::new(UsbTransport::open_first()?);
    d.verbose = true;
    d.connect_or_reset()?;
    if args.iter().any(|a| a == "--list-first") {
        d.list_dir()?;
        if let Ok(ms) = std::env::var("PROBE_SLEEP_MS") {
            std::thread::sleep(Duration::from_millis(ms.parse()?));
        }
        if std::env::var_os("PROBE_REPING").is_some() {
            d.connect()?;
        }
    }
    d.set_timeout(Duration::from_secs(4));
    d.send_vpkt(vtype, &payload)?;
    for _ in 0..4 {
        match d.recv_vpkt() {
            Ok(_) => {}
            Err(e) => {
                println!("stop: {e:#}");
                break;
            }
        }
    }
    Ok(())
}
