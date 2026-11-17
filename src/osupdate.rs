//! Looking for and installing calculator OS updates.
//!
//! *Looking:* TI publishes each OS on a page at education.ti.com whose HTML links the OS
//! file directly (e.g. `TI-84_Plus_CE-OS-5.8.5.0074.8eu`, `TI84Plus_OS255.8Xu`); the version
//! is read from that file name.
//!
//! *Installing:* the DUSB OS transfer, following the sequence of libticalcs
//! (`send_os` for the TI-84 Plus, `send_os_834pce` for the CE), including its quirks:
//! basic-mode ping `2,1,0,0,0x0FA0`; OS_BEGIN carrying a size; packet size taken from each
//! OS_ACK; on the 84 Plus the first (header) and last (signature) blocks are sent once at
//! 0x4000/0x4100 on page 0x7A, the rest in 256-byte steps; on the CE, OS_DATA carries a
//! little-endian address from 0x30000 and a final (possibly empty) remainder packet;
//! EOT, a 500 ms pause, then EOT_ACK.

use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};

use crate::dusb::{self, Dusb, Transport};
use crate::flash::OsFile;
use crate::model::{Model, OsKind};

/// TI's page for the latest OS of this calculator (by product name), when known.
pub fn os_page(product_name: &str) -> Option<&'static str> {
    Model::identify(None, product_name).os_page(product_name)
}

pub const DOWNLOADS_PAGE: &str = "https://education.ti.com/en/downloads";

/// Numeric components of a version string ("2.55MP" → [2,55], "5.8.5.0074" → [5,8,5,74]).
pub fn version_parts(s: &str) -> Vec<u32> {
    s.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).filter_map(|p| p.parse().ok()).collect()
}

/// Compare the first three components, missing ones counting as 0.
pub fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (a, b) = (version_parts(a), version_parts(b));
    let get = |v: &Vec<u32>, i: usize| v.get(i).copied().unwrap_or(0);
    (0..3).map(|i| get(&a, i).cmp(&get(&b, i))).find(|o| o.is_ne()).unwrap_or(std::cmp::Ordering::Equal)
}

/// Version encoded in a TI OS file name.
pub fn version_from_file_name(name: &str) -> Option<String> {
    // "…-OS-5.8.5.0074.8eu": a dotted run of digits
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    let mut best: Option<String> = None;
    for part in stem.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        let part = part.trim_matches('.');
        if part.matches('.').count() >= 1 && version_parts(part).len() >= 2 {
            best = Some(part.to_string());
        }
    }
    if best.is_some() {
        return best;
    }
    // "TI84Plus_OS255": OS followed by three digits = d.dd
    let up = stem.to_ascii_uppercase();
    let i = up.rfind("OS")?;
    let digits: String = up[i + 2..].chars().take_while(|c| c.is_ascii_digit()).collect();
    (digits.len() == 3).then(|| format!("{}.{}", &digits[..1], &digits[1..]))
}

#[derive(Clone, Debug)]
pub struct LatestOs {
    pub version: String,
    pub file_name: String,
    pub file_url: String,
    pub page_url: String,
}

/// Find the OS file link and its version in a TI software page.
pub fn parse_os_page(html: &str, page_url: &str) -> Option<LatestOs> {
    for chunk in html.split("href=\"").skip(1) {
        let href = chunk.split('"').next()?;
        let lower = href.to_ascii_lowercase();
        if !(lower.ends_with(".8eu") || lower.ends_with(".8xu")) {
            continue;
        }
        let file_name = href.rsplit('/').next()?.to_string();
        let version = version_from_file_name(&file_name)?;
        let file_url = if href.starts_with("http") { href.to_string() } else { format!("https://education.ti.com{href}") };
        return Some(LatestOs { version, file_name, file_url, page_url: page_url.to_string() });
    }
    None
}

