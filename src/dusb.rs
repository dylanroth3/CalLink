//! DUSB: the USB protocol of the TI-84 Plus / TI-84 Plus C SE / TI-84 Plus CE family
//! (TI's NativeConnectServer.dll calls it "CARS").
//!
//! Two layers:
//!
//! **Raw packets** – `u32 BE payload_len | u8 type | payload`
//!   1 = buffer size request, 2 = buffer size allocation, 3 = virtual data (more follows),
//!   4 = virtual data (last fragment), 5 = virtual data ACK (`E0 00`).
//!   Every type 3/4 packet is acknowledged with a type 5 packet by the receiver.
//!
//! **Virtual packets** – `u32 BE data_len | u16 BE vtype | data`, split over as many raw
//! packets as needed (each raw payload ≤ the negotiated buffer size).
//!
//! All multi-byte fields in this layer are big-endian.

use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Context, Result};

// Raw packet types
pub const RPKT_BUF_SIZE_REQ: u8 = 1;
pub const RPKT_BUF_SIZE_ALLOC: u8 = 2;
pub const RPKT_VIRT_DATA: u8 = 3;
pub const RPKT_VIRT_DATA_LAST: u8 = 4;
pub const RPKT_VIRT_DATA_ACK: u8 = 5;

pub const DEFAULT_BUF_SIZE: u32 = 1023;

/// Pause after a transaction ends with EOT before starting the next one.
const SETTLE: Duration = Duration::from_millis(250);

// Virtual packet types
pub const VPKT_PING: u16 = 0x0001; // a.k.a. mode set
pub const VPKT_OS_BEGIN: u16 = 0x0002;
pub const VPKT_OS_ACK: u16 = 0x0003;
pub const VPKT_OS_HEADER: u16 = 0x0004;
pub const VPKT_OS_DATA: u16 = 0x0005;
pub const VPKT_EOT_ACK: u16 = 0x0006;
pub const VPKT_PARM_REQ: u16 = 0x0007;
pub const VPKT_PARM_DATA: u16 = 0x0008;
pub const VPKT_DIR_REQ: u16 = 0x0009;
pub const VPKT_VAR_HDR: u16 = 0x000A;
pub const VPKT_RTS: u16 = 0x000B;
pub const VPKT_VAR_REQ: u16 = 0x000C;
pub const VPKT_VAR_CNTS: u16 = 0x000D;
pub const VPKT_PARM_SET: u16 = 0x000E;
pub const VPKT_MODIF_VAR: u16 = 0x0010;
pub const VPKT_EXECUTE: u16 = 0x0011;
pub const VPKT_MODE_SET: u16 = 0x0012; // reply to PING
pub const VPKT_DATA_ACK: u16 = 0xAA00;
pub const VPKT_DELAY_ACK: u16 = 0xBB00;
pub const VPKT_EOT: u16 = 0xDD00;
pub const VPKT_ERROR: u16 = 0xEE00;

// Parameter IDs
pub const PID_PRODUCT_NUMBER: u16 = 0x0001;
pub const PID_PRODUCT_NAME: u16 = 0x0002;
pub const PID_MAIN_PART_ID: u16 = 0x0003;
pub const PID_HW_VERSION: u16 = 0x0004;
pub const PID_FULL_ID: u16 = 0x0005;
pub const PID_LANGUAGE_ID: u16 = 0x0006;
pub const PID_SUBLANG_ID: u16 = 0x0007;
pub const PID_DEVICE_TYPE: u16 = 0x0008;
pub const PID_BOOT_VERSION: u16 = 0x0009;
pub const PID_OS_MODE: u16 = 0x000A;
pub const PID_OS_VERSION: u16 = 0x000B;
pub const PID_PHYS_RAM: u16 = 0x000C;
pub const PID_USER_RAM: u16 = 0x000D;
pub const PID_FREE_RAM: u16 = 0x000E;
pub const PID_PHYS_FLASH: u16 = 0x000F;
pub const PID_USER_FLASH: u16 = 0x0010;
pub const PID_FREE_FLASH: u16 = 0x0011;
pub const PID_LCD_WIDTH: u16 = 0x001E;
pub const PID_LCD_HEIGHT: u16 = 0x001F;
pub const PID_SCREENSHOT: u16 = 0x0022;
pub const PID_CLK_ON: u16 = 0x0024;
pub const PID_CLK_SEC_SINCE_1997: u16 = 0x0025;
pub const PID_BATTERY: u16 = 0x002D;
// Separate clock fields (TI-84 Plus CE and newer)
pub const PID_CLK_TIME_FMT: u16 = 0x0028; // 0x80 = 12-hour, 0x81 = 24-hour (CE)
pub const PID_CLK_SECONDS: u16 = 0x003B;
pub const PID_CLK_MINUTES: u16 = 0x003C;
pub const PID_CLK_HOURS: u16 = 0x003D;
pub const PID_CLK_DAY: u16 = 0x003E;
pub const PID_CLK_MONTH: u16 = 0x003F;
pub const PID_CLK_YEAR: u16 = 0x0040;

