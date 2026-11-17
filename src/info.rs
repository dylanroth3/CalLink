//! Calculator information (the "Handheld information" dialog in TI Connect CE).

use std::time::Duration;

use anyhow::{Context, Result};

use crate::dusb::{self, Dusb, Transport};
use crate::model::Model;

/// Which calculator this is: (model, product name, OS version as TI writes it).
pub fn identify<T: Transport>(d: &mut Dusb<T>) -> Result<(Model, String, String)> {
    let name = String::from_utf8_lossy(&d.param(dusb::PID_PRODUCT_NAME)?).trim_end_matches('\0').to_string();
    let product = d.param(dusb::PID_PRODUCT_NUMBER).ok().and_then(|v| v.last().copied());
    let model = Model::identify(product, &name);
    let os = d.param(dusb::PID_OS_VERSION).map(|v| model.format_version(&v)).unwrap_or_else(|_| "unknown".into());
    Ok((model, name, os))
}

/// Query everything we know how to display. Returns (label, value) pairs for supported items.
pub fn read_info<T: Transport>(d: &mut Dusb<T>) -> Result<Vec<(String, String)>> {
    // 0x0003 (main part ID) and 0x000F (physical flash) are left out: a TI-84 Plus on OS 2.55MP
    // never answers them and its USB link wedges.
    let ids: &[(u16, &str)] = &[
        (dusb::PID_PRODUCT_NAME, "Product name"),
        (dusb::PID_PRODUCT_NUMBER, "Product number"),
        (dusb::PID_FULL_ID, "Calculator ID"),
        (dusb::PID_HW_VERSION, "Hardware version"),
        (dusb::PID_OS_VERSION, "OS version"),
        (dusb::PID_BOOT_VERSION, "Boot version"),
        (dusb::PID_LANGUAGE_ID, "Language"),
        (dusb::PID_PHYS_RAM, "Physical RAM"),
        (dusb::PID_USER_RAM, "User RAM"),
        (dusb::PID_FREE_RAM, "Free RAM"),
        (dusb::PID_USER_FLASH, "User flash (archive)"),
        (dusb::PID_FREE_FLASH, "Free flash (archive)"),
        (dusb::PID_LCD_WIDTH, "LCD width"),
        (dusb::PID_LCD_HEIGHT, "LCD height"),
        (dusb::PID_BATTERY, "Battery"),
        (dusb::PID_CLK_SEC_SINCE_1997, "Clock"),
    ];
    let mut out = Vec::new();
    // Ask one at a time, with a short timeout: some OS versions (seen on a TI-84 Plus) never
    // answer a parameter they don't support and their USB link wedges, so on a timeout we
    // reset the link and move on.
    d.set_timeout(Duration::from_secs(3));
    let mut model = Model::Unknown(0);
    for (id, label) in ids {
        match d.params(&[*id]) {
            Ok(v) => {
                if let Some(data) = v.into_iter().next().and_then(|x| x.1) {
                    // the CE reports free RAM as 0; leave it out rather than show a wrong value
                    if *id == dusb::PID_FREE_RAM && dusb::be_uint(&data) == 0 {
                        continue;
                    }
                    let value = match *id {
                        dusb::PID_OS_VERSION | dusb::PID_BOOT_VERSION => model.format_version(&data),
                        _ => format_param(*id, &data),
                    };
                    out.push((label.to_string(), value));
                    if *id == dusb::PID_PRODUCT_NUMBER {
                        let name = out.iter().find(|(k, _)| k == "Product name").map(|(_, v)| v.clone()).unwrap_or_default();
                        model = Model::identify(data.last().copied(), &name);
                        out.push(("Model".into(), model.name().into()));
                    }
                }
            }
            Err(e) if e.downcast_ref::<dusb::CalcError>().is_some() => {}
            Err(e) => {
                if d.verbose {
                    eprintln!("parameter {id:#06x} got no answer ({e:#}); resetting link");
                }
                d.reset_link().context("calculator stopped responding and the USB reset did not recover it")?;
            }
        }
    }
    d.set_timeout(Duration::from_secs(10));
    if !out.iter().any(|(k, _)| k == "Clock") {
        // newer models (CE) keep the date and time in separate parameters
        if let Ok(t) = crate::backup::clock_text(d) {
            out.push(("Clock".into(), t));
        }
    }
    Ok(out)
}

pub fn format_param(id: u16, d: &[u8]) -> String {
    let int = dusb::be_uint(d);
    match id {
        dusb::PID_PRODUCT_NAME | dusb::PID_FULL_ID | dusb::PID_MAIN_PART_ID => {
            let s: String = String::from_utf8_lossy(d).trim_end_matches('\0').to_string();
            if s.chars().all(|c| !c.is_control()) { s } else { hexs(d) }
        }
        dusb::PID_OS_VERSION | dusb::PID_BOOT_VERSION => {
            // major, minor, revision, build (build is 16-bit on newer OSes)
            match d.len() {
                // TI-84 Plus CE (OS/boot 5.x): 00 major minor patch, e.g. 00 05 08 03 = 5.8.3
                4 if d[0] == 0 && d[1] >= 5 => format!("{}.{}.{}", d[1], d[2], d[3]),
                // TI-84 Plus: 00 major minor build, e.g. 00 02 37 00 = 2.55
                4 if d[0] == 0 && d[3] == 0 => format!("{}.{:02}", d[1], d[2]),
                4 if d[0] == 0 => format!("{}.{:02} (build {})", d[1], d[2], d[3]),
                4 => format!("{}.{}.{}.{:04}", d[0], d[1], d[2], d[3]),
                5 => format!("{}.{}.{}.{:04}", d[0], d[1], d[2], u16::from_be_bytes([d[3], d[4]])),
                _ => d.iter().map(|b| b.to_string()).collect::<Vec<_>>().join("."),
            }
        }
        dusb::PID_PHYS_RAM | dusb::PID_USER_RAM | dusb::PID_FREE_RAM | dusb::PID_PHYS_FLASH | dusb::PID_USER_FLASH
        | dusb::PID_FREE_FLASH => format!("{int} bytes"),
        dusb::PID_CLK_SEC_SINCE_1997 => {
            // 1997-01-01T00:00:00 is 852076800 Unix seconds
            let unix = int as i64 + 852_076_800;
            let days = unix.div_euclid(86400);
            let secs = unix.rem_euclid(86400);
            let (y, m, dd) = civil_from_days(days);
            format!("{y:04}-{m:02}-{dd:02} {:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
        }
        dusb::PID_BATTERY => match d.first() {
            Some(0) => "low".into(),
            Some(_) if d.len() == 1 => "ok".into(),
            _ => hexs(d),
        },
        _ if d.len() <= 8 => format!("{int}"),
        _ => hexs(d),
    }
}

fn hexs(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