/// Ask TI's website for the newest OS for this model.
pub fn check_latest(product_name: &str) -> Result<LatestOs> {
    let page = os_page(product_name).with_context(|| format!("no known TI update page for {product_name}; see {DOWNLOADS_PAGE}"))?;
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(15)).user_agent("CalLink/0.1").build();
    let html = agent.get(page).call().context("contacting education.ti.com")?.into_string()?;
    parse_os_page(&html, page).context("TI's page didn't contain an OS download link (the page layout may have changed)")
}

/// Refuse OS files that aren't for the connected calculator. The OS header names the model
/// it is signed for (field 80 1x); it must match the calculator's product ID.
pub fn check_compatible(os: &OsFile, model: Model) -> Result<()> {
    let kind = model
        .os_kind()
        .with_context(|| format!("CalLink doesn't know how to install an OS on this calculator ({})", model.name()))?;
    match (kind, os.is_ce()) {
        (OsKind::Z80, true) => bail!("this is a CE OS (flat image); the connected calculator is a {}", model.name()),
        (OsKind::Ce, false) => bail!("this OS is for a Z80 model (TI-84 Plus/C SE/T, 82 Advanced); the connected calculator is a {}", model.name()),
        _ => {}
    }
    let file_id = os.header_product_id().context("can't tell which calculator this OS is for (no model ID in its header)")?;
    let want = model.product_id().context("unknown calculator model")?;
    if file_id != want {
        let file_model = Model::identify(Some(file_id), "");
        bail!(
            "this OS is for a {} (model ID {file_id:#04x}); the connected calculator is a {} ({want:#04x}). Use a .{} file for it.",
            file_model.name(),
            model.name(),
            model.os_extension()
        );
    }
    Ok(())
}

fn os_ack<T: Transport>(d: &mut Dusb<T>) -> Result<usize> {
    let p = d.expect(dusb::VPKT_OS_ACK)?;
    ensure!(p.len() >= 4, "short OS acknowledgement");
    let n = u32::from_be_bytes([p[0], p[1], p[2], p[3]]) as usize;
    ensure!((8..=4096).contains(&n), "calculator asked for an unexpected OS packet size ({n})");
    Ok(n)
}

fn ping_basic<T: Transport>(d: &mut Dusb<T>) -> Result<()> {
    d.send_vpkt(dusb::VPKT_PING, &[0x00, 0x02, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x0F, 0xA0])?;
    d.expect(dusb::VPKT_MODE_SET)?;
    Ok(())
}

fn os_begin<T: Transport>(d: &mut Dusb<T>, size: u32) -> Result<()> {
    let mut b = vec![0u8; 7];
    b.extend_from_slice(&size.to_be_bytes());
    d.send_vpkt(dusb::VPKT_OS_BEGIN, &b)
}

fn finish<T: Transport>(d: &mut Dusb<T>) -> Result<()> {
    d.send_vpkt(dusb::VPKT_EOT, &[])?;
    std::thread::sleep(Duration::from_millis(500));
    d.expect(dusb::VPKT_EOT_ACK)?;
    Ok(())
}

/// Send an OS. `progress(fraction, message)` is called as it goes. The calculator
/// reboots afterwards, so the connection must be reopened.
pub fn install<T: Transport>(d: &mut Dusb<T>, os: &OsFile, progress: &mut dyn FnMut(f32, String)) -> Result<()> {
    // Flash erases can take a while before the calculator acknowledges.
    d.set_timeout(Duration::from_secs(60));
    let r = if os.is_ce() { install_ce(d, os, progress) } else { install_84p(d, os, progress) };
    d.set_timeout(Duration::from_secs(10));
    r
}

