//! Whole-calculator backup and restore, and clock sync.
//!
//! A backup is a plain folder, `CalLink backup <model> <date time>/`, with one `.8x*` file
//! per variable (the same files `get` writes) and a `backup.txt` manifest. Being ordinary
//! variable files, a backup can be inspected, edited, or restored piecemeal, also by
//! other tools. Flash apps and the OS are not included (they can't be read back over USB;
//! reinstall them from their original files).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{Local, NaiveDate, NaiveDateTime};

use crate::dusb::{self, CalcError, DirEntry, Dusb, Transport};
use crate::varfile::{self, VarEntry, VarFile, VarType};

pub const MANIFEST: &str = "backup.txt";
const COMMENT: &str = "Created by CalLink";

/// Types that live in flash as apps/OS/certificates and can't be backed up as variables.
pub fn is_flash_only(t: VarType) -> bool {
    matches!(t.0, 0x23 | 0x24 | 0x25 | 0x27)
}

pub fn entry_type(e: &DirEntry) -> VarType {
    VarType(e.var_type().unwrap_or(0xFF))
}

/// Receive one listed variable as a [`VarEntry`] ready to be written to a .8x file.
pub fn receive_entry<T: Transport>(d: &mut Dusb<T>, e: &DirEntry) -> Result<VarEntry> {
    let t = entry_type(e);
    let shown = varfile::link_display_name(&e.name);
    let (attrs, data) = d.get_var(&e.name, t.0).with_context(|| format!("receiving {shown}"))?;
    let version = dusb::find_attr(&attrs, dusb::AID_VAR_VERSION).and_then(|v| v.last().copied()).unwrap_or(0);
    Ok(VarEntry { var_type: t, name: varfile::file_name_from_link_name(t, &e.name), version, archived: e.archived(), data })
}

pub struct BackupReport {
    pub folder: PathBuf,
    pub saved: usize,
    /// Flash apps etc. that were listed but not backed up.
    pub skipped: Vec<String>,
}

/// Back up every variable into a new timestamped folder inside `parent`.
pub fn backup<T: Transport>(d: &mut Dusb<T>, parent: &Path, progress: &mut dyn FnMut(String)) -> Result<BackupReport> {
    let model = d
        .param(dusb::PID_PRODUCT_NAME)
        .map(|v| String::from_utf8_lossy(&v).trim_end_matches('\0').to_string())
        .unwrap_or_else(|_| "calculator".into());
    let stamp = Local::now().format("%Y-%m-%d %H%M%S");
    let folder = parent.join(format!("CalLink backup {model} {stamp}"));
    fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;

    let dir = d.list_dir()?;
    let (vars, flash): (Vec<&DirEntry>, Vec<&DirEntry>) = dir.iter().partition(|e| !is_flash_only(entry_type(e)));
    let mut manifest = format!("CalLink backup of {model}\nTaken {}\n\n", Local::now().format("%Y-%m-%d %H:%M:%S"));
    let mut used = std::collections::HashSet::new();
    for (i, e) in vars.iter().enumerate() {
        let shown = varfile::link_display_name(&e.name);
        progress(format!("Backing up {shown} ({}/{})", i + 1, vars.len()));
        let v = receive_entry(d, e)?;
        // Different names can map to the same safe file name (e.g. "L₁" and "L1" in custom lists).
        let stem = varfile::safe_file_stem(&shown);
        let mut file = format!("{stem}.{}", v.var_type.extension_for(v.data.len()));
        let mut n = 2;
        while !used.insert(file.clone()) {
            file = format!("{stem}_{n}.{}", v.var_type.extension_for(v.data.len()));
            n += 1;
        }
        let bytes = VarFile { comment: COMMENT.into(), entries: vec![v.clone()] }.to_bytes()?;
        fs::write(folder.join(&file), bytes)?;
        manifest.push_str(&format!(
            "{file:<20} {shown:<10} {:<18} {:>6} bytes  {}\n",
            v.var_type.name(),
            v.data.len(),
            if v.archived { "archive" } else { "RAM" }
        ));
    }
    let skipped: Vec<String> = flash.iter().map(|e| varfile::link_display_name(&e.name)).collect();
    if !skipped.is_empty() {
        manifest.push_str(&format!("\nNot included (flash apps / OS, reinstall separately): {}\n", skipped.join(", ")));
    }
    fs::write(folder.join(MANIFEST), manifest)?;
    Ok(BackupReport { folder, saved: vars.len(), skipped })
}

