//! Read raw parameters by id, or set one with id=HEXBYTES:
//! cargo run --release --example params -- 0x28 0x28=81 0x3d
use callink::dusb::Dusb;
use callink::usb::UsbTransport;

fn main() -> anyhow::Result<()> {
    let mut d = Dusb::new(UsbTransport::open_first()?);
    d.connect_or_reset()?;
    for a in std::env::args().skip(1) {
        if let Some((id, val)) = a.split_once('=') {
            let id = u16::from_str_radix(id.trim_start_matches("0x"), 16)?;
            let bytes: Vec<u8> = (0..val.len()).step_by(2).map(|i| u8::from_str_radix(&val[i..i + 2], 16)).collect::<Result<_, _>>()?;
            d.set_param(id, &bytes)?;
            println!("{id:#06x} := {bytes:02X?}");
            continue;
        }
        let id = u16::from_str_radix(a.trim_start_matches("0x"), 16)?;
        match d.params(&[id])?.into_iter().next().and_then(|x| x.1) {
            Some(v) => println!("{id:#06x}: {v:02X?}"),
            None => println!("{id:#06x}: unavailable"),
        }
    }
    Ok(())
}
