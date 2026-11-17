use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use callink::dusb::{self, Dusb, DirEntry};
use callink::usb::{self, UsbTransport};
use callink::varfile::{self, VarEntry, VarFile, VarType};
use callink::{screen, tokens};

#[derive(Parser)]
#[command(name = "callink-cli", version, about = "Talk to TI-84 Plus / CE calculators and work with .8x* files (a Rust take on TI Connect CE)")]
struct Cli {
    /// Print protocol traffic
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List TI devices on USB
    Devices,
    /// Show calculator information (model, OS version, memory, …)
    Info,
    /// List variables on the calculator
    #[command(alias = "ls")]
    List,
    /// Receive variables from the calculator into .8x* files
    Get {
        /// Variable names as shown by `list` (e.g. HELLO, L₁, Str1). Omit with --all.
        names: Vec<String>,
        /// Restrict to a type (program, appvar, list, string, real, 0x15, …)
        #[arg(short = 't', long = "type")]
        var_type: Option<String>,
        /// Receive every variable (apps and OS excluded)
        #[arg(long)]
        all: bool,
        /// Output directory
        #[arg(short, long, default_value = ".")]
        out: PathBuf,
    },
    /// Send .8x* files to the calculator
    Send {
        files: Vec<PathBuf>,
        /// Store in archive (flash) instead of RAM, overriding the file's flag
        #[arg(long)]
        archive: bool,
        /// Store in RAM, overriding the file's flag
        #[arg(long, conflicts_with = "archive")]
        ram: bool,
    },
    /// Delete a variable from the calculator
    #[command(alias = "rm")]
    Delete {
        name: String,
        #[arg(short = 't', long = "type")]
        var_type: Option<String>,
    },
    /// Capture the calculator screen to a PNG
    Screenshot {
        #[arg(short, long, default_value = "screenshot.png")]
        out: PathBuf,
        /// Integer upscale factor
        #[arg(long, default_value_t = 1)]
        scale: u32,
        /// Treat RGB565 data as big-endian (if colours come out wrong)
        #[arg(long)]
        big_endian: bool,
    },
    /// Inspect .8x* files
    File {
        #[command(subcommand)]
        cmd: FileCmd,
    },
    /// Program editor functions: convert between .8xp and text
    Prog {
        #[command(subcommand)]
        cmd: ProgCmd,
    },
    /// Back up every variable into a new timestamped folder
    Backup {
        /// Where to create the backup folder
        #[arg(short, long, default_value = ".")]
        out: PathBuf,
    },
    /// Send every variable file in a backup folder back to the calculator
    Restore {
        folder: PathBuf,
        /// Don't ask before replacing variables on the calculator
        #[arg(short, long)]
        yes: bool,
    },
    /// Set the calculator's clock to this computer's local date and time
    SyncClock,
    /// Look for a newer OS on TI's website for the connected calculator
    CheckUpdate,
    /// Install an OS file (.8xu for TI-84 Plus, .8eu for TI-84 Plus CE) downloaded from TI
    InstallOs {
        file: PathBuf,
        /// Don't ask for confirmation
        #[arg(short, long)]
        yes: bool,
    },
    /// Print the token table
    Tokens,
}

#[derive(Subcommand)]
enum FileCmd {
    /// List the variables in a file
    Info { files: Vec<PathBuf> },
    /// Show the contents of the variables in a file
    Show { files: Vec<PathBuf> },
}

#[derive(Subcommand)]
enum ProgCmd {
    /// .8xp → text
    Decode {
        file: PathBuf,
        /// Write to this file instead of stdout
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Keep TI's private-use font glyphs instead of readable Unicode
        #[arg(long)]
        raw_glyphs: bool,
    },
    /// text → .8xp
    Encode {
        file: PathBuf,
        /// On-calculator program name (A-Z, 0-9, θ; max 8). Defaults to the file name.
        #[arg(short, long)]
        name: Option<String>,
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Mark as a protected (non-editable) program
        #[arg(long)]
        protected: bool,
        /// Flag the program to be stored in archive
        #[arg(long)]
        archive: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn connect(verbose: bool) -> Result<Dusb<UsbTransport>> {
    let t = UsbTransport::open_first()?;
    let mut d = Dusb::new(t);
    d.verbose = verbose;
    d.connect_or_reset().context("handshake with calculator failed")?;
    Ok(d)
}

fn entry_type(e: &DirEntry) -> VarType {
    VarType(e.var_type().unwrap_or(0xFF))
}

fn entry_name(e: &DirEntry) -> String {
    varfile::link_display_name(&e.name)
}

fn find_entries<'a>(dir: &'a [DirEntry], name: &str, ty: Option<VarType>) -> Result<Vec<&'a DirEntry>> {
    let hits: Vec<&DirEntry> = dir
        .iter()
        .filter(|e| entry_name(e) == name || e.name == name.as_bytes())
        .filter(|e| ty.is_none_or(|t| entry_type(e) == t))
        .collect();
    if hits.is_empty() {
        bail!("no variable named {name:?} on the calculator (see `callink-cli list`)");
    }
    if hits.len() > 1 {
        let types: Vec<_> = hits.iter().map(|e| entry_type(e).name()).collect();
        bail!("{name:?} is ambiguous ({}); pass --type", types.join(", "));
    }
    Ok(hits)
}

