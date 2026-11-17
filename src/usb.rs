//! USB transport (libusb via `rusb`). TI Connect CE uses WinUSB through TI's driver; libusb
//! works with that same driver on Windows, and with no driver at all on Linux/macOS.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusb::{Device, DeviceHandle, Direction, GlobalContext, TransferType};

use crate::dusb::Transport;

pub const TI_VID: u16 = 0x0451;

/// Product IDs of calculators that speak DUSB.
pub const DUSB_PIDS: &[(u16, &str)] = &[
    (0xE003, "TI-84 Plus"),
    (0xE008, "TI-84 Plus SE / C SE / CE / T, TI-83 Premium CE, TI-82 Advanced"),
];

/// Other TI devices we recognise but cannot talk to with this tool.
pub const OTHER_PIDS: &[(u16, &str)] = &[
    (0xE001, "SilverLink cable"),
    (0xE004, "TI-89 Titanium"),
    (0xE012, "TI-Nspire"),
    (0xE01C, "TI-Nspire Lab Cradle"),
    (0xE01E, "TI-Nspire CX Navigator AP"),
    (0xE022, "TI-Nspire CX II"),
];

pub struct FoundDevice {
    pub device: Device<GlobalContext>,
    pub pid: u16,
    pub description: &'static str,
    pub supported: bool,
    pub bus: u8,
    pub address: u8,
}

pub fn list_devices() -> Result<Vec<FoundDevice>> {
    let mut out = Vec::new();
    for device in rusb::devices().context("enumerating USB devices")?.iter() {
        let Ok(desc) = device.device_descriptor() else { continue };
        if desc.vendor_id() != TI_VID {
            continue;
        }
        let pid = desc.product_id();
        let (description, supported) = match DUSB_PIDS.iter().find(|p| p.0 == pid) {
            Some(p) => (p.1, true),
            None => (OTHER_PIDS.iter().find(|p| p.0 == pid).map_or("Unrecognised TI device (CalLink will try it)", |p| p.1), false),
        };
        out.push(FoundDevice { bus: device.bus_number(), address: device.address(), device, pid, description, supported });
    }
    Ok(out)
}

pub struct UsbTransport {
    handle: DeviceHandle<GlobalContext>,
    iface: u8,
    ep_in: u8,
    ep_out: u8,
    max_packet: usize,
    pub timeout: Duration,
}

impl UsbTransport {
    /// Open the first supported calculator.
    pub fn open_first() -> Result<UsbTransport> {
        let devs = list_devices()?;
        // Known TI-84 family devices first; otherwise try any TI device that isn't known to use
        // a different protocol (e.g. a newer model such as the TI-84 Evo).
        let pick = devs.iter().position(|d| d.supported).or_else(|| devs.iter().position(|d| !OTHER_PIDS.iter().any(|p| p.0 == d.pid)));
        let Some(d) = pick.map(|i| &devs[i]) else {
            bail!("no TI-84 Plus family calculator found on USB (is it plugged in and turned on?)");
        };
        Self::open(&d.device)
    }

    pub fn open(device: &Device<GlobalContext>) -> Result<UsbTransport> {
        let handle = device.open().map_err(|e| match e {
            rusb::Error::Access => anyhow::anyhow!(
                "permission denied opening the calculator. On Linux add a udev rule, e.g.\n  \
                 SUBSYSTEM==\"usb\", ATTR{{idVendor}}==\"0451\", MODE=\"0666\"\n\
                 to /etc/udev/rules.d/69-ticalc.rules and replug, or run as root."
            ),
            e => anyhow::Error::new(e).context("opening USB device"),
        })?;
        let _ = handle.set_auto_detach_kernel_driver(true);

        let config = device.active_config_descriptor().or_else(|_| device.config_descriptor(0))?;
        let mut found = None;
        'search: for iface in config.interfaces() {
            for alt in iface.descriptors() {
                let mut ep_in = None;
                let mut ep_out = None;
                let mut mps = 64;
                for ep in alt.endpoint_descriptors() {
                    if ep.transfer_type() != TransferType::Bulk {
                        continue;
                    }
                    match ep.direction() {
                        Direction::In => ep_in = Some(ep.address()),
                        Direction::Out => {
                            ep_out = Some(ep.address());
                            mps = ep.max_packet_size() as usize;
                        }
                    }
                }
                if let (Some(i), Some(o)) = (ep_in, ep_out) {
                    found = Some((alt.interface_number(), alt.setting_number(), i, o, mps));
                    break 'search;
                }
            }
        }
        let Some((iface, alt, ep_in, ep_out, max_packet)) = found else {
            bail!("calculator has no bulk endpoints");
        };
        handle.claim_interface(iface).context("claiming USB interface (is TI Connect or another program using the calculator?)")?;
        if alt != 0 {
            handle.set_alternate_setting(iface, alt)?;
        }
        Ok(UsbTransport { handle, iface, ep_in, ep_out, max_packet, timeout: Duration::from_secs(10) })
    }
}

impl Transport for UsbTransport {
    fn write(&mut self, data: &[u8]) -> Result<()> {
        let mut off = 0;
        while off < data.len() {
            let n = self.handle.write_bulk(self.ep_out, &data[off..], self.timeout).context("USB write")?;
            off += n;
        }
        // A transfer that is an exact multiple of the packet size needs a zero-length packet
        // so the device knows it has ended.
        if data.len().is_multiple_of(self.max_packet) {
            self.handle.write_bulk(self.ep_out, &[], self.timeout).context("USB write (ZLP)")?;
        }
        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        self.do_reset()
    }

    fn set_timeout(&mut self, t: Duration) {
        self.timeout = t;
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        loop {
            match self.handle.read_bulk(self.ep_in, buf, self.timeout) {
                Ok(0) => continue,
                Ok(n) => return Ok(n),
                Err(rusb::Error::Timeout) => bail!("timed out waiting for the calculator"),
                Err(e) => return Err(anyhow::Error::new(e).context("USB read")),
            }
        }
    }
}

impl UsbTransport {
    fn do_reset(&mut self) -> Result<()> {
        let _ = self.handle.release_interface(self.iface);
        match self.handle.reset() {
            Ok(()) => {
                self.handle.claim_interface(self.iface).context("re-claiming interface after reset")?;
                Ok(())
            }
            Err(_) => {
                // The device re-enumerated: find and open it again.
                let timeout = self.timeout;
                let mut last = None;
                for _ in 0..10 {
                    std::thread::sleep(Duration::from_millis(500));
                    match UsbTransport::open_first() {
                        Ok(t) => {
                            *self = t;
                            self.timeout = timeout;
                            return Ok(());
                        }
                        Err(e) => last = Some(e),
                    }
                }
                Err(last.unwrap().context("calculator did not come back after USB reset"))
            }
        }
    }
}

impl Drop for UsbTransport {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(self.iface);
    }
}