// Attribute IDs
pub const AID_VAR_SIZE: u16 = 0x0001;
pub const AID_VAR_TYPE: u16 = 0x0002;
pub const AID_ARCHIVED: u16 = 0x0003;
pub const AID_VAR_VERSION: u16 = 0x0008;
pub const AID_VAR_TYPE2: u16 = 0x0011;

/// Something that moves bytes to and from the calculator (USB bulk endpoints, or a mock).
pub trait Transport {
    fn write(&mut self, data: &[u8]) -> Result<()>;
    /// Read whatever is available (at least one byte), blocking up to the transport's timeout.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;
    /// Reset the link (e.g. a USB port reset) after the calculator stopped responding.
    fn reset(&mut self) -> Result<()> {
        bail!("this transport cannot be reset")
    }
    fn set_timeout(&mut self, _t: Duration) {}
}

#[derive(Clone, Debug)]
pub struct Attr {
    pub id: u16,
    /// `None` when the calculator reported the attribute as unavailable.
    pub data: Option<Vec<u8>>,
}

impl Attr {
    pub fn new(id: u16, data: Vec<u8>) -> Attr {
        Attr { id, data: Some(data) }
    }
}

pub fn find_attr(attrs: &[Attr], id: u16) -> Option<&[u8]> {
    attrs.iter().find(|a| a.id == id).and_then(|a| a.data.as_deref())
}

/// A variable as reported by a directory listing.
#[derive(Clone, Debug)]
pub struct DirEntry {
    pub folder: Vec<u8>,
    pub name: Vec<u8>,
    pub attrs: Vec<Attr>,
}

impl DirEntry {
    pub fn var_type(&self) -> Option<u8> {
        find_attr(&self.attrs, AID_VAR_TYPE).and_then(|d| d.last().copied())
    }
    pub fn size(&self) -> Option<u32> {
        find_attr(&self.attrs, AID_VAR_SIZE).map(be_uint).map(|v| v as u32)
    }
    pub fn archived(&self) -> bool {
        find_attr(&self.attrs, AID_ARCHIVED).is_some_and(|d| d.iter().any(|&b| b != 0))
    }
}

pub fn be_uint(d: &[u8]) -> u64 {
    d.iter().take(8).fold(0u64, |a, &b| (a << 8) | b as u64)
}

fn be16(d: &[u8], at: usize) -> Result<u16> {
    let s = d.get(at..at + 2).context("packet truncated")?;
    Ok(u16::from_be_bytes([s[0], s[1]]))
}

fn be32(d: &[u8], at: usize) -> Result<u32> {
    let s = d.get(at..at + 4).context("packet truncated")?;
    Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// DUSB length-prefixed, NUL-terminated string.
fn put_str(out: &mut Vec<u8>, s: &[u8]) {
    out.push(s.len() as u8);
    if !s.is_empty() {
        out.extend_from_slice(s);
        out.push(0);
    }
}

fn get_str(d: &[u8], j: &mut usize) -> Result<Vec<u8>> {
    let n = *d.get(*j).context("packet truncated")? as usize;
    *j += 1;
    if n == 0 {
        return Ok(Vec::new());
    }
    let s = d.get(*j..*j + n).context("packet truncated")?.to_vec();
    *j += n + 1;
    Ok(s)
}

fn put_attrs(out: &mut Vec<u8>, attrs: &[Attr]) {
    out.extend_from_slice(&(attrs.len() as u16).to_be_bytes());
    for a in attrs {
        let d = a.data.as_deref().unwrap_or(&[]);
        out.extend_from_slice(&a.id.to_be_bytes());
        out.extend_from_slice(&(d.len() as u16).to_be_bytes());
        out.extend_from_slice(d);
    }
}

/// Parse a VAR_HDR packet: folder, name, then attributes with an "unavailable" flag byte.
pub fn parse_var_header(d: &[u8]) -> Result<DirEntry> {
    let mut j = 0;
    let folder = get_str(d, &mut j)?;
    let name = get_str(d, &mut j)?;
    let n = be16(d, j)?;
    j += 2;
    let mut attrs = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let id = be16(d, j)?;
        let unavailable = *d.get(j + 2).context("packet truncated")? != 0;
        j += 3;
        if unavailable {
            attrs.push(Attr { id, data: None });
        } else {
            let size = be16(d, j)? as usize;
            j += 2;
            let data = d.get(j..j + size).context("packet truncated")?.to_vec();
            j += size;
            attrs.push(Attr { id, data: Some(data) });
        }
    }
    Ok(DirEntry { folder, name, attrs })
}

/// The 4-byte type attribute value used by the 84 Plus family.
pub fn type_attr(prefix: u8, var_type: u8) -> Vec<u8> {
    vec![0xF0, prefix, 0x00, var_type]
}

#[derive(Debug)]
pub struct CalcError(pub u16);