/// The variable files in a backup folder (or any folder of .8x* files).
pub fn backup_files(folder: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = fs::read_dir(folder)
        .with_context(|| format!("reading {}", folder.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            // .8xk/.8ek apps, .8xu/.8eu OS and .8xg groups (which libticalcs-style tools
            // ungroup) are not handled here
            ext.len() == 3 && ext.starts_with('8') && !matches!(ext.as_str(), "8xk" | "8ek" | "8xu" | "8eu")
        })
        .collect();
    files.sort();
    Ok(files)
}

pub struct RestoreReport {
    pub sent: usize,
    pub replaced: usize,
    pub failed: Vec<String>,
}

/// Send every variable file in `folder`. A variable the calculator refuses because one
/// with that name already exists is deleted and sent again.
pub fn restore<T: Transport>(d: &mut Dusb<T>, folder: &Path, progress: &mut dyn FnMut(String)) -> Result<RestoreReport> {
    let mut entries = Vec::new();
    let mut failed = Vec::new();
    for f in backup_files(folder)? {
        match fs::read(&f).map_err(anyhow::Error::from).and_then(|b| VarFile::parse(&b)) {
            Ok(vf) => entries.extend(vf.entries),
            Err(e) => failed.push(format!("{}: {e:#}", f.display())),
        }
    }
    anyhow::ensure!(!entries.is_empty() || !failed.is_empty(), "no variable files found in {}", folder.display());
    let (mut sent, mut replaced) = (0, 0);
    for (i, e) in entries.iter().enumerate() {
        let shown = e.display_name();
        progress(format!("Restoring {shown} ({}/{})", i + 1, entries.len()));
        let link = varfile::link_name_from_file_name(e.var_type, &e.name);
        match d.send_var(&link, e.var_type.0, e.version, e.archived, &e.data) {
            Ok(()) => sent += 1,
            Err(err) if err.downcast_ref::<CalcError>().is_some() => {
                // Most likely "already exists": replace it.
                let retry = d.delete_var(&link, e.var_type.0).and_then(|_| d.send_var(&link, e.var_type.0, e.version, e.archived, &e.data));
                match retry {
                    Ok(()) => {
                        sent += 1;
                        replaced += 1;
                    }
                    Err(e2) => failed.push(format!("{shown}: {err:#}; replacing it failed too: {e2:#}")),
                }
            }
            Err(err) => return Err(err.context(format!("restoring {shown}"))),
        }
    }
    Ok(RestoreReport { sent, replaced, failed })
}

fn calc_epoch() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(1997, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap()
}

/// Set the calculator clock to this computer's local time. Returns the time that was set.
///
/// The TI-84 Plus keeps the time as seconds since 1997 (one parameter); the CE refuses that
/// parameter and has separate year/month/day/hour/minute/second ones (as libticalcs does it).
pub fn sync_clock<T: Transport>(d: &mut Dusb<T>) -> Result<NaiveDateTime> {
    use chrono::{Datelike, Timelike};
    let now = Local::now().naive_local();
    if d.param(dusb::PID_CLK_SEC_SINCE_1997).is_ok() {
        let secs = (now - calc_epoch()).num_seconds();
        anyhow::ensure!((0..=u32::MAX as i64).contains(&secs), "this computer's clock is out of range");
        d.set_param(dusb::PID_CLK_SEC_SINCE_1997, &(secs as u32).to_be_bytes())
            .context("setting the calculator clock")?;
    } else {
        // In 12-hour mode the CE stores the hour modulo 12 (16:00 becomes 4:00 AM), so switch
        // to 24-hour mode while setting the time and then restore the user's setting.
        let fmt = d.param(dusb::PID_CLK_TIME_FMT).ok().and_then(|v| v.first().copied());
        if fmt.is_some_and(|f| f != 0x81) {
            d.set_param(dusb::PID_CLK_TIME_FMT, &[0x81]).context("switching the clock to 24-hour mode")?;
        }
        let year = now.year() as u16;
        let fields: [(u16, Vec<u8>); 6] = [
            (dusb::PID_CLK_YEAR, year.to_be_bytes().to_vec()),
            (dusb::PID_CLK_MONTH, vec![now.month() as u8]),
            (dusb::PID_CLK_DAY, vec![now.day() as u8]),
            (dusb::PID_CLK_HOURS, vec![now.hour() as u8]),
            (dusb::PID_CLK_MINUTES, vec![now.minute() as u8]),
            (dusb::PID_CLK_SECONDS, vec![now.second() as u8]),
        ];
        let r = fields
            .into_iter()
            .try_for_each(|(id, v)| d.set_param(id, &v).with_context(|| format!("setting the calculator clock (parameter {id:#06x})")))
            .and_then(|_| {
                // check while still in 24-hour mode, where the hour reads back unambiguously
                let h = dusb::be_uint(&d.param(dusb::PID_CLK_HOURS)?) as u32;
                anyhow::ensure!(h == now.hour(), "the calculator kept hour {h} instead of {}", now.hour());
                Ok(())
            });
        if let Some(f) = fmt.filter(|&f| f != 0x81) {
            d.set_param(dusb::PID_CLK_TIME_FMT, &[f]).context("restoring the 12/24-hour setting")?;
        }
        r?;
    }
    Ok(now)
}