fn install_84p<T: Transport>(d: &mut Dusb<T>, os: &OsFile, progress: &mut dyn FnMut(f32, String)) -> Result<()> {
    let pages = &os.pages;
    let last = pages.len() - 1;
    let os_size: usize =
        pages.iter().map(|p| if p.data.len() < 256 { 4 } else { 4 * (p.data.len() / 260) }).sum::<usize>() + os.section.data.len();
    let total: usize = 2 + pages[1..last].iter().map(|p| p.data.len().div_ceil(256)).sum::<usize>();

    progress(0.0, "Preparing calculator…".into());
    ping_basic(d)?;
    let _ = d.params(&[dusb::PID_OS_MODE]); // as the reference does; the answer isn't needed
    os_begin(d, os_size as u32)?;
    let mut pkt = os_ack(d)?;

    let mut b = vec![0x40, 0x00, 0x7A, 0x80];
    b.extend(pages[0].padded(0, pkt - 4));
    d.send_vpkt(dusb::VPKT_OS_HEADER, &b)?;
    pkt = os_ack(d)?;

    let mut sent = 0usize;
    let mut data = |d: &mut Dusb<T>, addr: u16, page: u8, flag: u8, bytes: Vec<u8>| -> Result<()> {
        let mut b = addr.to_be_bytes().to_vec();
        b.push(page);
        b.push(flag);
        b.extend(bytes);
        d.send_vpkt(dusb::VPKT_OS_DATA, &b)?;
        d.expect(dusb::VPKT_DATA_ACK)?;
        sent += 1;
        progress(sent as f32 / total as f32, format!("Installing OS… {}%", sent * 100 / total));
        Ok(())
    };
    for (i, p) in pages.iter().enumerate() {
        if i == 0 {
            data(d, 0x4000, 0x7A, 0x80, p.padded(0, pkt - 4))?;
        } else if i == last {
            data(d, 0x4100, 0x7A, 0x80, p.padded(0, pkt - 4))?;
        } else {
            for j in (0..p.data.len()).step_by(256) {
                data(d, 0x4000u16.wrapping_add(j as u16), p.page as u8, p.flag, p.padded(j, pkt - 4))?;
            }
        }
    }
    progress(1.0, "Finishing…".into());
    finish(d)
}