impl std::fmt::Display for CalcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.0 {
            0x0004 => "invalid argument / name",
            0x0006 => "variable not found",
            0x0008 => "transmission error",
            0x0009 => "calculator is busy (not on the home screen?)",
            0x000C => "invalid data",
            0x000D => "out of memory / data too large",
            0x000E => "variable is locked or archived",
            0x0011 => "invalid parameter",
            0x0012 => "variable not found",
            0x001C => "out of memory",
            0x001D => "busy",
            _ => "calculator rejected the request",
        };
        write!(f, "{what} (DUSB error {:#06x})", self.0)
    }
}
impl std::error::Error for CalcError {}

pub struct Dusb<T: Transport> {
    t: T,
    rx: Vec<u8>,
    max_raw: usize,
    pub verbose: bool,
}

impl<T: Transport> Dusb<T> {
    pub fn new(t: T) -> Self {
        Dusb { t, rx: Vec::new(), max_raw: DEFAULT_BUF_SIZE as usize, verbose: false }
    }

    pub fn into_inner(self) -> T {
        self.t
    }

    pub fn set_timeout(&mut self, t: Duration) {
        self.t.set_timeout(t)
    }

    /// [`connect`](Self::connect), and if the calculator doesn't answer (its link can be left
    /// wedged by an earlier session), reset the link once and try again.
    pub fn connect_or_reset(&mut self) -> Result<()> {
        self.set_timeout(Duration::from_secs(3));
        let r = match self.connect() {
            Ok(()) => Ok(()),
            Err(e) if e.downcast_ref::<CalcError>().is_some() => Err(e),
            Err(e) => {
                if self.verbose {
                    eprintln!("handshake failed ({e:#}); resetting link");
                }
                self.reset_link()
            }
        };
        self.set_timeout(Duration::from_secs(10));
        r
    }

    /// Recover from a calculator that stopped answering: reset the link and redo the handshake.
    pub fn reset_link(&mut self) -> Result<()> {
        self.t.reset()?;
        self.rx.clear();
        self.max_raw = DEFAULT_BUF_SIZE as usize;
        self.connect()
    }

    // ---------- raw layer ----------

    pub fn send_raw(&mut self, ty: u8, payload: &[u8]) -> Result<()> {
        let mut p = Vec::with_capacity(payload.len() + 5);
        p.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        p.push(ty);
        p.extend_from_slice(payload);
        if self.verbose {
            eprintln!(">> raw t{ty} {}", hex(&p[5..], 24));
        }
        self.t.write(&p)
    }

    fn fill(&mut self, n: usize) -> Result<()> {
        let mut buf = [0u8; 4096];
        while self.rx.len() < n {
            let got = self.t.read(&mut buf)?;
            self.rx.extend_from_slice(&buf[..got]);
        }
        Ok(())
    }

    pub fn recv_raw(&mut self) -> Result<(u8, Vec<u8>)> {
        self.fill(5)?;
        let len = be32(&self.rx, 0)? as usize;
        let ty = self.rx[4];
        ensure!(len <= 0x10000 && (1..=5).contains(&ty), "garbled raw packet header {:02X?}", &self.rx[..5]);
        self.fill(5 + len)?;
        let payload = self.rx[5..5 + len].to_vec();
        self.rx.drain(..5 + len);
        if self.verbose {
            eprintln!("<< raw t{ty} {}", hex(&payload, 24));
        }
        Ok((ty, payload))
    }

    /// Negotiate the raw packet size. Must be the first thing done on a fresh connection.
    pub fn negotiate(&mut self) -> Result<()> {
        self.send_raw(RPKT_BUF_SIZE_REQ, &DEFAULT_BUF_SIZE.to_be_bytes())?;
        let (ty, p) = self.recv_raw()?;
        ensure!(ty == RPKT_BUF_SIZE_ALLOC, "expected buffer size allocation, got raw type {ty}");
        let size = be32(&p, 0)?;
        // libticalcs clamps CE allocations to 1018; smaller is always allowed, so do it for all.
        self.max_raw = size.min(1018).max(16) as usize;
        Ok(())
    }

    // ---------- virtual layer ----------

    pub fn send_vpkt(&mut self, vtype: u16, data: &[u8]) -> Result<()> {
        if self.verbose {
            eprintln!(">> vpkt {vtype:#06x} [{}] {}", data.len(), hex(data, 32));
        }
        let mut buf = Vec::with_capacity(data.len() + 6);
        buf.extend_from_slice(&(data.len() as u32).to_be_bytes());
        buf.extend_from_slice(&vtype.to_be_bytes());
        buf.extend_from_slice(data);
        let chunks: Vec<&[u8]> = buf.chunks(self.max_raw).collect();
        let n = chunks.len();
        for (i, c) in chunks.into_iter().enumerate() {
            let ty = if i + 1 == n { RPKT_VIRT_DATA_LAST } else { RPKT_VIRT_DATA };
            self.send_raw(ty, c)?;
            self.recv_raw_ack()?;
        }
        Ok(())
    }

    fn recv_raw_ack(&mut self) -> Result<()> {
        loop {
            let (ty, p) = self.recv_raw()?;
            match ty {
                RPKT_VIRT_DATA_ACK => return Ok(()),
                RPKT_BUF_SIZE_REQ => {
                    self.answer_buf_size_req(&p)?;
                }
                _ => bail!("expected raw ACK, got raw type {ty}"),
            }
        }
    }