/// The calculator clock as text. On a CE in 12-hour mode the hour is reported without
/// AM/PM, so it is shown as 1–12 with a note (switching modes just to read it would change
/// the user's setting).
pub fn clock_text<T: Transport>(d: &mut Dusb<T>) -> Result<String> {
    let t = read_clock(d)?;
    let twelve = d.param(dusb::PID_CLK_SEC_SINCE_1997).is_err()
        && d.param(dusb::PID_CLK_TIME_FMT).ok().and_then(|v| v.first().copied()) == Some(0x80);
    Ok(if twelve {
        format!("{} {}:{} (12-hour mode, AM/PM not reported)", t.format("%Y-%m-%d"), t.format("%-I"), t.format("%M:%S"))
    } else {
        t.format("%Y-%m-%d %H:%M:%S").to_string()
    })
}

/// Read the calculator clock (local time, as the calculator has no time zone).
/// On a CE in 12-hour mode the hour is 1–12 (see [`clock_text`]).
pub fn read_clock<T: Transport>(d: &mut Dusb<T>) -> Result<NaiveDateTime> {
    if let Ok(v) = d.param(dusb::PID_CLK_SEC_SINCE_1997) {
        return Ok(calc_epoch() + chrono::Duration::seconds(dusb::be_uint(&v) as i64));
    }
    let mut get = |id| -> Result<u32> { Ok(dusb::be_uint(&d.param(id)?) as u32) };
    // (in 12-hour mode the hour is 1–12; 12 would be out of range as an hour of the day only
    // if it were 24, so this always yields a valid time)
    let (y, mo, da) = (get(dusb::PID_CLK_YEAR)?, get(dusb::PID_CLK_MONTH)?, get(dusb::PID_CLK_DAY)?);
    let (h, mi, se) = (get(dusb::PID_CLK_HOURS)?, get(dusb::PID_CLK_MINUTES)?, get(dusb::PID_CLK_SECONDS)?);
    NaiveDate::from_ymd_opt(y as i32, mo, da)
        .and_then(|date| date.and_hms_opt(h, mi, se))
        .with_context(|| format!("calculator reported an invalid date {y}-{mo}-{da} {h}:{mi}:{se}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dusb::mock::MockCalc;

    fn scratch(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("callink-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn backup_then_restore_to_empty_calc() {
        let dir = scratch("backup");
        let mut src = Dusb::new(MockCalc::demo());
        src.connect().unwrap();
        let rep = backup(&mut src, &dir, &mut |_| {}).unwrap();
        assert_eq!(rep.saved, 6);
        assert!(rep.folder.join(MANIFEST).exists());
        assert!(rep.folder.join("L1.8xl").exists());

        let mut dst = Dusb::new(MockCalc::new(250));
        dst.connect().unwrap();
        let r = restore(&mut dst, &rep.folder, &mut |_| {}).unwrap();
        assert_eq!((r.sent, r.failed.len()), (6, 0));
        let original = src.into_inner().vars;
        let mut restored = dst.into_inner().vars;
        for v in &original {
            let got = restored.iter().position(|r| r.0 == v.0).expect("variable restored under its USB name");
            assert_eq!(restored.remove(got), *v);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clock_roundtrip_ce_style() {
        let mut m = MockCalc::new(1023);
        m.new_clock = true;
        let mut d = Dusb::new(m);
        d.connect().unwrap();
        let set = sync_clock(&mut d).unwrap();
        let got = read_clock(&mut d).unwrap();
        assert!((got - set).num_seconds().abs() <= 1, "{set} vs {got}");
    }

    #[test]
    fn clock_roundtrip() {
        let mut d = Dusb::new(MockCalc::new(1023));
        d.connect().unwrap();
        let set = sync_clock(&mut d).unwrap();
        let got = read_clock(&mut d).unwrap();
        assert!((got - set).num_seconds().abs() <= 1);
    }
}