fn install_ce<T: Transport>(d: &mut Dusb<T>, os: &OsFile, progress: &mut dyn FnMut(f32, String)) -> Result<()> {
    let data = &os.section.data;
    let hdr = data
        .windows(4)
        .take(64 * 1024)
        .position(|w| w == [0xFF; 4])
        .context("couldn't find the end of the OS header in this file")?;

    progress(0.0, "Preparing calculator…".into());
    ping_basic(d)?;
    os_begin(d, data.len() as u32)?;
    let _ = os_ack(d)?;
    d.send_vpkt(dusb::VPKT_OS_HEADER, &data[..hdr])?;
    let pkt = os_ack(d)?;

    let chunk = pkt - 4;
    let q = data.len() / chunk;
    let total = q + 1;
    for i in 0..=q {
        let start = i * chunk;
        let end = (start + chunk).min(data.len()); // the last one may be short or even empty
        let mut b = ((0x30000 + start) as u32).to_le_bytes().to_vec();
        b.extend_from_slice(&data[start..end]);
        d.send_vpkt(dusb::VPKT_OS_DATA, &b)?;
        d.expect(dusb::VPKT_DATA_ACK)?;
        progress((i + 1) as f32 / total as f32, format!("Installing OS… {}%", (i + 1) * 100 / total));
    }
    progress(1.0, "Finishing…".into());
    finish(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dusb::mock::MockCalc;
    use crate::flash::tests::{fake_84p_os, fake_ce_os};

    #[test]
    fn versions() {
        assert_eq!(version_from_file_name("TI-84_Plus_CE-OS-5.8.5.0074.8eu").as_deref(), Some("5.8.5.0074"));
        assert_eq!(version_from_file_name("TI84Plus_OS255.8Xu").as_deref(), Some("2.55"));
        assert_eq!(version_cmp("2.55MP", "2.55"), std::cmp::Ordering::Equal);
        assert_eq!(version_cmp("5.8.2.0033", "5.8.5.0074"), std::cmp::Ordering::Less);
        assert_eq!(version_cmp("5.10", "5.8.5"), std::cmp::Ordering::Greater);
    }

    #[test]
    fn page_parsing() {
        let html = r#"<a class="x" href="/download/en/ed-tech/158B/4D13/TI-84_Plus_CE-OS-5.8.5.0074.8eu">Download</a>"#;
        let l = parse_os_page(html, "p").unwrap();
        assert_eq!(l.version, "5.8.5.0074");
        assert!(l.file_url.starts_with("https://education.ti.com/download/"));
    }

    /// Hits education.ti.com: `cargo test -- --ignored live_ti_pages`
    #[test]
    #[ignore]
    fn live_ti_pages() {
        for model in ["TI-84 Plus", "TI-84 Plus CE"] {
            let l = check_latest(model).unwrap();
            println!("{model}: {} {} {}", l.version, l.file_name, l.file_url);
            assert!(version_parts(&l.version).len() >= 2);
        }
    }

    #[test]
    fn compatibility() {
        let ce = OsFile::parse(&fake_ce_os()).unwrap();
        let z80 = OsFile::parse(&fake_84p_os()).unwrap();
        assert!(check_compatible(&ce, Model::Ti84PlusCE).is_ok());
        assert!(check_compatible(&ce, Model::Ti83PremiumCE).is_ok()); // same product ID; TI's own signature check decides
        assert!(check_compatible(&ce, Model::Ti84Plus).is_err());
        assert!(check_compatible(&z80, Model::Ti84Plus).is_ok());
        // a TI-84 Plus OS must not go onto a C SE or an 84 Plus T, though all are Intel-HEX files
        assert!(check_compatible(&z80, Model::Ti84PlusCSE).unwrap_err().to_string().contains(".8cu"));
        assert!(check_compatible(&z80, Model::Ti84PlusT).is_err());
        assert!(check_compatible(&z80, Model::Unknown(0x42)).is_err());
        assert_eq!(os_page("TI-84 Plus C Silver Edition").map(|p| p.contains("5F0CBAC1")), Some(true));
        assert!(os_page("TI-82 Advanced").is_none());
    }

    #[test]
    fn install_84p_on_mock() {
        let os = OsFile::parse(&fake_84p_os()).unwrap();
        let mut d = Dusb::new(MockCalc::new(250));
        d.connect().unwrap();
        let mut last = 0.0;
        install(&mut d, &os, &mut |f, _| last = f).unwrap();
        assert_eq!(last, 1.0);
        let m = d.into_inner();
        let r = m.os.as_ref().unwrap();
        assert!(r.done);
        // header = first 256 bytes of block 0, padded
        assert_eq!(r.header[..4], [0x40, 0x00, 0x7A, 0x80]);
        assert_eq!(r.header.len(), 4 + 256);
        // packets: 1 (header block) + 3 + 3 (two 600-byte pages in 256-byte steps) + 1 (signature)
        assert_eq!(r.data.len(), 8);
        assert_eq!(r.data[7][..4], [0x41, 0x00, 0x7A, 0x80]);
        assert_eq!(r.data[1][..4], [0x40, 0x00, 0x00, 0x00]);
        assert_eq!(r.data[2][..2], [0x41, 0x00]);
    }

    #[test]
    fn install_ce_on_mock() {
        let os = OsFile::parse(&fake_ce_os()).unwrap();
        let mut d = Dusb::new(MockCalc::new(1023));
        d.connect().unwrap();
        install(&mut d, &os, &mut |_, _| {}).unwrap();
        let m = d.into_inner();
        let r = m.os.as_ref().unwrap();
        assert!(r.done);
        assert_eq!(r.header.len(), 46); // up to the first FF FF FF FF
        // reassemble: little-endian addresses from 0x30000, contiguous
        let mut img = Vec::new();
        for p in &r.data {
            let addr = u32::from_le_bytes([p[0], p[1], p[2], p[3]]) as usize;
            assert_eq!(addr - 0x30000, img.len());
            img.extend_from_slice(&p[4..]);
        }
        assert_eq!(img, os.section.data);
    }
}