    /// Receive one virtual packet, transparently handling DELAY_ACK and turning ERROR
    /// packets into [`CalcError`].
    pub fn recv_vpkt(&mut self) -> Result<(u16, Vec<u8>)> {
        loop {
            let (vtype, data) = self.recv_vpkt_raw()?;
            match vtype {
                VPKT_DELAY_ACK => {
                    let us = be32(&data, 0).unwrap_or(0).min(5_000_000);
                    std::thread::sleep(Duration::from_micros(us as u64));
                }
                VPKT_ERROR => return Err(CalcError(be16(&data, 0).unwrap_or(0xFFFF)).into()),
                _ => return Ok((vtype, data)),
            }
        }
    }

    fn recv_vpkt_raw(&mut self) -> Result<(u16, Vec<u8>)> {
        let mut out: Option<(u32, u16, Vec<u8>)> = None;
        loop {
            let (ty, p) = self.recv_raw()?;
            match ty {
                RPKT_VIRT_DATA | RPKT_VIRT_DATA_LAST => {}
                RPKT_BUF_SIZE_REQ => {
                    self.answer_buf_size_req(&p)?;
                    continue;
                }
                _ => bail!("unexpected raw packet type {ty} while waiting for data"),
            }
            self.send_raw(RPKT_VIRT_DATA_ACK, &[0xE0, 0x00])?;
            match &mut out {
                None => {
                    let size = be32(&p, 0)?;
                    let vtype = be16(&p, 4)?;
                    let mut d = Vec::with_capacity(size as usize);
                    d.extend_from_slice(&p[6..]);
                    out = Some((size, vtype, d));
                }
                Some((_, _, d)) => d.extend_from_slice(&p),
            }
            if ty == RPKT_VIRT_DATA_LAST {
                let (size, vtype, d) = out.unwrap();
                if d.len() != size as usize {
                    eprintln!("warning: virtual packet {vtype:#06x} announced {size} bytes, got {}", d.len());
                }
                if self.verbose {
                    eprintln!("<< vpkt {vtype:#06x} [{}] {}", d.len(), hex(&d, 32));
                }
                return Ok((vtype, d));
            }
        }
    }

    /// The calculator asked for a raw packet size (it does this e.g. after OS_BEGIN).
    /// Like the reference implementation, grant exactly what it asked for.
    fn answer_buf_size_req(&mut self, p: &[u8]) -> Result<()> {
        let want = be32(p, 0)?.clamp(16, 0xFFFF);
        self.max_raw = want as usize;
        self.send_raw(RPKT_BUF_SIZE_ALLOC, &want.to_be_bytes())
    }

    pub fn expect(&mut self, want: u16) -> Result<Vec<u8>> {
        let (vtype, data) = self.recv_vpkt()?;
        ensure!(vtype == want, "expected packet {want:#06x}, got {vtype:#06x}");
        Ok(data)
    }

    // ---------- commands ----------

    /// Raw-size negotiation followed by the "normal mode" ping. Call once after opening.
    pub fn connect(&mut self) -> Result<()> {
        self.negotiate()?;
        // mode 3 = normal, 1, 0, 0, 0x07D0 (2000 ms timeout)
        self.send_vpkt(VPKT_PING, &[0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x07, 0xD0])?;
        // Real hardware (TI-84 Plus, OS 2.x) answers with MODE_SET `00 00 07 D0`.
        let (vtype, _) = self.recv_vpkt()?;
        ensure!(
            vtype == VPKT_MODE_SET || vtype == VPKT_DATA_ACK,
            "expected mode acknowledgement, got packet {vtype:#06x}"
        );
        Ok(())
    }

