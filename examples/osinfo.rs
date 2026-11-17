//! Describe an OS file as CalLink will send it: sections, flash pages and the packet plan.
//! Usage: cargo run --release --example osinfo -- FILE.8xu
use callink::flash::{parse_sections, OsFile};

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: osinfo FILE");
    let bytes = std::fs::read(&path)?;
    for s in parse_sections(&bytes)? {
        println!(
            "section: name={:?} version={}.{:02} device={:#04x} type={:#04x} hw_id={:#04x} data={} bytes",
            s.name, s.version.0, s.version.1, s.device_type, s.data_type, s.hw_id, s.data.len()
        );
    }
    let os = OsFile::parse(&bytes)?;
    println!("OS {} ({})", os.version_string(), if os.is_ce() { "CE, raw" } else { "TI-84 Plus, Intel HEX" });
    let mut packets = 0;
    for (i, p) in os.pages.iter().enumerate() {
        let n = if i == 0 || i + 1 == os.pages.len() { 1 } else { p.data.len().div_ceil(256) };
        packets += n;
        println!("  page[{i:2}] flag={:#04x} page={:#04x} addr={:#06x} size={:5}  → {n} packet(s)", p.flag, p.page, p.addr, p.data.len());
    }
    println!("{} pages, {packets} OS_DATA packets", os.pages.len());
    Ok(())
}