fn parse_type(t: &Option<String>) -> Result<Option<VarType>> {
    t.as_deref()
        .map(|s| VarType::parse(s).with_context(|| format!("unknown type {s:?}")))
        .transpose()
}

fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Devices => {
            let devs = usb::list_devices()?;
            if devs.is_empty() {
                println!("No TI devices found.");
            }
            for d in devs {
                println!(
                    "bus {:03} addr {:03}  {:04x}:{:04x}  {}{}",
                    d.bus,
                    d.address,
                    usb::TI_VID,
                    d.pid,
                    d.description,
                    if d.supported { "" } else { "  (not supported)" }
                );
            }
        }
        Cmd::Info => {
            let mut d = connect(cli.verbose)?;
            for (label, value) in callink::info::read_info(&mut d)? {
                println!("{label:<22} {value}");
            }
        }
        Cmd::List => {
            let mut d = connect(cli.verbose)?;
            let mut dir = d.list_dir()?;
            dir.sort_by_key(|e| (entry_type(e).0, entry_name(e)));
            println!("{:<10} {:<18} {:>8}  LOCATION", "NAME", "TYPE", "SIZE");
            for e in &dir {
                println!(
                    "{:<10} {:<18} {:>8}  {}",
                    entry_name(e),
                    entry_type(e).name(),
                    e.size().map_or("?".into(), |s| s.to_string()),
                    if e.archived() { "archive" } else { "RAM" }
                );
            }
            println!("{} variable(s)", dir.len());
        }
        Cmd::Get { names, var_type, all, out } => {
            let ty = parse_type(&var_type)?;
            if names.is_empty() && !all {
                bail!("give variable names or --all");
            }
            let mut d = connect(cli.verbose)?;
            let dir = d.list_dir()?;
            let targets: Vec<&DirEntry> = if all {
                dir.iter()
                    .filter(|e| !matches!(entry_type(e).0, 0x23 | 0x24 | 0x25 | 0x27))
                    .filter(|e| ty.is_none_or(|t| entry_type(e) == t))
                    .collect()
            } else {
                let mut v = Vec::new();
                for n in &names {
                    v.extend(find_entries(&dir, n, ty)?);
                }
                v
            };
            fs::create_dir_all(&out)?;
            for e in targets {
                let t = entry_type(e);
                if t == VarType::APP || t == VarType::OS {
                    eprintln!("skipping {} ({}): flash apps/OS cannot be received", entry_name(e), t.name());
                    continue;
                }
                let (attrs, data) = d.get_var(&e.name, t.0).with_context(|| format!("receiving {}", entry_name(e)))?;
                let version = dusb::find_attr(&attrs, dusb::AID_VAR_VERSION).and_then(|v| v.last().copied()).unwrap_or(0);
                let entry = VarEntry { var_type: t, name: varfile::file_name_from_link_name(t, &e.name), version, archived: e.archived(), data };
                let file = VarFile { comment: "Created by CalLink".into(), entries: vec![entry] };
                let path = out.join(format!("{}.{}", varfile::safe_file_stem(&entry_name(e)), t.extension_for(file.entries[0].data.len())));
                fs::write(&path, file.to_bytes()?)?;
                println!("{} -> {}", entry_name(e), path.display());
            }
        }
        Cmd::Send { files, archive, ram } => {
            if files.is_empty() {
                bail!("no files given");
            }
            let mut parsed = Vec::new();
            for f in &files {
                let bytes = fs::read(f).with_context(|| format!("reading {}", f.display()))?;
                parsed.push((f, VarFile::parse(&bytes).with_context(|| f.display().to_string())?));
            }
            let mut d = connect(cli.verbose)?;
            for (f, vf) in parsed {
                for e in vf.entries {
                    let arch = if archive { true } else if ram { false } else { e.archived };
                    d.send_var(&varfile::link_name_from_file_name(e.var_type, &e.name), e.var_type.0, e.version, arch, &e.data)
                        .with_context(|| format!("sending {} from {}", e.display_name(), f.display()))?;
                    println!("{} ({}) -> {}", e.display_name(), e.var_type.name(), if arch { "archive" } else { "RAM" });
                }
            }
        }
        Cmd::Delete { name, var_type } => {
            let ty = parse_type(&var_type)?;
            let mut d = connect(cli.verbose)?;
            let dir = d.list_dir()?;
            let e = find_entries(&dir, &name, ty)?[0].clone();
            d.delete_var(&e.name, entry_type(&e).0)?;
            println!("deleted {} ({})", entry_name(&e), entry_type(&e).name());
        }
        Cmd::Screenshot { out, scale, big_endian } => {
            let mut d = connect(cli.verbose)?;
            let data = d.param(dusb::PID_SCREENSHOT)?;
            let img = screen::decode(&data, big_endian)?;
            let f = fs::File::create(&out)?;
            screen::write_png(&img, std::io::BufWriter::new(f), scale)?;
            println!("saved {}x{} screenshot to {}", img.width * scale.max(1), img.height * scale.max(1), out.display());
        }
        Cmd::File { cmd } => match cmd {
            FileCmd::Info { files } => {
                for f in files {
                    let vf = read_varfile(&f)?;
                    println!("{}  comment: {:?}", f.display(), vf.comment);
                    for e in &vf.entries {
                        println!(
                            "  {:<10} {:<18} {:>6} bytes  v{}  {}",
                            e.display_name(),
                            e.var_type.name(),
                            e.data.len(),
                            e.version,
                            if e.archived { "archived" } else { "RAM" }
                        );
                    }
                }
            }
            FileCmd::Show { files } => {
                for f in files {
                    let vf = read_varfile(&f)?;
                    for e in &vf.entries {
                        println!("== {} ({})", e.display_name(), e.var_type.name());
                        println!("{}", varfile::describe_contents(e));
                    }
                }
            }
        },
        Cmd::Prog { cmd } => match cmd {
            ProgCmd::Decode { file, out, raw_glyphs } => {
                let vf = read_varfile(&file)?;
                let e = vf
                    .entries
                    .iter()
                    .find(|e| matches!(e.var_type.0, 0x05 | 0x06))
                    .context("file contains no program")?;
                let body = varfile::token_body(&e.data)?;
                if body.starts_with(&[0xEF, 0x7B]) || body.starts_with(&[0xEF, 0x69]) {
                    eprintln!("note: {} is an assembly/C program; the text below is not meaningful", e.display_name());
                }
                let text = tokens::detokenize(body, raw_glyphs);
                match out {
                    Some(p) => fs::write(&p, text + "\n")?,
                    None => println!("{text}"),
                }
            }
            ProgCmd::Encode { file, name, out, protected, archive } => {
                let text = fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
                let name = match name {
                    Some(n) => n,
                    None => file.file_stem().and_then(|s| s.to_str()).unwrap_or("PROG").to_ascii_uppercase(),
                };
                let mut e = varfile::program_from_text(&name, &text, protected)?;
                e.archived = archive;
                let out = out.unwrap_or_else(|| file.with_extension("8xp"));
                let vf = VarFile { comment: "Created by CalLink".into(), entries: vec![e] };
                fs::write(&out, vf.to_bytes()?)?;
                println!("wrote {}", out.display());
            }
        },
        Cmd::Backup { out } => {
            let mut d = connect(cli.verbose)?;
            let r = callink::backup::backup(&mut d, &out, &mut |m| eprintln!("{m}"))?;
            println!("backed up {} variable(s) to {}", r.saved, r.folder.display());
            if !r.skipped.is_empty() {
                println!("not included (flash apps/OS): {}", r.skipped.join(", "));
            }
        }
        Cmd::Restore { folder, yes } => {
            let files = callink::backup::backup_files(&folder)?;
            if files.is_empty() {
                bail!("no variable files in {}", folder.display());
            }
            if !yes {
                eprint!(
                    "Send {} variable file(s) from {} to the calculator, replacing any with the same names? [y/N] ",
                    files.len(),
                    folder.display()
                );
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if !matches!(answer.trim(), "y" | "Y" | "yes") {
                    bail!("cancelled");
                }
            }
            let mut d = connect(cli.verbose)?;
            let r = callink::backup::restore(&mut d, &folder, &mut |m| eprintln!("{m}"))?;
            println!("restored {} variable(s) ({} replaced existing ones)", r.sent, r.replaced);
            for f in &r.failed {
                println!("failed: {f}");
            }
        }
        Cmd::SyncClock => {
            let mut d = connect(cli.verbose)?;
            let before = callink::backup::clock_text(&mut d).ok();
            let set = callink::backup::sync_clock(&mut d)?;
            let after = callink::backup::clock_text(&mut d)?;
            if let Some(b) = before {
                println!("calculator clock was {b}");
            }
            println!("set to {} (calculator now reads {after})", set.format("%Y-%m-%d %H:%M:%S"));
        }
        Cmd::CheckUpdate => {
            let mut d = connect(cli.verbose)?;
            let (model, name, current) = callink::info::identify(&mut d)?;
            drop(d);
            println!("{name} ({}), OS {current}", model.name());
            let latest = callink::osupdate::check_latest(&name)?;
            match callink::osupdate::version_cmp(&latest.version, &current) {
                std::cmp::Ordering::Greater => {
                    println!("update available: OS {} ({})", latest.version, latest.file_name);
                    println!("download it from TI (accepting their license) at:\n  {}", latest.page_url);
                    println!("then run: callink-cli install-os {}", latest.file_name);
                }
                _ => println!("up to date (latest on TI's site: {})", latest.version),
            }
        }
        Cmd::InstallOs { file, yes } => {
            let bytes = fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            let os = callink::flash::OsFile::parse(&bytes).with_context(|| file.display().to_string())?;
            let mut d = connect(cli.verbose)?;
            let (model, name, current) = callink::info::identify(&mut d)?;
            callink::osupdate::check_compatible(&os, model)?;
            if let Ok(b) = d.param(dusb::PID_BATTERY) {
                if b.first() == Some(&0) {
                    bail!("the calculator's batteries are low; replace or charge them before installing an OS");
                }
            }
            let new_ver = callink::osupdate::version_from_file_name(&file.to_string_lossy()).unwrap_or_else(|| os.version_string());
            let cmp = callink::osupdate::version_cmp(&new_ver, &current);
            println!("{name}: OS {current} → {new_ver} ({})", file.display());
            if cmp.is_lt() {
                println!("warning: this is OLDER than the installed OS. A CE refuses downgrades.");
            } else if cmp.is_eq() {
                println!("note: this is the same version that is already installed (reinstall).");
            }
            println!("Installing an OS clears RAM (archived variables and apps are kept), takes several minutes,");
            println!("and the cable must stay connected the whole time. Back up first with `callink-cli backup`.");
            if !yes {
                eprint!("Install now? [y/N] ");
                let mut a = String::new();
                std::io::stdin().read_line(&mut a)?;
                if !matches!(a.trim(), "y" | "Y" | "yes") {
                    bail!("cancelled");
                }
            }
            let mut last = usize::MAX;
            callink::osupdate::install(&mut d, &os, &mut |f, m| {
                let pct = (f * 100.0) as usize;
                if pct != last {
                    last = pct;
                    eprint!("\r{m}   ");
                }
            })?;
            eprintln!();
            println!("OS installed. The calculator now verifies it and restarts (about a minute).");
            println!("Don't run other commands until it shows its home screen (\"RAM Cleared\"); talking to it");
            println!("while it verifies can knock it off USB. If it isn't found afterwards, unplug and replug the cable.");
        }
        Cmd::Tokens => {
            for (code, text) in tokens::TokenTable::get().all() {
                let shown = text.replace('\n', "\\n");
                if code > 0xFF {
                    println!("{code:04X}  {shown}");
                } else {
                    println!("  {code:02X}  {shown}");
                }
            }
        }
    }
    Ok(())
}


fn read_varfile(p: &Path) -> Result<VarFile> {
    let bytes = fs::read(p).with_context(|| format!("reading {}", p.display()))?;
    VarFile::parse(&bytes).with_context(|| p.display().to_string())
}