    /// Request parameters; result is `(id, Some(data))` or `(id, None)` if unsupported.
    pub fn params(&mut self, ids: &[u16]) -> Result<Vec<(u16, Option<Vec<u8>>)>> {
        let mut d = (ids.len() as u16).to_be_bytes().to_vec();
        for id in ids {
            d.extend_from_slice(&id.to_be_bytes());
        }
        self.send_vpkt(VPKT_PARM_REQ, &d)?;
        let d = self.expect(VPKT_PARM_DATA)?;
        let n = be16(&d, 0)? as usize;
        let mut j = 2;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let id = be16(&d, j)?;
            let unavailable = *d.get(j + 2).context("packet truncated")? != 0;
            j += 3;
            if unavailable {
                out.push((id, None));
                continue;
            }
            let size = be16(&d, j)? as usize;
            j += 2;
            // The size field is 16 bits, too small for e.g. a 153600-byte CE screenshot:
            // for the last parameter take everything that's left.
            let end = if i + 1 == n { d.len() } else { j + size };
            let v = d.get(j..end).context("packet truncated")?.to_vec();
            j = end;
            out.push((id, Some(v)));
        }
        Ok(out)
    }

    pub fn param(&mut self, id: u16) -> Result<Vec<u8>> {
        self.params(&[id])?
            .into_iter()
            .find(|(i, _)| *i == id)
            .and_then(|(_, d)| d)
            .ok_or_else(|| anyhow!("calculator does not support parameter {id:#06x}"))
    }

    pub fn list_dir(&mut self) -> Result<Vec<DirEntry>> {
        let aids = [AID_VAR_SIZE, AID_VAR_TYPE, AID_ARCHIVED];
        let mut d = (aids.len() as u32).to_be_bytes().to_vec();
        for a in aids {
            d.extend_from_slice(&a.to_be_bytes());
        }
        d.extend_from_slice(&[0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x01]);
        self.send_vpkt(VPKT_DIR_REQ, &d)?;
        let mut out = Vec::new();
        loop {
            let (vtype, data) = self.recv_vpkt()?;
            match vtype {
                VPKT_EOT => {
                    // A TI-84 Plus silently drops the next request if it arrives right after
                    // the end of a listing (it doesn't even ACK it); 100 ms was enough on real
                    // hardware, so allow a margin.
                    std::thread::sleep(SETTLE);
                    break;
                }
                VPKT_VAR_HDR => {
                    let e = parse_var_header(&data)?;
                    if !e.name.is_empty() {
                        out.push(e);
                    }
                }
                _ => bail!("unexpected packet {vtype:#06x} in directory listing"),
            }
        }
        Ok(out)
    }

    /// Fetch a variable's data. Returns the header attributes and the raw contents.
    pub fn get_var(&mut self, name: &[u8], var_type: u8) -> Result<(Vec<Attr>, Vec<u8>)> {
        let mut d = Vec::new();
        put_str(&mut d, b"");
        put_str(&mut d, name);
        d.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF]);
        let aids = [AID_ARCHIVED, AID_VAR_VERSION, AID_VAR_SIZE];
        d.extend_from_slice(&(aids.len() as u16).to_be_bytes());
        for a in aids {
            d.extend_from_slice(&a.to_be_bytes());
        }
        put_attrs(&mut d, &[Attr::new(AID_VAR_TYPE2, type_attr(0x07, var_type))]);
        d.extend_from_slice(&[0x00, 0x00]);
        self.send_vpkt(VPKT_VAR_REQ, &d)?;
        let hdr = parse_var_header(&self.expect(VPKT_VAR_HDR)?)?;
        let data = self.expect(VPKT_VAR_CNTS)?;
        Ok((hdr.attrs, data))
    }

    pub fn send_var(&mut self, name: &[u8], var_type: u8, version: u8, archived: bool, data: &[u8]) -> Result<()> {
        let mut d = Vec::new();
        put_str(&mut d, b"");
        put_str(&mut d, name);
        d.extend_from_slice(&(data.len() as u32).to_be_bytes());
        d.push(0x01);
        put_attrs(
            &mut d,
            &[
                Attr::new(AID_VAR_TYPE, type_attr(0x07, var_type)),
                Attr::new(AID_ARCHIVED, vec![archived as u8]),
                Attr::new(AID_VAR_VERSION, vec![0, 0, 0, version]),
            ],
        );
        self.send_vpkt(VPKT_RTS, &d)?;
        self.expect(VPKT_DATA_ACK)?;
        self.send_vpkt(VPKT_VAR_CNTS, data)?;
        self.expect(VPKT_DATA_ACK)?;
        self.send_vpkt(VPKT_EOT, &[])?;
        std::thread::sleep(SETTLE);
        Ok(())
    }

    pub fn delete_var(&mut self, name: &[u8], var_type: u8) -> Result<()> {
        let mut d = Vec::new();
        put_str(&mut d, b"");
        put_str(&mut d, name);
        // MODIF_VAR = source (folder, name, attributes), protection mode, destination; an empty
        // destination means delete. (An earlier layout put the mode byte before the attributes;
        // the TI-84 Plus tolerated it, the CE answered ERROR 0x0004.)
        put_attrs(&mut d, &[Attr::new(AID_VAR_TYPE2, type_attr(0x0B, var_type))]);
        d.push(0x01); // protection mode: ignore
        d.push(0x00); // destination folder: none
        d.push(0x00); // destination name: none
        d.extend_from_slice(&[0x00, 0x00]); // destination attributes: none
        self.send_vpkt(VPKT_MODIF_VAR, &d)?;
        self.expect(VPKT_DATA_ACK)?;
        Ok(())
    }

    /// Set a parameter (e.g. the clock). `u16 id | u16 size | data`, answered with DATA_ACK.
    pub fn set_param(&mut self, id: u16, data: &[u8]) -> Result<()> {
        let mut d = id.to_be_bytes().to_vec();
        d.extend_from_slice(&(data.len() as u16).to_be_bytes());
        d.extend_from_slice(data);
        self.send_vpkt(VPKT_PARM_SET, &d)?;
        self.expect(VPKT_DATA_ACK)?;
        Ok(())
    }
}

fn hex(d: &[u8], max: usize) -> String {
    let mut s: Vec<String> = d.iter().take(max).map(|b| format!("{b:02X}")).collect();
    if d.len() > max {
        s.push("…".into());
    }
    s.join(" ")
}

/// Lets a boxed transport be used where a concrete one is expected (e.g. USB or mock at runtime).
impl Transport for Box<dyn Transport + Send> {
    fn write(&mut self, data: &[u8]) -> Result<()> {
        (**self).write(data)
    }
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        (**self).read(buf)
    }
    fn reset(&mut self) -> Result<()> {
        (**self).reset()
    }
    fn set_timeout(&mut self, t: Duration) {
        (**self).set_timeout(t)
    }
}

pub mod mock {
    //! A tiny simulated calculator, enough to exercise framing, fragmentation and commands.
    //! Used by the tests, and by the GUI when `CALLINK_MOCK=1` is set.
    use super::*;
    use std::collections::VecDeque;

    /// What the mock received during an OS install.
    #[derive(Default, Debug)]
    pub struct MockOs {
        pub header: Vec<u8>,
        pub data: Vec<Vec<u8>>,
        pub done: bool,
    }

    pub struct MockCalc {
        pub vars: Vec<(Vec<u8>, u8, Vec<u8>)>,
        pub buf_size: u32,
        pub screen: Vec<u8>,
        out: VecDeque<u8>,
        inbuf: Vec<u8>,
        assembling: Option<Vec<u8>>,
        /// Virtual packets waiting to go to the host, as raw fragments; a fragment is only
        /// released after the host ACKs the previous one.
        pending: VecDeque<Vec<u8>>,
        awaiting_ack: bool,
        rts: Option<(Vec<u8>, u8)>,
        pub log: Vec<u16>,
        /// seconds since 1997-01-01
        pub clock: u32,
        /// Behave like a CE: refuse 0x0025 and use the separate clock fields.
        pub new_clock: bool,
        pub clock_fields: std::collections::HashMap<u16, Vec<u8>>,
        pub os: Option<MockOs>,
    }

    impl MockCalc {
        pub fn new(buf_size: u32) -> Self {
            MockCalc {
                vars: vec![],
                buf_size,
                screen: vec![0x1F; 320 * 240 * 2],
                out: VecDeque::new(),
                inbuf: vec![],
                assembling: None,
                pending: VecDeque::new(),
                awaiting_ack: false,
                rts: None,
                log: vec![],
                clock: 938_000_000,
                new_clock: false,
                clock_fields: Default::default(),
                os: None,
            }
        }

        /// A calculator with a few sample variables and a test-pattern screen.
        pub fn demo() -> Self {
            let mut m = MockCalc::new(1023);
            let prog = |t: &str| {
                let tk = crate::tokens::tokenize(t).unwrap();
                let mut d = (tk.len() as u16).to_le_bytes().to_vec();
                d.extend(tk);
                d
            };
            m.vars = vec![
                (b"HELLO".to_vec(), 0x05, prog("ClrHome\nDisp \"Hello from Rust!\"\nFor(I,1,10)\nOutput(I,1,I²)\nEnd")),
                (b"GUESS".to_vec(), 0x05, prog("randInt(1,100)→N\nRepeat G=N\nInput \"Guess? \",G\nEnd")),
                (b"SCORES".to_vec(), 0x15, (0..40u8).collect()),
                ("L₁".as_bytes().to_vec(), 0x01, {
                    let mut d = vec![3, 0];
                    for v in [1u8, 2, 3] {
                        d.extend([0x00, 0x80, v << 4, 0, 0, 0, 0, 0, 0]);
                    }
                    d
                }),
                (b"Str1".to_vec(), 0x04, prog("hello world")),
                (b"A".to_vec(), 0x00, vec![0x00, 0x80, 0x31, 0x41, 0x59, 0x26, 0x53, 0x58, 0x97]),
            ];
            // diagonal colour bands + a white border
            let mut s = Vec::with_capacity(320 * 240 * 2);
            for y in 0..240u32 {
                for x in 0..320u32 {
                    let v: u16 = if x < 2 || y < 2 || x > 317 || y > 237 {
                        0xFFFF
                    } else {
                        let r = (x * 31 / 319) as u16;
                        let g = (y * 63 / 239) as u16;
                        let b = (((x + y) / 8) % 32) as u16;
                        (r << 11) | (g << 5) | b
                    };
                    s.extend(v.to_le_bytes());
                }
            }
            m.screen = s;
            m
        }

        fn raw(&mut self, ty: u8, p: &[u8]) {
            self.out.extend((p.len() as u32).to_be_bytes());
            self.out.push_back(ty);
            self.out.extend(p);
        }

        fn queue_vpkt(&mut self, vtype: u16, data: &[u8]) {
            let mut b = (data.len() as u32).to_be_bytes().to_vec();
            b.extend_from_slice(&vtype.to_be_bytes());
            b.extend_from_slice(data);
            let chunks: Vec<Vec<u8>> = b.chunks(self.buf_size as usize).map(|c| c.to_vec()).collect();
            let n = chunks.len();
            for (i, c) in chunks.into_iter().enumerate() {
                let mut f = vec![if i + 1 == n { 4 } else { 3 }];
                f.extend(c);
                self.pending.push_back(f);
            }
            self.pump();
        }

        fn pump(&mut self) {
            if !self.awaiting_ack {
                if let Some(f) = self.pending.pop_front() {
                    self.raw(f[0], &f[1..].to_vec());
                    self.awaiting_ack = true;
                }
            }
        }

        fn handle_vpkt(&mut self, vtype: u16, d: Vec<u8>) {
            self.log.push(vtype);
            match vtype {
                VPKT_PING => self.queue_vpkt(VPKT_MODE_SET, &[0x00, 0x00, 0x07, 0xD0]),
                VPKT_PARM_REQ => {
                    let n = be16(&d, 0).unwrap();
                    let mut r = n.to_be_bytes().to_vec();
                    for i in 0..n as usize {
                        let id = be16(&d, 2 + 2 * i).unwrap();
                        r.extend(id.to_be_bytes());
                        let v: Option<Vec<u8>> = match id {
                            PID_PRODUCT_NAME => Some(b"TI-84 Plus CE\0".to_vec()),
                            PID_SCREENSHOT => Some(self.screen.clone()),
                            PID_OS_MODE => Some(vec![0x01]),
                            PID_OS_VERSION => Some(vec![5, 8, 2, 0x00, 0x21]),
                            PID_FREE_RAM => Some(149_456u64.to_be_bytes().to_vec()),
                            PID_FREE_FLASH => Some(2_851_201u64.to_be_bytes().to_vec()),
                            PID_LCD_WIDTH => Some(320u16.to_be_bytes().to_vec()),
                            PID_LCD_HEIGHT => Some(240u16.to_be_bytes().to_vec()),
                            PID_CLK_SEC_SINCE_1997 if !self.new_clock => Some(self.clock.to_be_bytes().to_vec()),
                            PID_CLK_SECONDS..=PID_CLK_YEAR if self.new_clock => self.clock_fields.get(&id).cloned(),
                            _ => None,
                        };
                        match v {
                            Some(v) => {
                                r.push(0);
                                r.extend((v.len() as u16).to_be_bytes());
                                r.extend(v);
                            }
                            None => r.push(1),
                        }
                    }
                    self.queue_vpkt(VPKT_PARM_DATA, &r);
                }
                VPKT_DIR_REQ => {
                    for (name, ty, data) in self.vars.clone() {
                        let mut h = vec![0u8];
                        h.push(name.len() as u8);
                        h.extend(&name);
                        h.push(0);
                        h.extend(3u16.to_be_bytes());
                        h.extend(AID_VAR_SIZE.to_be_bytes());
                        h.extend([0, 0, 4]);
                        h.extend((data.len() as u32).to_be_bytes());
                        h.extend(AID_VAR_TYPE.to_be_bytes());
                        h.extend([0, 0, 4, 0xF0, 0x07, 0x00, ty]);
                        h.extend(AID_ARCHIVED.to_be_bytes());
                        h.extend([0, 0, 1, 0]);
                        self.queue_vpkt(VPKT_VAR_HDR, &h);
                    }
                    self.queue_vpkt(VPKT_EOT, &[]);
                }
                VPKT_VAR_REQ => {
                    let mut j = 0;
                    get_str(&d, &mut j).unwrap();
                    let name = get_str(&d, &mut j).unwrap();
                    match self.vars.iter().find(|v| v.0 == name).cloned() {
                        Some((n, _ty, data)) => {
                            let mut h = vec![0u8, n.len() as u8];
                            h.extend(&n);
                            h.push(0);
                            h.extend(0u16.to_be_bytes());
                            self.queue_vpkt(VPKT_VAR_HDR, &h);
                            self.queue_vpkt(VPKT_VAR_CNTS, &data);
                        }
                        None => self.queue_vpkt(VPKT_ERROR, &[0x00, 0x12]),
                    }
                }
                VPKT_RTS => {
                    let mut j = 0;
                    get_str(&d, &mut j).unwrap();
                    let name = get_str(&d, &mut j).unwrap();
                    let attrs_at = j + 5;
                    let ty = d[attrs_at + 2 + 4 + 3]; // first attr's 4th data byte
                    self.rts = Some((name, ty));
                    self.queue_vpkt(VPKT_DATA_ACK, &[0x00, 0x01]);
                }
                VPKT_VAR_CNTS => {
                    let (name, ty) = self.rts.take().unwrap();
                    self.vars.retain(|v| v.0 != name);
                    self.vars.push((name, ty, d));
                    self.queue_vpkt(VPKT_DATA_ACK, &[0x00, 0x01]);
                }
                VPKT_MODIF_VAR => {
                    let mut j = 0;
                    get_str(&d, &mut j).unwrap();
                    let name = get_str(&d, &mut j).unwrap();
                    self.vars.retain(|v| v.0 != name);
                    self.queue_vpkt(VPKT_DATA_ACK, &[0x00, 0x01]);
                }
                VPKT_PARM_SET => {
                    let id = be16(&d, 0).unwrap();
                    if id == PID_CLK_SEC_SINCE_1997 {
                        self.clock = be32(&d, 4).unwrap();
                    } else if self.new_clock {
                        self.clock_fields.insert(id, d[4..].to_vec());
                    }
                    self.queue_vpkt(VPKT_DATA_ACK, &[0x00, 0x01]);
                }
                VPKT_OS_BEGIN => {
                    self.os = Some(MockOs::default());
                    // A running OS (not the boot code) first asks for a raw packet size.
                    self.raw(RPKT_BUF_SIZE_REQ, &0x10Au32.to_be_bytes());
                    self.queue_vpkt(VPKT_OS_ACK, &260u32.to_be_bytes());
                }
                VPKT_OS_HEADER => {
                    self.os.as_mut().unwrap().header = d;
                    self.queue_vpkt(VPKT_OS_ACK, &260u32.to_be_bytes());
                }
                VPKT_OS_DATA => {
                    self.os.as_mut().unwrap().data.push(d);
                    self.queue_vpkt(VPKT_DATA_ACK, &[]);
                }
                VPKT_EOT => {
                    if let Some(os) = self.os.as_mut() {
                        if !os.done {
                            os.done = true;
                            self.queue_vpkt(VPKT_EOT_ACK, &[]);
                        }
                    }
                }
                _ => self.queue_vpkt(VPKT_ERROR, &[0x00, 0x11]),
            }
        }

        fn process(&mut self) {
            loop {
                if self.inbuf.len() < 5 {
                    return;
                }
                let len = be32(&self.inbuf, 0).unwrap() as usize;
                if self.inbuf.len() < 5 + len {
                    return;
                }
                let ty = self.inbuf[4];
                let p: Vec<u8> = self.inbuf[5..5 + len].to_vec();
                self.inbuf.drain(..5 + len);
                assert!(p.len() <= self.buf_size as usize || ty < 3, "host sent oversized fragment");
                match ty {
                    1 => self.raw(2, &self.buf_size.to_be_bytes()),
                    2 => self.buf_size = be32(&p, 0).unwrap(), // host granted our size request
                    3 | 4 => {
                        self.raw(5, &[0xE0, 0x00]);
                        self.assembling.get_or_insert_with(Vec::new).extend(p);
                        if ty == 4 {
                            let b = self.assembling.take().unwrap();
                            let size = be32(&b, 0).unwrap() as usize;
                            let vtype = be16(&b, 4).unwrap();
                            assert_eq!(b.len() - 6, size);
                            self.handle_vpkt(vtype, b[6..].to_vec());
                        }
                    }
                    5 => {
                        self.awaiting_ack = false;
                        self.pump();
                    }
                    _ => panic!("bad raw type {ty}"),
                }
            }
        }
    }

    impl Transport for MockCalc {
        fn write(&mut self, data: &[u8]) -> Result<()> {
            self.inbuf.extend_from_slice(data);
            self.process();
            Ok(())
        }
        fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
            // Return data in small, odd-sized pieces to exercise reassembly.
            let n = self.out.len().min(buf.len()).min(61);
            ensure!(n > 0, "mock: read with nothing to read (host is waiting for a reply that will never come)");
            for b in buf.iter_mut().take(n) {
                *b = self.out.pop_front().unwrap();
            }
            Ok(n)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::MockCalc;
    use super::*;

    fn connected(buf: u32) -> Dusb<MockCalc> {
        let mut d = Dusb::new(MockCalc::new(buf));
        d.connect().unwrap();
        d
    }

    #[test]
    fn connect_and_params() {
        let mut d = connected(250);
        let p = d.params(&[PID_PRODUCT_NAME, 0x7777]).unwrap();
        assert_eq!(p[0].1.as_deref(), Some(&b"TI-84 Plus CE\0"[..]));
        assert!(p[1].1.is_none());
        let shot = d.param(PID_SCREENSHOT).unwrap();
        assert_eq!(shot.len(), 153600);
    }

    #[test]
    fn send_list_get_delete() {
        let mut d = connected(1023);
        let big: Vec<u8> = (0..5000u32).map(|i| (i * 7) as u8).collect();
        d.send_var(b"BIG", 0x15, 0, true, &big).unwrap();
        d.send_var(b"HELLO", 0x05, 0, false, &[3, 0, 0xDE, 0x2A, 0x2A]).unwrap();
        let dir = d.list_dir().unwrap();
        assert_eq!(dir.len(), 2);
        assert_eq!(dir[0].name, b"BIG");
        assert_eq!(dir[0].var_type(), Some(0x15));
        assert_eq!(dir[0].size(), Some(5000));
        let (_, data) = d.get_var(b"BIG", 0x15).unwrap();
        assert_eq!(data, big);
        d.delete_var(b"BIG", 0x15).unwrap();
        assert_eq!(d.list_dir().unwrap().len(), 1);
        let err = d.get_var(b"NOPE", 0x05).unwrap_err();
        assert!(err.downcast_ref::<CalcError>().is_some_and(|e| e.0 == 0x12));
    }
}
