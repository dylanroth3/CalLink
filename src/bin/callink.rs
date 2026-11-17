//! Desktop GUI: Calculator Explorer, Screen Capture and Program Editor, like TI Connect CE.
//!
//! All calculator I/O happens on a worker thread; the UI talks to it through channels.
//! Set `CALLINK_MOCK=1` to run against a simulated calculator.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

use anyhow::{Context as _, Result};
use eframe::egui::{self, Color32, RichText, TextureHandle, TextureOptions};
use egui::text::{CCursor, CCursorRange};

use callink::dusb::{self, DirEntry, Dusb, Transport};
use callink::screen::{self, Image};
use callink::varfile::{self, VarEntry, VarFile, VarType};
use callink::basic::check::{self, Diag, Level};
use callink::basic::interp::{self as basic_interp, Machine, State as DbgState};
use callink::basic::sim;
use callink::basic::suggest::{self, Suggestion};
use callink::{backup, flash, info, osupdate, tokens, usb};

const COMMENT: &str = "Created by CalLink";

/// Sample program used by the scripted debugger screenshots (it has a deliberate bug:
/// the second loop divides by zero when K reaches 0).
const DEMO_PROGRAM: &str = "ClrHome\nDisp \"PRIMES TO 30\"\n{}→ʟP\nFor(N,2,30)\n1→F\nFor(D,2,√(N))\nIf remainder(N,D)=0\n0→F\nEnd\nIf F\nN→ʟP(1+dim(ʟP))\nEnd\nDisp ʟP,\"COUNT\",dim(ʟP)\nFor(K,3,⁻1,⁻1)\nDisp 6/K\nEnd\nGoto Q";
/// After an OS install, keep Connect disabled this long so we don't disturb verification.
const OS_SETTLE_SECS: u64 = 60;

// ───────────────────────────── worker ─────────────────────────────

enum Req {
    Refresh,
    Get { entries: Vec<DirEntry>, dir: PathBuf },
    Open(DirEntry),
    SendFiles { files: Vec<PathBuf>, archive: Option<bool> },
    SendVar(VarEntry),
    Delete(Vec<DirEntry>),
    Screenshot,
    Backup(PathBuf),
    Restore(PathBuf),
    SyncClock,
    CheckUpdate { product: String },
    InstallOs(PathBuf),
    Disconnect,
}

impl Req {
    fn label(&self) -> String {
        match self {
            Req::Refresh => "Reading calculator contents…".into(),
            Req::Get { entries, .. } => format!("Receiving {} variable(s)…", entries.len()),
            Req::Open(e) => format!("Receiving {}…", entry_name(e)),
            Req::SendFiles { files, .. } => format!("Sending {} file(s)…", files.len()),
            Req::SendVar(e) => format!("Sending {}…", e.display_name()),
            Req::Delete(v) => format!("Deleting {} variable(s)…", v.len()),
            Req::Screenshot => "Capturing screen…".into(),
            Req::Backup(_) => "Backing up calculator…".into(),
            Req::Restore(_) => "Restoring backup…".into(),
            Req::SyncClock => "Setting the calculator clock…".into(),
            Req::CheckUpdate { .. } => "Checking TI's website for a newer OS…".into(),
            Req::InstallOs(_) => "Installing OS — keep the calculator connected…".into(),
            Req::Disconnect => "Disconnecting…".into(),
        }
    }
}

enum Resp {
    Connected(Vec<(String, String)>),
    Disconnected,
    Dir(Vec<DirEntry>),
    Screen(Image),
    Opened(VarEntry),
    Info(String),
    Error(String),
    Busy(Option<String>),
    /// Fresh calculator info (after the clock was set)
    InfoTable(Vec<(String, String)>),
    UpdateCheck(std::result::Result<osupdate::LatestOs, String>),
    Progress(Option<f32>),
    /// The OS went through; the calculator is now verifying and rebooting.
    OsInstalled,
}

type Calc = Dusb<Box<dyn Transport + Send>>;

fn open_calc() -> Result<(Calc, Vec<(String, String)>)> {
    let t: Box<dyn Transport + Send> = if std::env::var_os("CALLINK_MOCK").is_some() {
        Box::new(dusb::mock::MockCalc::demo())
    } else {
        Box::new(usb::UsbTransport::open_first()?)
    };
    let mut d = Dusb::new(t);
    d.connect_or_reset().context("handshake with calculator failed")?;
    let info = info::read_info(&mut d)?;
    Ok((d, info))
}

fn entry_type(e: &DirEntry) -> VarType {
    VarType(e.var_type().unwrap_or(0xFF))
}

fn entry_name(e: &DirEntry) -> String {
    varfile::link_display_name(&e.name)
}

fn worker(rx: Receiver<Req>, tx: Sender<Resp>, ctx: egui::Context) {
    let mut calc: Option<Calc> = None;
    let send = |r: Resp| {
        let _ = tx.send(r);
        ctx.request_repaint();
    };
    for req in rx {
        if let Req::Disconnect = req {
            calc = None;
            send(Resp::Disconnected);
            continue;
        }
        if let Req::CheckUpdate { product } = &req {
            send(Resp::Busy(Some(req.label())));
            send(Resp::UpdateCheck(osupdate::check_latest(product).map_err(|e| format!("{e:#}"))));
            send(Resp::Busy(None));
            continue;
        }
        let installing = matches!(req, Req::InstallOs(_));
        send(Resp::Busy(Some(req.label())));
        if calc.is_none() {
            match open_calc() {
                Ok((d, info)) => {
                    calc = Some(d);
                    send(Resp::Connected(info));
                }
                Err(e) => {
                    send(Resp::Error(format!("{e:#}")));
                    send(Resp::Busy(None));
                    continue;
                }
            }
        }
        let d = calc.as_mut().unwrap();
        let result = handle(d, req, &send);
        if installing {
            send(Resp::Progress(None));
            if result.is_ok() {
                // The calculator reboots into the new OS.
                calc = None;
                send(Resp::Disconnected);
            }
        }
        if let Err(e) = result {
            send(Resp::Error(format!("{e:#}")));
            // A refusal from the calculator leaves the link usable; anything else (USB error,
            // timeout, garbled packet) means we should start over next time.
            if e.downcast_ref::<dusb::CalcError>().is_none() {
                calc = None;
                send(Resp::Disconnected);
            }
        }
        send(Resp::Busy(None));
    }
}

fn handle(d: &mut Calc, req: Req, send: &dyn Fn(Resp)) -> Result<()> {
    match req {
        Req::Refresh => send(Resp::Dir(d.list_dir()?)),
        Req::Get { entries, dir } => {
            let mut n = 0;
            for e in entries {
                let t = entry_type(&e);
                if matches!(t.0, 0x23 | 0x24 | 0x25 | 0x27) {
                    send(Resp::Error(format!("{}: {} can't be received", entry_name(&e), t.name())));
                    continue;
                }
                let v = fetch(d, &e)?;
                let path = dir.join(format!("{}.{}", varfile::safe_file_stem(&entry_name(&e)), t.extension_for(v.data.len())));
                let f = VarFile { comment: COMMENT.into(), entries: vec![v] };
                std::fs::write(&path, f.to_bytes()?).with_context(|| format!("writing {}", path.display()))?;
                send(Resp::Info(format!("Saved {} → {}", entry_name(&e), path.display())));
                n += 1;
            }
            send(Resp::Info(format!("Received {n} variable(s) into {}", dir.display())));
        }
        Req::Open(e) => send(Resp::Opened(fetch(d, &e)?)),
        Req::SendFiles { files, archive } => {
            for f in files {
                let bytes = std::fs::read(&f).with_context(|| format!("reading {}", f.display()))?;
                let vf = match VarFile::parse(&bytes) {
                    Ok(v) => v,
                    Err(err) => {
                        send(Resp::Error(format!("{}: {err:#}", f.display())));
                        continue;
                    }
                };
                for e in vf.entries {
                    let arch = archive.unwrap_or(e.archived);
                    d.send_var(&varfile::link_name_from_file_name(e.var_type, &e.name), e.var_type.0, e.version, arch, &e.data)
                        .with_context(|| format!("sending {}", e.display_name()))?;
                    send(Resp::Info(format!("Sent {} ({})", e.display_name(), e.var_type.name())));
                }
            }
            send(Resp::Dir(d.list_dir()?));
        }
        Req::SendVar(e) => {
            d.send_var(&varfile::link_name_from_file_name(e.var_type, &e.name), e.var_type.0, e.version, e.archived, &e.data)?;
            send(Resp::Info(format!("Sent {} to the calculator", e.display_name())));
            send(Resp::Dir(d.list_dir()?));
        }
        Req::Delete(entries) => {
            for e in entries {
                d.delete_var(&e.name, entry_type(&e).0)?;
                send(Resp::Info(format!("Deleted {}", entry_name(&e))));
            }
            send(Resp::Dir(d.list_dir()?));
        }
        Req::Screenshot => {
            let data = d.param(dusb::PID_SCREENSHOT)?;
            send(Resp::Screen(screen::decode(&data, false)?));
        }
        Req::Backup(parent) => {
            let r = backup::backup(d, &parent, &mut |m| send(Resp::Busy(Some(m))))?;
            let mut msg = format!("Backed up {} variable(s) to {}", r.saved, r.folder.display());
            if !r.skipped.is_empty() {
                msg.push_str(&format!(" (flash apps not included: {})", r.skipped.join(", ")));
            }
            send(Resp::Info(msg));
        }
        Req::Restore(folder) => {
            let r = backup::restore(d, &folder, &mut |m| send(Resp::Busy(Some(m))))?;
            for f in &r.failed {
                send(Resp::Error(format!("Not restored: {f}")));
            }
            send(Resp::Info(format!("Restored {} variable(s), {} replaced", r.sent, r.replaced)));
            send(Resp::Dir(d.list_dir()?));
        }
        Req::SyncClock => {
            let t = backup::sync_clock(d)?;
            send(Resp::Info(format!("Calculator clock set to {}", t.format("%Y-%m-%d %H:%M:%S"))));
            send(Resp::InfoTable(info::read_info(d)?));
        }
        Req::InstallOs(path) => {
            let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let os = flash::OsFile::parse(&bytes)?;
            let (model, _, _) = info::identify(d)?;
            osupdate::check_compatible(&os, model)?;
            if let Ok(b) = d.param(dusb::PID_BATTERY) {
                anyhow::ensure!(b.first() != Some(&0), "the calculator's batteries are low; charge or replace them first");
            }
            osupdate::install(d, &os, &mut |f, m| {
                send(Resp::Progress(Some(f)));
                send(Resp::Busy(Some(m)));
            })?;
            send(Resp::OsInstalled);
        }
        Req::CheckUpdate { .. } | Req::Disconnect => unreachable!(),
    }
    Ok(())
}

fn fetch(d: &mut Calc, e: &DirEntry) -> Result<VarEntry> {
    let t = entry_type(e);
    let (attrs, data) = d.get_var(&e.name, t.0).with_context(|| format!("receiving {}", entry_name(e)))?;
    let version = dusb::find_attr(&attrs, dusb::AID_VAR_VERSION).and_then(|v| v.last().copied()).unwrap_or(0);
    Ok(VarEntry { var_type: t, name: varfile::file_name_from_link_name(t, &e.name), version, archived: e.archived(), data })
}

// ───────────────────────────── UI ─────────────────────────────

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Explorer,
    Screen,
    Editor,
    Simulator,
}

struct Editor {
    name: String,
    text: String,
    protected: bool,
    archived: bool,
    path: Option<PathBuf>,
    dirty: bool,
    filter: String,
    check: std::result::Result<usize, String>,
    /// Problems found by the checker.
    diags: Vec<Diag>,
    /// Source lines with a breakpoint (1-based).
    breakpoints: BTreeSet<usize>,
    right: RightTab,
    /// Simulate the CE's 26×10 home screen (else the TI-84 Plus's 16×8).
    screen_ce: bool,
    /// Move the cursor to this line (and focus the editor) next frame.
    goto_line: Option<usize>,
    /// Scroll this line into view next frame.
    scroll_line: Option<usize>,
    /// Open completion list.
    ac: Option<Completion>,
    /// Cursor position where the user dismissed the list (don't reopen there).
    ac_suppressed_at: Option<usize>,
    /// Token use counts in this program, for predictive ranking.
    usage: std::collections::HashMap<u16, usize>,
}

struct Completion {
    /// Char index where the typed word starts, and its length in chars.
    start: usize,
    len: usize,
    items: Vec<Suggestion>,
    sel: usize,
}

#[derive(PartialEq, Clone, Copy)]
enum RightTab {
    Catalog,
    Debugger,
}

/// A running debug session of the editor's program.
struct Debug {
    m: Machine,
    input: String,
    input_err: Option<String>,
    /// Keys go to getKey while the simulated screen has focus.
    screen_focus: bool,
    shown_line: Option<usize>,
}

impl Default for Editor {
    fn default() -> Self {
        Editor {
            name: "PROGRAM".into(),
            text: String::new(),
            protected: false,
            archived: false,
            path: None,
            dirty: false,
            filter: String::new(),
            check: Ok(0),
            diags: Vec::new(),
            breakpoints: BTreeSet::new(),
            right: RightTab::Catalog,
            screen_ce: true,
            goto_line: None,
            scroll_line: None,
            ac: None,
            ac_suppressed_at: None,
            usage: Default::default(),
        }
    }
}

impl Editor {
    fn recheck(&mut self) {
        self.check = tokens::tokenize(&self.text).map(|t| t.len()).map_err(|e| e.to_string());
        self.diags = check::check(&self.text);
        self.usage = suggest::usage(&self.text);
    }
    fn to_entry(&self) -> Result<VarEntry> {
        let mut e = varfile::program_from_text(&self.name, &self.text, self.protected)?;
        e.archived = self.archived;
        Ok(e)
    }
    fn load_entry(&mut self, e: &VarEntry, path: Option<PathBuf>) -> Result<()> {
        let body = varfile::token_body(&e.data)?;
        self.text = tokens::detokenize(body, false);
        self.name = e.display_name();
        self.protected = e.var_type == VarType::PROT_PROGRAM;
        self.archived = e.archived;
        self.path = path;
        self.dirty = false;
        self.recheck();
        Ok(())
    }
}

struct UpdateWindow {
    product: String,
    current: String,
    latest: Option<std::result::Result<osupdate::LatestOs, String>>,
}

struct App {
    tx: Sender<Req>,
    rx: Receiver<Resp>,
    tab: Tab,
    busy: Option<String>,
    connected: bool,
    info: Vec<(String, String)>,
    dir: Vec<DirEntry>,
    selected: BTreeSet<usize>,
    screen: Option<(Image, TextureHandle)>,
    zoom: f32,
    editor: Editor,
    log: Vec<(bool, String)>,
    show_log: bool,
    viewer: Option<(String, String)>,
    update: Option<UpdateWindow>,
    progress: Option<f32>,
    /// Probing a calculator while it verifies a new OS can knock it off the USB bus.
    os_installed_at: Option<std::time::Instant>,
    dbg: Option<Debug>,
    sim: sim::Sim,
    /// Keyboard goes to the simulator.
    sim_focus: bool,
    // scripted screenshot for docs/testing: CALLINK_GUI_SHOT=out.png [CALLINK_GUI_TAB=…]
    shot: Option<PathBuf>,
    frames: u64,
    shot_requested: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        setup_fonts(&cc.egui_ctx);
        let (req_tx, req_rx) = channel();
        let (resp_tx, resp_rx) = channel();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || worker(req_rx, resp_tx, ctx));
        let tab = match std::env::var("CALLINK_GUI_TAB").as_deref() {
            Ok("screen") => Tab::Screen,
            Ok("editor") | Ok("debug") | Ok("debug-error") | Ok("complete") | Ok("arghint") => Tab::Editor,
            Ok("sim") | Ok("sim-graph") | Ok("sim-prgm") | Ok("sim-menu") => Tab::Simulator,
            _ => Tab::Explorer,
        };
        let mut editor = Editor::default();
        editor.text = "ClrHome\nDisp \"Hello!\"\n".into();
        editor.name = "HELLO".into();
        editor.recheck();
        let mut app = App {
            tx: req_tx,
            rx: resp_rx,
            tab,
            busy: None,
            connected: false,
            info: vec![],
            dir: vec![],
            selected: BTreeSet::new(),
            screen: None,
            zoom: 2.0,
            editor,
            log: vec![],
            show_log: false,
            viewer: None,
            update: None,
            progress: None,
            os_installed_at: None,
            dbg: None,
            sim: sim::Sim::new(),
            sim_focus: true,
            shot: std::env::var_os("CALLINK_GUI_SHOT").map(PathBuf::from),
            frames: 0,
            shot_requested: false,
        };
        app.request(Req::Refresh);
        // scripted demos of the debugger for screenshots
        if app.shot.is_some() {
            let demo = match std::env::var("CALLINK_GUI_TAB").as_deref() {
                Ok("debug") => Some((DEMO_PROGRAM, Some(9), false)),
                Ok("debug-error") => Some((DEMO_PROGRAM, None, true)),
                _ => None,
            };
            if let Some((text, bp, to_end)) = demo {
                app.editor.text = text.into();
                app.editor.name = "PRIMES".into();
                app.editor.recheck();
                app.editor.breakpoints.extend(bp);
                app.start_debug(false);
                if to_end {
                    if let Some(d) = &mut app.dbg {
                        d.m.run_for(std::time::Duration::from_millis(200));
                    }
                }
            }
        }
        if tab == Tab::Screen {
            app.request(Req::Screenshot);
        }
        app
    }

    fn request(&self, r: Req) {
        let _ = self.tx.send(r);
    }

    fn note(&mut self, err: bool, msg: impl Into<String>) {
        self.log.push((err, msg.into()));
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(r) = self.rx.try_recv() {
            match r {
                Resp::Connected(info) => {
                    // simulate the connected model's home screen in the debugger
                    if let Some((_, m)) = info.iter().find(|(k, _)| k == "Model") {
                        let colour = m.contains(" CE") || m.contains("C Silver");
                        if self.editor.screen_ce != colour && self.dbg.is_none() {
                            self.editor.screen_ce = colour;
                        }
                    }
                    self.connected = true;
                    let name = info.iter().find(|i| i.0 == "Product name").map(|i| i.1.clone()).unwrap_or_default();
                    self.note(false, format!("Connected to {name}"));
                    self.info = info;
                    if self.shot.is_some() && std::env::var("CALLINK_GUI_TAB").as_deref() == Ok("update") {
                        let get = |k: &str| self.info.iter().find(|i| i.0 == k).map(|i| i.1.clone()).unwrap_or_default();
                        let product = get("Product name");
                        self.update = Some(UpdateWindow { product: product.clone(), current: get("OS version"), latest: None });
                        self.request(Req::CheckUpdate { product });
                    }
                }
                Resp::InfoTable(info) => self.info = info,
                Resp::UpdateCheck(r) => {
                    if let Some(u) = self.update.as_mut() {
                        u.latest = Some(r);
                    }
                }
                Resp::Progress(p) => self.progress = p,
                Resp::OsInstalled => {
                    self.os_installed_at = Some(std::time::Instant::now());
                    self.note(
                        false,
                        "OS installed. The calculator is verifying it; wait for its home screen (\"RAM Cleared\"), \
                         then unplug and replug the cable and press Connect.",
                    );
                }
                Resp::Disconnected => {
                    self.connected = false;
                    self.info.clear();
                    self.dir.clear();
                    self.selected.clear();
                }
                Resp::Dir(mut d) => {
                    d.sort_by_key(|e| (entry_type(e).0, entry_name(e)));
                    self.dir = d;
                    self.selected.clear();
                    if self.shot.is_some() && std::env::var("CALLINK_GUI_TAB").as_deref() == Ok("editor") {
                        if let Some(e) = self.dir.iter().find(|e| entry_name(e) == "HELLO") {
                            self.request(Req::Open(e.clone()));
                        }
                    }
                }
                Resp::Screen(img) => {
                    let ci = egui::ColorImage::from_rgb([img.width as usize, img.height as usize], &img.rgb);
                    let tex = ctx.load_texture("screen", ci, TextureOptions::NEAREST);
                    if img.width <= 96 && self.zoom < 4.0 {
                        self.zoom = 4.0; // 96×64 monochrome screens are tiny otherwise
                    }
                    self.screen = Some((img, tex));
                    self.note(false, "Screen captured");
                }
                Resp::Opened(e) => {
                    if matches!(e.var_type.0, 0x05 | 0x06) {
                        if self.confirm_discard() {
                            match self.editor.load_entry(&e, None) {
                                Ok(()) => {
                                    self.dbg = None;
                                    self.tab = Tab::Editor;
                                    self.note(false, format!("Opened {} from the calculator", e.display_name()));
                                }
                                Err(err) => self.note(true, format!("{err:#}")),
                            }
                        }
                    } else {
                        self.viewer = Some((
                            format!("{} ({})", e.display_name(), e.var_type.name()),
                            varfile::describe_contents(&e),
                        ));
                    }
                }
                Resp::Info(m) => self.note(false, m),
                Resp::Error(m) => self.note(true, m),
                Resp::Busy(b) => self.busy = b,
            }
        }
    }

    fn confirm_discard(&self) -> bool {
        !self.editor.dirty
            || rfd::MessageDialog::new()
                .set_title("Unsaved changes")
                .set_description(format!("Discard the changes to {}?", self.editor.name))
                .set_buttons(rfd::MessageButtons::YesNo)
                .show()
                == rfd::MessageDialogResult::Yes
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if dropped.is_empty() {
            return;
        }
        let (text, vars): (Vec<_>, Vec<_>) = dropped.into_iter().partition(|p| has_ext(p, &["txt"]));
        if !vars.is_empty() {
            self.request(Req::SendFiles { files: vars, archive: None });
        }
        if let Some(t) = text.first() {
            self.open_text_file(t);
        }
    }

    fn open_text_file(&mut self, p: &Path) {
        if !self.confirm_discard() {
            return;
        }
        match std::fs::read_to_string(p) {
            Ok(t) => {
                self.editor.text = t;
                self.editor.name = p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("PROGRAM")
                    .to_ascii_uppercase()
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == 'θ')
                    .take(8)
                    .collect();
                self.editor.path = Some(p.to_path_buf());
                self.editor.dirty = false;
                self.editor.recheck();
                self.tab = Tab::Editor;
            }
            Err(e) => self.note(true, format!("{}: {e}", p.display())),
        }
    }

    // ── panels ──

    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("CalLink");
                ui.separator();
                ui.selectable_value(&mut self.tab, Tab::Explorer, "🗂 Calculator Explorer");
                ui.selectable_value(&mut self.tab, Tab::Screen, "📷 Screen Capture");
                ui.selectable_value(&mut self.tab, Tab::Editor, "✏ Program Editor");
                ui.selectable_value(&mut self.tab, Tab::Simulator, "🖩 Simulator");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.connected {
                        if ui.button("Disconnect").clicked() {
                            self.request(Req::Disconnect);
                        }
                        ui.label(RichText::new("● Connected").color(Color32::from_rgb(60, 170, 90)));
                    } else {
                        let settling = self.os_installed_at.map(|t| t.elapsed().as_secs()).filter(|&s| s < OS_SETTLE_SECS);
                        if let Some(s) = settling {
                            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
                            ui.label(RichText::new(format!("calculator starting new OS… {}s", OS_SETTLE_SECS - s)).weak());
                        }
                        if ui.add_enabled(self.busy.is_none() && settling.is_none(), egui::Button::new("Connect")).clicked() {
                            self.request(Req::Refresh);
                        }
                        ui.label(RichText::new("● Not connected").color(Color32::GRAY));
                    }
                });
            });
            ui.add_space(2.0);
        });
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(b) = &self.busy {
                    ui.add(egui::Spinner::new());
                    if let Some(p) = self.progress {
                        ui.add(egui::ProgressBar::new(p).desired_width(220.0).show_percentage());
                    }
                    ui.label(b);
                } else if let Some((err, m)) = self.log.last() {
                    let c = if *err { Color32::from_rgb(220, 80, 70) } else { ui.visuals().text_color() };
                    ui.label(RichText::new(m).color(c));
                } else {
                    ui.label("Ready");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.toggle_value(&mut self.show_log, "Log");
                });
            });
        });
    }

    fn explorer(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("info").default_width(250.0).show(ctx, |ui| {
            ui.add_space(6.0);
            ui.strong("Calculator");
            ui.add_space(4.0);
            if self.info.is_empty() {
                ui.label(if self.busy.is_some() { "Connecting…" } else { "No calculator connected.\n\nPlug in a TI-84 Plus CE and press Connect." });
            } else {
                egui::Grid::new("info").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
                    for (k, v) in &self.info {
                        ui.label(RichText::new(k).weak());
                        ui.label(v);
                        ui.end_row();
                    }
                });
            }
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(4.0);
            let idle = self.busy.is_none();
            let w = ui.available_width();
            let btn = |t: &str| egui::Button::new(t).min_size(egui::vec2(w, 26.0));
            if ui.add_enabled(idle, btn("💾 Back up calculator…")).on_hover_text("Save every variable into a new dated folder").clicked() {
                if let Some(dir) = rfd::FileDialog::new().set_title("Choose where to put the backup folder").pick_folder() {
                    self.request(Req::Backup(dir));
                }
            }
            if ui.add_enabled(idle, btn("♻ Restore backup…")).on_hover_text("Send every variable in a backup folder back to the calculator").clicked() {
                if let Some(dir) = rfd::FileDialog::new().set_title("Choose a CalLink backup folder").pick_folder() {
                    match backup::backup_files(&dir) {
                        Ok(files) if !files.is_empty() => {
                            let ok = rfd::MessageDialog::new()
                                .set_title("Restore backup")
                                .set_description(format!(
                                    "Send {} variable(s) from \"{}\" to the calculator?\n\nVariables on the calculator with the same names will be replaced.",
                                    files.len(),
                                    dir.file_name().and_then(|n| n.to_str()).unwrap_or("backup")
                                ))
                                .set_buttons(rfd::MessageButtons::YesNo)
                                .show()
                                == rfd::MessageDialogResult::Yes;
                            if ok {
                                self.request(Req::Restore(dir));
                            }
                        }
                        Ok(_) => self.note(true, format!("No calculator variable files in {}", dir.display())),
                        Err(e) => self.note(true, format!("{e:#}")),
                    }
                }
            }
            if ui
                .add_enabled(idle, btn("🕒 Sync date & time"))
                .on_hover_text(format!("Set the calculator clock to {}", chrono::Local::now().format("%Y-%m-%d %H:%M")))
                .clicked()
            {
                self.request(Req::SyncClock);
            }
            if ui
                .add_enabled(idle && self.connected, btn("⬆ OS update…"))
                .on_hover_text("Look for a newer calculator OS and install it")
                .clicked()
            {
                let get = |k: &str| self.info.iter().find(|i| i.0 == k).map(|i| i.1.clone()).unwrap_or_default();
                let product = get("Product name");
                self.update = Some(UpdateWindow { product: product.clone(), current: get("OS version"), latest: None });
                self.request(Req::CheckUpdate { product });
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let idle = self.busy.is_none();
            let sel: Vec<DirEntry> = self.selected.iter().filter_map(|&i| self.dir.get(i).cloned()).collect();
            ui.horizontal(|ui| {
                if ui.add_enabled(idle, egui::Button::new("⟳ Refresh")).clicked() {
                    self.request(Req::Refresh);
                }
                if ui.add_enabled(idle, egui::Button::new("⬆ Send files…")).on_hover_text("Or drop .8x* files on the window").clicked() {
                    if let Some(files) = rfd::FileDialog::new()
                        .add_filter("TI variables", &["8xp", "8xv", "8xl", "8xm", "8xs", "8xn", "8xy", "8xc", "8ca", "8ci", "8xi", "8xg", "8xd", "8xw", "8xz", "8xt"])
                        .add_filter("All files", &["*"])
                        .pick_files()
                    {
                        self.request(Req::SendFiles { files, archive: None });
                    }
                }
                if ui.add_enabled(idle && !sel.is_empty(), egui::Button::new("⬇ Receive…")).clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_title("Save variables to folder").pick_folder() {
                        self.request(Req::Get { entries: sel.clone(), dir });
                    }
                }
                let one = (sel.len() == 1).then(|| sel[0].clone());
                let openable = one.as_ref().is_some_and(|e| !matches!(entry_type(e).0, 0x23 | 0x24 | 0x25 | 0x27));
                let is_prog = one.as_ref().is_some_and(|e| matches!(entry_type(e).0, 0x05 | 0x06));
                if ui
                    .add_enabled(idle && openable, egui::Button::new(if is_prog { "✏ Edit" } else { "🔍 View" }))
                    .clicked()
                {
                    self.request(Req::Open(one.clone().unwrap()));
                }
                if ui.add_enabled(idle && !sel.is_empty(), egui::Button::new("🗑 Delete")).clicked() {
                    let names: Vec<String> = sel.iter().map(entry_name).collect();
                    let ok = rfd::MessageDialog::new()
                        .set_title("Delete from calculator")
                        .set_description(format!("Permanently delete {} from the calculator?", names.join(", ")))
                        .set_buttons(rfd::MessageButtons::YesNo)
                        .show()
                        == rfd::MessageDialogResult::Yes;
                    if ok {
                        self.request(Req::Delete(sel.clone()));
                    }
                }
            });
            ui.separator();

            if self.dir.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| ui.label(RichText::new("No variables to show").weak()));
                return;
            }
            let all = self.selected.len() == self.dir.len();
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                egui::Grid::new("vars").num_columns(5).striped(true).spacing([18.0, 6.0]).show(ui, |ui| {
                    let mut all_box = all;
                    if ui.checkbox(&mut all_box, "").changed() {
                        self.selected = if all_box { (0..self.dir.len()).collect() } else { BTreeSet::new() };
                    }
                    ui.strong("Name");
                    ui.strong("Type");
                    ui.strong("Size");
                    ui.strong("Location");
                    ui.end_row();
                    let mut open = None;
                    for (i, e) in self.dir.iter().enumerate() {
                        let mut on = self.selected.contains(&i);
                        if ui.checkbox(&mut on, "").changed() {
                            if on { self.selected.insert(i); } else { self.selected.remove(&i); }
                        }
                        let r = ui.add(egui::SelectableLabel::new(on, RichText::new(entry_name(e)).monospace()));
                        if r.clicked() {
                            if !ui.input(|i| i.modifiers.command) {
                                self.selected.clear();
                            }
                            self.selected.insert(i);
                        }
                        if r.double_clicked() {
                            open = Some(e.clone());
                        }
                        ui.label(entry_type(e).name());
                        ui.label(e.size().map_or("?".into(), human_size));
                        ui.label(if e.archived() { "Archive" } else { "RAM" });
                        ui.end_row();
                    }
                    if let Some(e) = open {
                        if idle {
                            self.request(Req::Open(e));
                        }
                    }
                });
            });
        });
    }

    fn screen_tab(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.add_enabled(self.busy.is_none(), egui::Button::new("📷 Take screenshot")).clicked() {
                    self.request(Req::Screenshot);
                }
                if ui.add_enabled(self.screen.is_some(), egui::Button::new("💾 Save PNG…")).clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("PNG image", &["png"]).set_file_name("screenshot.png").save_file() {
                        let (img, _) = self.screen.as_ref().unwrap();
                        let res = std::fs::File::create(&p)
                            .map_err(anyhow::Error::from)
                            .and_then(|f| screen::write_png(img, std::io::BufWriter::new(f), self.zoom.round() as u32));
                        match res {
                            Ok(()) => self.note(false, format!("Saved {}", p.display())),
                            Err(e) => self.note(true, format!("{e:#}")),
                        }
                    }
                }
                ui.separator();
                ui.label("Zoom");
                ui.add(egui::Slider::new(&mut self.zoom, 1.0..=4.0).step_by(1.0).suffix("×"));
            });
            ui.separator();
            egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| match &self.screen {
                Some((img, tex)) => {
                    let size = egui::vec2(img.width as f32 * self.zoom, img.height as f32 * self.zoom);
                    ui.add(egui::Image::new((tex.id(), size)));
                }
                None => {
                    ui.add_space(20.0);
                    ui.label(RichText::new("No screenshot yet").weak());
                }
            });
        });
    }

    fn start_debug(&mut self, stepping: bool) {
        let (cols, rows) = if self.editor.screen_ce { (26, 10) } else { (16, 8) };
        match Machine::new(&self.editor.text, cols, rows) {
            Ok(mut m) => {
                m.breakpoints = self.editor.breakpoints.clone();
                if !stepping {
                    m.resume();
                }
                self.dbg = Some(Debug { m, input: String::new(), input_err: None, screen_focus: true, shown_line: None });
                self.editor.right = RightTab::Debugger;
            }
            Err(e) => self.note(true, format!("Can't run the program: {e}")),
        }
    }

    fn dbg_run(&mut self) {
        match &mut self.dbg {
            Some(d) if !d.m.is_finished() => d.m.resume(),
            _ => self.start_debug(false),
        }
    }

    fn dbg_step(&mut self) {
        match &mut self.dbg {
            Some(d) if !d.m.is_finished() => d.m.step(),
            _ => self.start_debug(true),
        }
    }

    /// Advance a running program, feed keys to getKey, and follow the current line.
    fn tick_debugger(&mut self, ctx: &egui::Context) {
        if self.tab == Tab::Editor {
            let (f5, f10, stop) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::F5) && !i.modifiers.shift,
                    i.key_pressed(egui::Key::F10),
                    i.key_pressed(egui::Key::F5) && i.modifiers.shift,
                )
            });
            if stop {
                self.dbg = None;
            } else if f5 {
                self.dbg_run();
            } else if f10 {
                self.dbg_step();
            }
        }
        let Some(d) = &mut self.dbg else { return };
        d.m.breakpoints = self.editor.breakpoints.clone();
        if d.screen_focus && self.tab == Tab::Editor {
            let keys: Vec<egui::Key> = ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Key { key, pressed: true, .. } => Some(*key),
                        _ => None,
                    })
                    .collect()
            });
            for k in keys {
                if d.m.state == DbgState::PauseCmd && k == egui::Key::Enter {
                    d.m.enter();
                } else if let Some(code) = basic_interp::key_code(if k == egui::Key::Space { " " } else { k.name() }) {
                    d.m.press_key(code);
                }
            }
        }
        if d.m.state == DbgState::Running {
            d.m.run_for(std::time::Duration::from_millis(12));
            ctx.request_repaint();
        }
        if !matches!(d.m.state, DbgState::Running) {
            let line = match &d.m.state {
                DbgState::Error { line, .. } => Some(*line),
                _ => d.m.current_line(),
            };
            if line != d.shown_line {
                d.shown_line = line;
                self.editor.scroll_line = line;
            }
        }
    }

    fn editor_tab(&mut self, ctx: &egui::Context) {
        let text_id = egui::Id::new("program-text");
        let idle = self.busy.is_none();

        egui::TopBottomPanel::top("editor-tools").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("New").clicked() && self.confirm_discard() {
                    self.editor = Editor { filter: std::mem::take(&mut self.editor.filter), ..Editor::default() };
                    self.dbg = None;
                }
                if ui.button("Open…").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("Programs", &["8xp", "txt"])
                        .add_filter("All files", &["*"])
                        .pick_file()
                    {
                        if has_ext(&p, &["txt"]) {
                            self.open_text_file(&p);
                        } else if self.confirm_discard() {
                            let r = std::fs::read(&p).map_err(anyhow::Error::from).and_then(|b| {
                                let vf = VarFile::parse(&b)?;
                                let e = vf.entries.into_iter().find(|e| matches!(e.var_type.0, 0x05 | 0x06)).context("file contains no program")?;
                                self.editor.load_entry(&e, Some(p.clone()))
                            });
                            if let Err(e) = r {
                                self.note(true, format!("{}: {e:#}", p.display()));
                            }
                        }
                        self.dbg = None;
                    }
                }
                if ui.button("Save").clicked() {
                    match self.editor.path.clone() {
                        Some(p) => self.save_to(&p),
                        None => self.save_as(),
                    }
                }
                if ui.button("Save as…").clicked() {
                    self.save_as();
                }
                ui.separator();
                if ui.add_enabled(idle && self.editor.check.is_ok(), egui::Button::new("⬆ Send to calculator")).clicked() {
                    match self.editor.to_entry() {
                        Ok(e) => self.request(Req::SendVar(e)),
                        Err(e) => self.note(true, format!("{e:#}")),
                    }
                }
                ui.separator();
                // debugger controls
                let state = self.dbg.as_ref().map(|d| d.m.state.clone());
                let live = state.as_ref().is_some_and(|s| !matches!(s, DbgState::Done | DbgState::Error { .. }));
                let paused = matches!(state, Some(DbgState::Paused));
                let running = matches!(state, Some(DbgState::Running));
                let can_run = self.editor.check.is_ok() && (!live || paused);
                let run_label = if paused { "▶ Continue" } else { "▶ Run" };
                if ui.add_enabled(can_run, egui::Button::new(run_label)).on_hover_text("Run in the debugger (F5)").clicked() {
                    self.dbg_run();
                }
                if ui.add_enabled(self.editor.check.is_ok() && (!live || paused), egui::Button::new("⏭ Step")).on_hover_text("Run one statement (F10)").clicked() {
                    self.dbg_step();
                }
                if ui.add_enabled(running, egui::Button::new("⏸ Pause")).clicked() {
                    if let Some(d) = &mut self.dbg {
                        d.m.pause();
                    }
                }
                if ui.add_enabled(self.dbg.is_some(), egui::Button::new("⏹ Stop")).on_hover_text("End the debug session (Shift+F5)").clicked() {
                    self.dbg = None;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Name");
                let r = ui.add(egui::TextEdit::singleline(&mut self.editor.name).desired_width(90.0).char_limit(8).font(egui::TextStyle::Monospace));
                if r.changed() {
                    self.editor.name = self.editor.name.to_uppercase();
                    self.editor.dirty = true;
                }
                if varfile::program_name_bytes(&self.editor.name).is_err() {
                    ui.label(RichText::new("⚠ 1–8 of A–Z, 0–9, θ; no leading digit").color(Color32::from_rgb(220, 150, 40)));
                }
                ui.checkbox(&mut self.editor.protected, "Protected");
                ui.checkbox(&mut self.editor.archived, "Archive");
                if let Some(p) = &self.editor.path {
                    ui.label(RichText::new(p.display().to_string()).weak());
                }
                if self.editor.dirty {
                    ui.label(RichText::new("● modified").weak());
                }
            });
            ui.add_space(2.0);
        });

        self.problems_panel(ctx);

        let mut insert: Option<String> = None;
        egui::SidePanel::right("catalog").default_width(290.0).show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.editor.right, RightTab::Catalog, "Catalog");
                let dbg_label = match self.dbg.as_ref().map(|d| &d.m.state) {
                    Some(DbgState::Running) => "Debugger ▶",
                    Some(DbgState::Error { .. }) => "Debugger ✖",
                    _ => "Debugger",
                };
                ui.selectable_value(&mut self.editor.right, RightTab::Debugger, dbg_label);
            });
            ui.separator();
            match self.editor.right {
                RightTab::Catalog => insert = self.catalog(ui),
                RightTab::Debugger => self.debugger_panel(ui),
            }
        });

        let mut changed = false;
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                changed = self.code_view(ui, text_id);
            });
        });
        if changed {
            self.editor.dirty = true;
            self.editor.recheck();
            if self.dbg.take().is_some() {
                self.note(false, "Debugging stopped because the program was edited");
            }
        }

        if let Some(s) = insert {
            let text = &mut self.editor.text;
            let n = text.chars().count();
            let mut state = egui::TextEdit::load_state(ctx, text_id).unwrap_or_default();
            let at = state.cursor.char_range().map_or(n, |r| r.primary.index.min(n));
            let byte = text.char_indices().nth(at).map_or(text.len(), |(b, _)| b);
            text.insert_str(byte, &s);
            state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(at + s.chars().count()))));
            state.store(ctx, text_id);
            ctx.memory_mut(|m| m.request_focus(text_id));
            self.editor.dirty = true;
            self.editor.recheck();
            self.dbg = None;
        }
    }

    /// Line-number/breakpoint gutter plus the highlighted program text. Returns whether the text changed.
    fn code_view(&mut self, ui: &mut egui::Ui, text_id: egui::Id) -> bool {
        const GUTTER: f32 = 52.0;
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let cur_line = self.dbg.as_ref().and_then(|d| match &d.m.state {
            DbgState::Running | DbgState::Done => None,
            DbgState::Error { line, .. } => Some(*line),
            _ => d.m.current_line(),
        });
        let error_lines: BTreeSet<usize> = self.editor.diags.iter().filter(|d| d.level == Level::Error).map(|d| d.line).collect();
        let text_color = ui.visuals().text_color();
        let dark = ui.visuals().dark_mode;
        let cur_bg = if dark { Color32::from_rgba_unmultiplied(230, 190, 40, 60) } else { Color32::from_rgba_unmultiplied(250, 210, 60, 120) };
        let err_bg = Color32::from_rgba_unmultiplied(220, 60, 60, if dark { 45 } else { 40 });
        let font2 = font.clone();
        let mut layouter = move |ui: &egui::Ui, text: &str, _wrap: f32| {
            let mut job = egui::text::LayoutJob::default();
            for (i, line) in text.split_inclusive('\n').enumerate() {
                let n = i + 1;
                let background = if Some(n) == cur_line {
                    cur_bg
                } else if error_lines.contains(&n) {
                    err_bg
                } else {
                    Color32::TRANSPARENT
                };
                job.append(line, 0.0, egui::TextFormat { font_id: font2.clone(), color: text_color, background, ..Default::default() });
            }
            job.wrap.max_width = f32::INFINITY;
            ui.fonts(|f| f.layout_job(job))
        };

        // While the completion list is open, it owns ↑ ↓ Tab Enter Esc.
        let mut accept: Option<usize> = None;
        if let Some(ac) = &mut self.editor.ac {
            let (down, up, enter, esc) = ui.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Tab) || i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                )
            });
            if down {
                ac.sel = (ac.sel + 1) % ac.items.len();
            }
            if up {
                ac.sel = (ac.sel + ac.items.len() - 1) % ac.items.len();
            }
            if enter {
                accept = Some(ac.sel);
            }
            if esc {
                self.editor.ac_suppressed_at = Some(ac.start + ac.len);
                self.editor.ac = None;
            }
        }

        let mut changed = false;
        ui.horizontal_top(|ui| {
            let left = ui.cursor().min.x;
            ui.add_space(GUTTER);
            let out = egui::TextEdit::multiline(&mut self.editor.text)
                .id(text_id)
                .font(egui::TextStyle::Monospace)
                .code_editor()
                .desired_width(f32::INFINITY)
                .desired_rows(30)
                .layouter(&mut layouter)
                .show(ui);
            changed = out.response.changed();
            if self.completion(ui, &out, changed, accept, text_id).is_some() {
                changed = true;
            }
            if out.response.has_focus() {
                if let Some(d) = &mut self.dbg {
                    d.screen_focus = false;
                }
            }
            let galley = out.galley.clone();
            let origin = out.galley_pos;
            let gutter = egui::Rect::from_min_max(egui::pos2(left, out.response.rect.top()), egui::pos2(left + GUTTER - 6.0, out.response.rect.bottom()));
            let resp = ui.interact(gutter, ui.id().with("gutter"), egui::Sense::click()).on_hover_text("Click a line number to set or clear a breakpoint");
            // row positions, one per source line (no wrapping)
            let mut rows: Vec<(usize, egui::Rect)> = Vec::new();
            let mut line = 1;
            for row in &galley.rows {
                let r = row.rect.translate(origin.to_vec2());
                if rows.last().is_none_or(|(l, _)| *l != line) {
                    rows.push((line, r));
                }
                if row.ends_with_newline {
                    line += 1;
                }
            }
            if self.editor.text.ends_with('\n') || self.editor.text.is_empty() {
                // the empty last line has its own row
                if let Some(row) = galley.rows.last() {
                    rows.push((line, row.rect.translate(origin.to_vec2())));
                }
            }
            if resp.clicked() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    if let Some((l, _)) = rows.iter().find(|(_, r)| pos.y >= r.top() && pos.y < r.bottom()) {
                        if !self.editor.breakpoints.remove(l) {
                            self.editor.breakpoints.insert(*l);
                        }
                    }
                }
            }
            let painter = ui.painter_at(gutter.expand2(egui::vec2(0.0, 2.0)));
            let small = egui::FontId::monospace(font.size * 0.85);
            for (l, r) in &rows {
                let y = r.center().y;
                let has_err = self.editor.diags.iter().any(|d| d.line == *l && d.level == Level::Error);
                let num_color = if has_err { Color32::from_rgb(230, 90, 80) } else { ui.visuals().weak_text_color() };
                painter.text(egui::pos2(gutter.right() - 2.0, y), egui::Align2::RIGHT_CENTER, l.to_string(), small.clone(), num_color);
                if self.editor.breakpoints.contains(l) {
                    painter.circle_filled(egui::pos2(gutter.left() + 8.0, y), 5.0, Color32::from_rgb(220, 50, 50));
                }
                if Some(*l) == cur_line {
                    painter.text(egui::pos2(gutter.left() + 18.0, y), egui::Align2::CENTER_CENTER, "▶", small.clone(), Color32::from_rgb(240, 190, 30));
                }
            }
            // jump / scroll requests
            if let Some(l) = self.editor.scroll_line.take() {
                if let Some((_, r)) = rows.iter().find(|(x, _)| *x == l) {
                    ui.scroll_to_rect(r.expand(30.0), None);
                }
            }
            if let Some(l) = self.editor.goto_line.take() {
                let idx: usize = self.editor.text.split_inclusive('\n').take(l.saturating_sub(1)).map(|s| s.chars().count()).sum();
                let mut st = out.state.clone();
                st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(idx))));
                st.store(ui.ctx(), text_id);
                ui.ctx().memory_mut(|m| m.request_focus(text_id));
                if let Some((_, r)) = rows.iter().find(|(x, _)| *x == l) {
                    ui.scroll_to_rect(r.expand(30.0), None);
                }
            }
        });
        changed
    }

    /// Update, draw and apply the completion list and the argument hint.
    /// Returns Some(()) when a suggestion was inserted (the text changed).
    fn completion(
        &mut self,
        ui: &mut egui::Ui,
        out: &egui::text_edit::TextEditOutput,
        typed: bool,
        mut accept: Option<usize>,
        text_id: egui::Id,
    ) -> Option<()> {
        let focused = out.response.has_focus();
        let Some(range) = out.cursor_range else {
            self.editor.ac = None;
            return None;
        };
        let cursor = range.primary.ccursor.index;
        let chars: Vec<char> = self.editor.text.chars().collect();
        let cursor = cursor.min(chars.len());
        let line_start = chars[..cursor].iter().rposition(|&c| c == '\n').map_or(0, |i| i + 1);
        let line_end = chars[cursor..].iter().position(|&c| c == '\n').map_or(chars.len(), |i| cursor + i);
        let before: String = chars[line_start..cursor].iter().collect();
        let after: String = chars[cursor..line_end].iter().collect();

        if !focused {
            self.editor.ac = None;
        } else if typed {
            self.editor.ac = None;
            if self.editor.ac_suppressed_at != Some(cursor) {
                self.editor.ac_suppressed_at = None;
                if let Some((n, word)) = suggest::word_before(&before, &after) {
                    let items = suggest::suggest(&word, suggest::at_statement_start(&before, n), &self.editor.usage, 8);
                    if !items.is_empty() {
                        self.editor.ac = Some(Completion { start: cursor - n, len: n, items, sel: 0 });
                    }
                }
            }
        } else if self.editor.ac.as_ref().is_some_and(|ac| ac.start + ac.len != cursor) {
            // the cursor moved away
            self.editor.ac = None;
        }

        let anchor = out.galley_pos + out.galley.pos_from_cursor(&range.primary).left_bottom().to_vec2() + egui::vec2(0.0, 3.0);
        if let Some(ac) = &self.editor.ac {
            egui::Area::new(egui::Id::new("completion"))
                .fixed_pos(anchor)
                .order(egui::Order::Foreground)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_min_width(300.0);
                        ui.set_max_width(460.0);
                        for (i, s) in ac.items.iter().enumerate() {
                            let r = ui.add(egui::SelectableLabel::new(i == ac.sel, RichText::new(&s.label).monospace()));
                            if r.clicked() {
                                accept = Some(i);
                            }
                            if i == ac.sel {
                                if let Some(h) = s.hint {
                                    ui.add(egui::Label::new(RichText::new(h).weak().small()).wrap());
                                }
                            }
                        }
                        ui.separator();
                        ui.label(RichText::new("↑↓ choose · Tab/Enter insert · Esc close").weak().small());
                    });
                });
        } else if focused {
            if let Some((hint, arg)) = suggest::call_hint(&before) {
                let (pre, cur, post) = suggest::split_hint(hint, arg);
                egui::Area::new(egui::Id::new("arg-hint"))
                    .fixed_pos(anchor)
                    .order(egui::Order::Foreground)
                    .interactable(false)
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                ui.label(RichText::new(pre).monospace().weak());
                                ui.label(RichText::new(cur).monospace().strong().underline());
                                ui.label(RichText::new(post).monospace().weak());
                            });
                        });
                    });
            }
        }

        let i = accept?;
        let ac = self.editor.ac.take()?;
        let s = ac.items.get(i)?;
        let start_b = self.editor.text.char_indices().nth(ac.start).map_or(self.editor.text.len(), |(b, _)| b);
        let end_b = self.editor.text.char_indices().nth(ac.start + ac.len).map_or(self.editor.text.len(), |(b, _)| b);
        self.editor.text.replace_range(start_b..end_b, &s.insert);
        let mut st = out.state.clone();
        st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(ac.start + s.insert.chars().count()))));
        st.store(ui.ctx(), text_id);
        ui.ctx().memory_mut(|m| m.request_focus(text_id));
        Some(())
    }

    fn problems_panel(&mut self, ctx: &egui::Context) {
        let errors = self.editor.diags.iter().filter(|d| d.level == Level::Error).count();
        let warnings = self.editor.diags.iter().filter(|d| d.level == Level::Warning).count();
        egui::TopBottomPanel::bottom("editor-problems").resizable(true).default_height(110.0).min_height(32.0).show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.strong("Problems");
                let summary = match (errors, warnings) {
                    (0, 0) => RichText::new("✔ none found").color(Color32::from_rgb(60, 170, 90)),
                    (e, w) => {
                        let mut parts = Vec::new();
                        if e > 0 {
                            parts.push(format!("{e} error{}", if e == 1 { "" } else { "s" }));
                        }
                        if w > 0 {
                            parts.push(format!("{w} warning{}", if w == 1 { "" } else { "s" }));
                        }
                        RichText::new(parts.join(", ")).color(if e > 0 { Color32::from_rgb(220, 80, 70) } else { Color32::from_rgb(220, 160, 40) })
                    }
                };
                ui.label(summary);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let info = match &self.editor.check {
                        Ok(n) => format!("{n} bytes · {} lines", self.editor.text.lines().count()),
                        Err(_) => "can't tokenize".into(),
                    };
                    ui.label(RichText::new(info).weak());
                });
            });
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                let mut jump = None;
                for d in &self.editor.diags {
                    let (icon, color) = match d.level {
                        Level::Error => ("✖", Color32::from_rgb(220, 80, 70)),
                        Level::Warning => ("⚠", Color32::from_rgb(220, 160, 40)),
                        Level::Info => ("ℹ", ui.visuals().weak_text_color()),
                    };
                    let r = ui.add(
                        egui::Label::new(RichText::new(format!("{icon} Line {}: {}", d.line, d.msg)).color(color)).sense(egui::Sense::click()),
                    );
                    if r.on_hover_text("Go to this line").clicked() {
                        jump = Some(d.line);
                    }
                }
                if let Some(l) = jump {
                    self.editor.goto_line = Some(l);
                }
            });
        });
    }

    fn catalog(&mut self, ui: &mut egui::Ui) -> Option<String> {
        let mut insert = None;
        ui.add(egui::TextEdit::singleline(&mut self.editor.filter).hint_text("Search tokens…"));
        ui.add_space(4.0);
        let f = self.editor.filter.to_lowercase();
        let mut items: Vec<(u16, &str)> = tokens::TokenTable::get()
            .all()
            .into_iter()
            .filter(|(c, t)| *c != 0x3F && (f.is_empty() || t.to_lowercase().contains(&f)))
            .collect();
        // Words first, alphabetically; symbols after.
        items.sort_by_key(|(_, t)| (!t.starts_with(|c: char| c.is_alphabetic()), t.to_lowercase()));
        let row_h = ui.spacing().interact_size.y;
        egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, row_h, items.len(), |ui, range| {
            for (code, t) in &items[range] {
                let hex = if *code > 0xFF { format!("{code:04X}") } else { format!("{code:02X}") };
                let r = ui
                    .horizontal(|ui| {
                        let b = ui.add(egui::Button::new(RichText::new(t.trim_end().to_string()).monospace()).frame(false));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(hex).weak().small().monospace());
                        });
                        b
                    })
                    .inner
                    .on_hover_text("Click to insert at the cursor");
                if r.clicked() {
                    insert = Some(t.to_string());
                }
            }
        });
        insert
    }

    fn debugger_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Screen").weak());
            let before = self.editor.screen_ce;
            ui.selectable_value(&mut self.editor.screen_ce, true, "CE 26×10");
            ui.selectable_value(&mut self.editor.screen_ce, false, "84+ 16×8");
            if before != self.editor.screen_ce {
                self.dbg = None;
            }
        });
        ui.add_space(4.0);
        let Some(d) = &mut self.dbg else {
            ui.add_space(8.0);
            ui.label("Press ▶ Run (F5) to run the program here, or ⏭ Step (F10) to go one statement at a time.");
            ui.add_space(6.0);
            ui.label(RichText::new("Click a line number in the editor to set a breakpoint.").weak());
            ui.add_space(6.0);
            ui.label(
                RichText::new(
                    "The debugger runs TI-BASIC itself: numbers, strings, lists, Disp/Output(/Input/Prompt/Pause/Menu(, \
                     getKey, If/For(/While/Repeat, Goto/Lbl and common math. Graph-screen commands are skipped.",
                )
                .weak()
                .small(),
            );
            return;
        };

        // status
        let (msg, color) = match &d.m.state {
            DbgState::Paused => {
                let l = d.m.current_line().unwrap_or(0);
                let why = if self.editor.breakpoints.contains(&l) { " (breakpoint)" } else { "" };
                (format!("⏸ Paused before line {l}{why}"), Color32::from_rgb(230, 180, 40))
            }
            DbgState::Running => (format!("▶ Running… {} statements", d.m.steps), Color32::from_rgb(70, 160, 230)),
            DbgState::Input { .. } => ("⌨ Waiting for input".into(), Color32::from_rgb(70, 160, 230)),
            DbgState::PauseCmd => ("⏸ Pause: press Enter to continue".into(), Color32::from_rgb(70, 160, 230)),
            DbgState::Menu { .. } => ("☰ Waiting for a menu choice".into(), Color32::from_rgb(70, 160, 230)),
            DbgState::Done => (format!("✔ Finished after {} statements", d.m.steps), Color32::from_rgb(60, 170, 90)),
            DbgState::Error { line, msg } => (format!("✖ Line {line}: {msg}"), Color32::from_rgb(230, 90, 80)),
        };
        ui.add(egui::Label::new(RichText::new(msg).color(color).strong()).wrap());
        ui.add_space(6.0);

        // simulated home screen
        let lcd = Color32::from_rgb(182, 196, 168);
        let ink = Color32::from_rgb(28, 36, 40);
        let focus = d.screen_focus;
        let frame = egui::Frame::none()
            .fill(lcd)
            .inner_margin(egui::Margin::same(6.0))
            .rounding(4.0)
            .stroke(if focus { egui::Stroke::new(2.0_f32, Color32::from_rgb(70, 160, 230)) } else { egui::Stroke::new(1.0_f32, Color32::from_gray(90)) })
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let size = if self.editor.screen_ce { 12.5 } else { 15.0 };
                for row in &d.m.screen.cells {
                    let s: String = row.iter().collect();
                    ui.label(RichText::new(s).monospace().size(size).color(ink));
                }
            });
        let screen_resp = frame.response.interact(egui::Sense::click());
        if screen_resp.clicked() {
            d.screen_focus = true;
            ui.ctx().memory_mut(|m| m.surrender_focus(egui::Id::new("program-text")));
        }
        ui.label(
            RichText::new(if focus { "Keys go to getKey (Esc = CLEAR, arrows, Enter, digits, letters)" } else { "Click the screen to send keys to getKey" })
                .weak()
                .small(),
        );
        ui.add_space(6.0);

        // interaction for waiting states
        match d.m.state.clone() {
            DbgState::Input { prompt, .. } => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(prompt).monospace());
                    let r = ui.add(egui::TextEdit::singleline(&mut d.input).desired_width(110.0).font(egui::TextStyle::Monospace));
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if !r.has_focus() && d.input.is_empty() {
                        r.request_focus();
                    }
                    if ui.button("Enter").clicked() || enter {
                        match d.m.give_input(&d.input) {
                            Ok(()) => {
                                d.input.clear();
                                d.input_err = None;
                            }
                            Err(e) => d.input_err = Some(e),
                        }
                    }
                });
                if let Some(e) = &d.input_err {
                    ui.label(RichText::new(e).color(Color32::from_rgb(230, 90, 80)).small());
                }
                ui.label(RichText::new("Type a value or expression (use ⁻ for negative numbers).").weak().small());
            }
            DbgState::PauseCmd => {
                if ui.button("Continue (Enter)").clicked() {
                    d.m.enter();
                }
            }
            DbgState::Menu { title, items } => {
                ui.strong(title);
                for (i, (label, _)) in items.iter().enumerate() {
                    if ui.button(format!("{}: {label}", i + 1)).clicked() {
                        d.m.choose(i);
                    }
                }
            }
            DbgState::Error { line, .. } => {
                if ui.button(format!("Go to line {line}")).clicked() {
                    self.editor.goto_line = Some(line);
                }
            }
            _ => {}
        }

        ui.add_space(6.0);
        egui::CollapsingHeader::new("Variables").default_open(true).show(ui, |ui| {
            egui::Grid::new("dbg-vars").num_columns(2).striped(true).spacing([14.0, 3.0]).show(ui, |ui| {
                for (k, v) in d.m.variables() {
                    ui.label(RichText::new(k).monospace());
                    ui.add(egui::Label::new(RichText::new(v).monospace()).truncate());
                    ui.end_row();
                }
            });
        });
        let stack = d.m.block_stack();
        if !stack.is_empty() {
            egui::CollapsingHeader::new(format!("Open blocks ({})", stack.len())).default_open(false).show(ui, |ui| {
                for s in stack.iter().rev() {
                    ui.label(RichText::new(s).monospace().small());
                }
            });
        }
        if !d.m.skipped.is_empty() {
            ui.add_space(4.0);
            let list: Vec<&str> = d.m.skipped.iter().map(String::as_str).collect();
            ui.label(RichText::new(format!("Skipped (not simulated): {}", list.join(", "))).weak().small());
        }
    }

    // ───────────────────────────── Simulator tab ─────────────────────────────

    /// Keep the simulator's programs in step with the editor, run it, and feed it keys.
    fn tick_sim(&mut self, ctx: &egui::Context) {
        if self.tab != Tab::Simulator {
            return;
        }
        let name = self.editor.name.trim().to_string();
        if !name.is_empty() && self.sim.m.programs.get(&name) != Some(&self.editor.text) {
            self.sim.add_program(&name, &self.editor.text);
        }
        if self.sim_focus {
            let events: Vec<egui::Event> = ctx.input(|i| i.events.clone());
            for e in events {
                let codes = match &e {
                    egui::Event::Text(t) => sim::desktop_key(t, false),
                    egui::Event::Key { key, pressed: true, .. } => match key {
                        egui::Key::Enter | egui::Key::Backspace | egui::Key::Delete | egui::Key::Escape => sim::desktop_key(key.name(), false),
                        egui::Key::ArrowUp | egui::Key::ArrowDown | egui::Key::ArrowLeft | egui::Key::ArrowRight => sim::desktop_key(key.name(), false),
                        _ => None,
                    },
                    _ => None,
                };
                for c in codes.unwrap_or_default() {
                    self.sim.press(c);
                }
            }
        }
        self.sim.tick(std::time::Duration::from_millis(12));
        if self.sim.running.is_some() {
            ctx.request_repaint();
        } else {
            // blinking cursor
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
        if let Some((prog, line)) = self.sim.goto_request.take() {
            if prog == name {
                self.tab = Tab::Editor;
                self.editor.goto_line = Some(line);
            } else {
                self.note(true, format!("prgm{prog} isn't the program open in the editor"));
            }
        }
    }

    fn simulator_tab(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("sim-side").default_width(210.0).show(ctx, |ui| {
            ui.add_space(6.0);
            ui.strong("Programs in the simulator");
            ui.add_space(4.0);
            let names: Vec<String> = self.sim.m.programs.keys().cloned().collect();
            if names.is_empty() {
                ui.label(RichText::new("None yet").weak());
            }
            for n in &names {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("prgm{n}")).monospace());
                    if *n == self.editor.name.trim() {
                        ui.label(RichText::new("(editor)").weak().small());
                    }
                });
            }
            ui.add_space(6.0);
            if ui.button("➕ Load .8xp programs…").clicked() {
                if let Some(files) = rfd::FileDialog::new().add_filter("TI programs", &["8xp"]).pick_files() {
                    for f in files {
                        let r = std::fs::read(&f).map_err(anyhow::Error::from).and_then(|b| VarFile::parse(&b));
                        match r {
                            Ok(vf) => {
                                for e in vf.entries.iter().filter(|e| matches!(e.var_type.0, 0x05 | 0x06)) {
                                    if let Ok(body) = varfile::token_body(&e.data) {
                                        self.sim.add_program(&e.display_name(), &tokens::detokenize(body, false));
                                    }
                                }
                            }
                            Err(e) => self.note(true, format!("{}: {e:#}", f.display())),
                        }
                    }
                }
            }
            if ui.button("⟲ Reset calculator").clicked() {
                let programs = std::mem::take(&mut self.sim.m.programs);
                self.sim = sim::Sim::new();
                self.sim.m.programs = programs;
            }
            ui.add_space(10.0);
            ui.separator();
            ui.label(RichText::new("How to use").strong());
            ui.label(
                RichText::new(
                    "Click the keys, or type: digits, letters (alpha), + - * / ^ ( ) , . \" : ?, \
                     Enter, Backspace = del, Esc = clear, arrows.\n\n\
                     prgm → EXEC runs a program; the one open in the Program Editor is always here. \
                     ON stops a running program. y= then graph plots functions of X; arrows pan, zoom resets.",
                )
                .small(),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new(
                    "This is CalLink's own TI-84 Plus CE-style simulator, not TI's OS: it covers the home \
                     screen, programs (the debugger's TI-BASIC subset), PRGM/MATH/TEST/CATALOG menus, Y= and graphs. \
                     Other menus and apps aren't simulated.",
                )
                .weak()
                .small(),
            );
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                // fit the screen into what the keypad leaves
                const KEYPAD_W: f32 = 5.0 * 60.0 + 24.0 + 30.0;
                let scale = ((ui.available_width() - KEYPAD_W - 60.0) / 320.0).clamp(0.9, 2.0);
                ui.horizontal_top(|ui| {
                    // the screen, inside a calculator-coloured bezel
                    let bezel = egui::Frame::none().fill(Color32::from_rgb(38, 42, 52)).rounding(14.0).inner_margin(egui::Margin::same(16.0));
                    bezel.show(ui, |ui| {
                        ui.vertical(|ui| {
                            let r = self.sim_screen(ui, scale);
                            if r.clicked() {
                                self.sim_focus = true;
                            }
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(if self.sim_focus { "Keyboard → calculator (click outside to stop)" } else { "Click the screen to type on it" })
                                    .small()
                                    .color(Color32::from_gray(170)),
                            );
                        });
                    });
                    ui.add_space(12.0);
                    let pad = egui::Frame::none().fill(Color32::from_rgb(38, 42, 52)).rounding(14.0).inner_margin(egui::Margin::same(12.0));
                    pad.show(ui, |ui| self.sim_keypad(ui));
                });
            });
        });
    }

    /// Draw the simulated screen; returns its response (click focuses the keyboard).
    fn sim_screen(&mut self, ui: &mut egui::Ui, scale: f32) -> egui::Response {
        use callink::basic::interp::State as MState;
        use sim::View as SV;
        let (w, h) = (320.0 * scale, 240.0 * scale);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 2.0, Color32::WHITE);
        // status bar
        let bar = egui::Rect::from_min_size(rect.min, egui::vec2(w, 18.0 * scale));
        p.rect_filled(bar, 0.0, Color32::from_rgb(85, 87, 83));
        p.text(bar.left_center() + egui::vec2(4.0 * scale, 0.0), egui::Align2::LEFT_CENTER, self.sim.status(), egui::FontId::proportional(10.5 * scale), Color32::WHITE);
        let body = egui::Rect::from_min_max(egui::pos2(rect.left(), bar.bottom()), rect.max);
        let (cols, rows) = (26.0, 10.0);
        let cw = body.width() / cols;
        let ch = body.height() / rows;
        // monospace glyphs are about 0.6 em wide: size the font to fill each character cell
        let font = egui::FontId::monospace((cw / 0.6).min(ch * 0.95));
        let ink = Color32::from_rgb(10, 10, 10);
        let blue = Color32::from_rgb(30, 90, 200);
        let cell = |r: usize, c: usize| egui::pos2(body.left() + c as f32 * cw, body.top() + r as f32 * ch);
        let put = |p: &egui::Painter, r: usize, c: usize, s: &str, color: Color32| {
            for (i, chr) in s.chars().enumerate() {
                if c + i >= 26 {
                    break;
                }
                p.text(cell(r, c + i) + egui::vec2(cw / 2.0, ch / 2.0), egui::Align2::CENTER_CENTER, chr, font.clone(), color);
            }
        };
        let invert = |p: &egui::Painter, r: usize, c: usize, s: &str| {
            let rr = egui::Rect::from_min_size(cell(r, c), egui::vec2(cw * s.chars().count() as f32, ch));
            p.rect_filled(rr, 0.0, Color32::from_rgb(40, 40, 40));
            put(p, r, c, s, Color32::WHITE);
        };
        let blink = (ui.input(|i| i.time) * 2.0) as i64 % 2 == 0;
        let cursor_block = |p: &egui::Painter, r: usize, c: usize, sim: &sim::Sim| {
            if !blink {
                return;
            }
            let rr = egui::Rect::from_min_size(cell(r, c), egui::vec2(cw * 0.9, ch * 0.92));
            p.rect_filled(rr, 0.0, ink);
            let mark = if sim.second { "↑" } else if sim.alpha || sim.alpha_lock { "A" } else { "" };
            if !mark.is_empty() {
                p.text(rr.center(), egui::Align2::CENTER_CENTER, mark, font.clone(), Color32::WHITE);
            }
        };

        // a program's Menu( screen takes over
        if let MState::Menu { title, items } = &self.sim.m.state {
            invert(&p, 0, 0, title);
            for (i, (label, _)) in items.iter().enumerate().take(9) {
                let s = format!("{}:{label}", i + 1);
                if i == self.sim.prog_menu_sel {
                    invert(&p, i + 1, 0, &s);
                } else {
                    put(&p, i + 1, 0, &s, ink);
                }
            }
            return resp;
        }
        match self.sim.view.clone() {
            SV::Home => {
                let (rows_txt, cur) = self.sim.home_rows();
                for (r, line) in rows_txt.iter().enumerate() {
                    put(&p, r, 0, line, ink);
                }
                if let Some((r, c)) = cur {
                    cursor_block(&p, r, c.min(25), &self.sim);
                }
                if self.sim.m.state == MState::PauseCmd {
                    p.text(bar.right_center() - egui::vec2(6.0, 0.0), egui::Align2::RIGHT_CENTER, "Paused", egui::FontId::monospace(9.0 * scale), Color32::YELLOW);
                }
            }
            SV::Menu => {
                if let Some(m) = &self.sim.menu {
                    let mut c = 0;
                    for (i, (t, _)) in m.tabs.iter().enumerate() {
                        if i == m.tab {
                            invert(&p, 0, c, t);
                        } else {
                            put(&p, 0, c, t, ink);
                        }
                        c += t.chars().count() + 1;
                    }
                    let items = &m.tabs[m.tab].1;
                    let first = m.sel.saturating_sub(8);
                    for (row, i) in (first..items.len()).take(9).enumerate() {
                        let num = if m.catalog { String::new() } else { format!("{}:", if i < 9 { (i + 1).to_string() } else if i == 9 { "0".into() } else { " ".into() }) };
                        let s = format!("{num}{}", items[i].0);
                        if i == m.sel {
                            invert(&p, row + 1, 0, &s);
                        } else {
                            put(&p, row + 1, 0, &s, ink);
                        }
                    }
                    if items.is_empty() {
                        put(&p, 1, 0, "(empty)", ink);
                    }
                }
            }
            SV::YEdit { row } => {
                put(&p, 0, 0, "Plot1 Plot2 Plot3", Color32::GRAY);
                const COLORS: [Color32; 6] = [
                    Color32::from_rgb(30, 90, 200),
                    Color32::from_rgb(210, 40, 40),
                    Color32::from_rgb(20, 20, 20),
                    Color32::from_rgb(200, 40, 200),
                    Color32::from_rgb(30, 150, 60),
                    Color32::from_rgb(230, 130, 20),
                ];
                for (i, t) in self.sim.y_rows().iter().enumerate() {
                    let label = format!("Y{}=", char::from_u32('₁' as u32 + i as u32).unwrap());
                    put(&p, i + 1, 0, "■", COLORS[i]);
                    put(&p, i + 1, 1, &label, ink);
                    put(&p, i + 1, 4, t, ink);
                    if i == row {
                        cursor_block(&p, i + 1, (4 + self.sim.cursor_col()).min(25), &self.sim);
                    }
                }
            }
            SV::Graph => {
                let win = self.sim.window;
                let gx = |x: f64| body.left() + ((x - win.xmin) / (win.xmax - win.xmin)) as f32 * body.width();
                let gy = |y: f64| body.bottom() - ((y - win.ymin) / (win.ymax - win.ymin)) as f32 * body.height();
                let axis = egui::Stroke::new(1.0_f32, Color32::from_gray(60));
                if win.xmin < 0.0 && win.xmax > 0.0 {
                    p.line_segment([egui::pos2(gx(0.0), body.top()), egui::pos2(gx(0.0), body.bottom())], axis);
                }
                if win.ymin < 0.0 && win.ymax > 0.0 {
                    p.line_segment([egui::pos2(body.left(), gy(0.0)), egui::pos2(body.right(), gy(0.0))], axis);
                }
                // tick marks every 1 unit when sensible
                for t in (win.xmin.ceil() as i64)..=(win.xmax.floor() as i64) {
                    if win.xmax - win.xmin <= 40.0 && win.ymin < 0.0 && win.ymax > 0.0 {
                        p.line_segment([egui::pos2(gx(t as f64), gy(0.0) - 3.0), egui::pos2(gx(t as f64), gy(0.0) + 3.0)], axis);
                    }
                }
                const COLORS: [Color32; 6] = [
                    Color32::from_rgb(30, 90, 200),
                    Color32::from_rgb(210, 40, 40),
                    Color32::from_rgb(20, 20, 20),
                    Color32::from_rgb(200, 40, 200),
                    Color32::from_rgb(30, 150, 60),
                    Color32::from_rgb(230, 130, 20),
                ];
                let n = (body.width() as usize).max(2);
                let xs: Vec<f64> = (0..=n).map(|i| win.xmin + (win.xmax - win.xmin) * i as f64 / n as f64).collect();
                for (i, e) in self.sim.functions() {
                    let ys = self.sim.sample(&e, &xs);
                    let stroke = egui::Stroke::new(2.0_f32, COLORS[i]);
                    for k in 1..xs.len() {
                        if let (Some(a), Some(b)) = (ys[k - 1], ys[k]) {
                            // don't join across asymptotes
                            if (b - a).abs() < (win.ymax - win.ymin) * 2.0 {
                                let pa = egui::pos2(gx(xs[k - 1]), gy(a));
                                let pb = egui::pos2(gx(xs[k]), gy(b));
                                if body.expand(4.0).contains(pa) || body.expand(4.0).contains(pb) {
                                    p.line_segment([pa, pb], stroke);
                                }
                            }
                        }
                    }
                }
                if self.sim.functions().is_empty() {
                    put(&p, 4, 3, "Press y= to enter a function", blue);
                }
            }
            SV::Error { msg, .. } => {
                let (head, detail) = msg.split_once(" (").map_or((msg.as_str(), ""), |(a, b)| (a, b.trim_end_matches(')')));
                invert(&p, 0, 0, head);
                let mut r = 1;
                for chunk in detail.chars().collect::<Vec<_>>().chunks(26) {
                    put(&p, r, 0, &chunk.iter().collect::<String>(), Color32::from_gray(90));
                    r += 1;
                }
                put(&p, r + 1, 0, "1:Quit", ink);
                put(&p, r + 2, 0, "2:Goto", ink);
            }
        }
        resp
    }

    fn sim_keypad(&mut self, ui: &mut egui::Ui) {
        let keypad = sim::keypad();
        let second_col = Color32::from_rgb(110, 170, 255);
        let alpha_col = Color32::from_rgb(110, 210, 120);
        let mut pressed: Option<u16> = None;
        let key = |ui: &mut egui::Ui, k: &sim::KeyDef, pressed: &mut Option<u16>| {
            ui.vertical(|ui| {
                ui.set_width(54.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(RichText::new(k.second_label).size(9.0).color(second_col));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(k.alpha_label).size(9.0).color(alpha_col));
                    });
                });
                let (fill, text) = match k.code {
                    21 => (Color32::from_rgb(60, 120, 210), Color32::WHITE),
                    31 => (Color32::from_rgb(60, 160, 80), Color32::WHITE),
                    72..=74 | 82..=84 | 92..=94 | 102..=104 => (Color32::from_rgb(235, 235, 235), Color32::BLACK),
                    105 => (Color32::from_rgb(200, 200, 205), Color32::BLACK),
                    _ => (Color32::from_rgb(70, 74, 84), Color32::WHITE),
                };
                let b = egui::Button::new(RichText::new(k.label).size(12.5).color(text)).fill(fill).min_size(egui::vec2(54.0, 26.0)).rounding(6.0);
                if ui.add(b).clicked() {
                    *pressed = Some(k.code);
                }
            });
        };
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 4.0);
            // graph keys
            ui.horizontal(|ui| {
                for k in &keypad[0] {
                    key(ui, k, &mut pressed);
                }
            });
            ui.add_space(4.0);
            // 2nd/mode/del and alpha/X,T,θ,n/stat, with the arrow pad beside them
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    for row in &keypad[1..3] {
                        ui.horizontal(|ui| {
                            for k in row {
                                key(ui, k, &mut pressed);
                            }
                        });
                    }
                });
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.add_space(6.0);
                    let arrow = |ui: &mut egui::Ui, code: u16, label: &str, pressed: &mut Option<u16>| {
                        let b = egui::Button::new(RichText::new(label).size(13.0).color(Color32::WHITE))
                            .fill(Color32::from_rgb(70, 74, 84))
                            .min_size(egui::vec2(36.0, 26.0))
                            .rounding(13.0);
                        if ui.add(b).clicked() {
                            *pressed = Some(code);
                        }
                    };
                    ui.horizontal(|ui| {
                        ui.add_space(40.0);
                        arrow(ui, 25, "▲", &mut pressed);
                    });
                    ui.horizontal(|ui| {
                        arrow(ui, 24, "◀", &mut pressed);
                        ui.add_space(36.0);
                        arrow(ui, 26, "▶", &mut pressed);
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(40.0);
                        arrow(ui, 34, "▼", &mut pressed);
                    });
                });
            });
            ui.add_space(4.0);
            for row in &keypad[3..] {
                ui.horizontal(|ui| {
                    for k in row {
                        key(ui, k, &mut pressed);
                    }
                });
            }
        });
        if let Some(code) = pressed {
            self.sim.press(code);
            self.sim_focus = true;
        }
    }

    fn save_as(&mut self) {
        let default = format!("{}.8xp", if self.editor.name.is_empty() { "PROGRAM" } else { &self.editor.name });
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("TI program", &["8xp"])
            .add_filter("Text", &["txt"])
            .set_file_name(default)
            .save_file()
        {
            self.save_to(&p);
        }
    }

    fn save_to(&mut self, p: &Path) {
        let r = if has_ext(p, &["txt"]) {
            std::fs::write(p, &self.editor.text).map_err(anyhow::Error::from)
        } else {
            self.editor
                .to_entry()
                .and_then(|e| VarFile { comment: COMMENT.into(), entries: vec![e] }.to_bytes())
                .and_then(|b| std::fs::write(p, b).map_err(anyhow::Error::from))
        };
        match r {
            Ok(()) => {
                self.editor.path = Some(p.to_path_buf());
                self.editor.dirty = false;
                self.note(false, format!("Saved {}", p.display()));
            }
            Err(e) => self.note(true, format!("{e:#}")),
        }
    }

    fn update_window(&mut self, ctx: &egui::Context) {
        let Some(u) = &self.update else { return };
        let mut open = true;
        let mut pick = false;
        egui::Window::new("Calculator OS update").open(&mut open).collapsible(false).default_width(460.0).show(ctx, |ui| {
            egui::Grid::new("osinfo").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label(RichText::new("Calculator").weak());
                ui.label(&u.product);
                ui.end_row();
                ui.label(RichText::new("Installed OS").weak());
                ui.label(&u.current);
                ui.end_row();
                ui.label(RichText::new("Latest from TI").weak());
                match &u.latest {
                    None => {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new());
                            ui.label("checking…");
                        });
                    }
                    Some(Ok(l)) => {
                        ui.label(&l.version);
                    }
                    Some(Err(_)) => {
                        ui.label("unknown");
                    }
                }
                ui.end_row();
            });
            ui.add_space(8.0);
            let page = match &u.latest {
                Some(Ok(l)) => {
                    if osupdate::version_cmp(&l.version, &u.current).is_gt() {
                        ui.label(RichText::new(format!("⬆ OS {} is available.", l.version)).strong().color(Color32::from_rgb(70, 160, 230)));
                        ui.label("Download it from TI (you'll accept TI's license there), then choose the downloaded file below.");
                    } else {
                        ui.label(RichText::new("✔ Your calculator has the latest OS.").color(Color32::from_rgb(60, 170, 90)));
                    }
                    l.page_url.clone()
                }
                Some(Err(e)) => {
                    ui.label(RichText::new(format!("Couldn't check: {e}")).color(Color32::from_rgb(220, 150, 40)));
                    osupdate::os_page(&u.product).unwrap_or(osupdate::DOWNLOADS_PAGE).to_string()
                }
                None => osupdate::os_page(&u.product).unwrap_or(osupdate::DOWNLOADS_PAGE).to_string(),
            };
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("🌐 Open TI download page").clicked() {
                    open_url(&page);
                }
                if ui.add_enabled(self.busy.is_none(), egui::Button::new("⬆ Install OS file…")).clicked() {
                    pick = true;
                }
            });
            ui.add_space(6.0);
            ui.label(
                RichText::new(
                    "Installing an OS takes several minutes and clears RAM (archived variables and apps stay). \
                     Keep the cable connected until the calculator shows its home screen. Back up first.",
                )
                .weak()
                .small(),
            );
        });
        if pick {
            self.pick_and_install();
        }
        if !open {
            self.update = None;
        }
    }

    fn pick_and_install(&mut self) {
        let Some(u) = &self.update else { return };
        let (product, current) = (u.product.clone(), u.current.clone());
        let Some(path) = rfd::FileDialog::new()
            .set_title("Choose the OS file downloaded from TI")
            .add_filter("Calculator OS", &["8xu", "8eu", "8cu", "8pu", "82u"])
            .pick_file()
        else {
            return;
        };
        let checked = std::fs::read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|b| flash::OsFile::parse(&b))
            .and_then(|os| osupdate::check_compatible(&os, callink::model::Model::identify(None, &product)).map(|_| os));
        let os = match checked {
            Ok(os) => os,
            Err(e) => {
                rfd::MessageDialog::new()
                    .set_title("Can't install this file")
                    .set_description(format!("{e:#}"))
                    .set_level(rfd::MessageLevel::Error)
                    .show();
                return;
            }
        };
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let new_ver = osupdate::version_from_file_name(&file_name).unwrap_or_else(|| os.version_string());
        let note = match osupdate::version_cmp(&new_ver, &current) {
            std::cmp::Ordering::Less => "\n\n⚠ This is OLDER than the installed OS. A TI-84 Plus CE will refuse it.",
            std::cmp::Ordering::Equal => "\n\nThis is the version already installed (it will be reinstalled).",
            _ => "",
        };
        let ok = rfd::MessageDialog::new()
            .set_title("Install calculator OS")
            .set_description(format!(
                "Install OS {new_ver} on the {product} (currently {current})?{note}\n\n\
                 • Takes several minutes; don't unplug the calculator or close CalLink.\n\
                 • RAM is cleared; archived variables and apps are kept.\n\
                 • If it's interrupted, the calculator waits for an OS; run the install again."
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .set_level(rfd::MessageLevel::Warning)
            .show()
            == rfd::MessageDialogResult::Yes;
        if ok {
            self.request(Req::InstallOs(path));
        }
    }

    fn windows(&mut self, ctx: &egui::Context) {
        self.update_window(ctx);
        if let Some((title, body)) = &self.viewer {
            let mut open = true;
            egui::Window::new(title.as_str()).open(&mut open).default_width(420.0).show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(body).monospace()).wrap());
                });
            });
            if !open {
                self.viewer = None;
            }
        }
        if self.show_log {
            egui::Window::new("Log").open(&mut self.show_log).default_size([520.0, 260.0]).show(ctx, |ui| {
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink(false).show(ui, |ui| {
                    for (err, m) in &self.log {
                        let c = if *err { Color32::from_rgb(220, 80, 70) } else { ui.visuals().text_color() };
                        ui.label(RichText::new(m).color(c));
                    }
                });
            });
        }
    }

    fn scripted_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.shot.clone() else { return };
        self.frames += 1;
        // scripted typing for the completion screenshots
        let script = std::env::var("CALLINK_GUI_TAB").unwrap_or_default();
        if script.starts_with("sim") && self.frames == 3 {
            let seq: Vec<u16> = match script.as_str() {
                // 2nd √ 2 ) enter ; 5 sto→ A enter ; A²+3 enter
                "sim" => vec![21, 61, 93, 64, 105, 83, 91, 31, 41, 105, 31, 41, 61, 95, 94, 105],
                // y= X² - 4 ▼ sin( X ) × 3 graph
                "sim-graph" => vec![11, 32, 61, 85, 82, 34, 52, 32, 64, 75, 94, 15],
                // prgm 1 enter, then 7 enter for its Input
                "sim-prgm" => vec![43, 92, 105],
                "sim-menu" => vec![41, 26],
                _ => vec![],
            };
            if script == "sim-prgm" {
                self.sim.add_program(
                    "GUESS",
                    "ClrHome\nDisp \"GUESS 1-20\"\n13→N\n0→T\nRepeat G=N\nInput \"GUESS?\",G\nT+1→T\nIf G<N:Disp \"HIGHER\"\nIf G>N:Disp \"LOWER\"\nEnd\nDisp \"GOT IT IN\",T",
                );
            }
            for c in seq {
                self.sim.press(c);
                self.sim.tick(std::time::Duration::from_millis(50));
            }
            if script == "sim-prgm" {
                for keys in [[93u16, 102, 105], [92, 102, 105], [92, 94, 105]] {
                    for c in keys {
                        self.sim.press(c);
                    }
                    self.sim.tick(std::time::Duration::from_millis(50));
                }
            }
        }
        if script == "complete" || script == "arghint" {
            let id = egui::Id::new("program-text");
            if self.frames == 3 {
                self.editor.text = if script == "complete" { "ClrHome\nFor(I,1,10)\nDisp ran".into() } else { "ClrHome\nOutput(3,randInt(1,".into() };
                self.editor.recheck();
                let n = self.editor.text.chars().count();
                let mut st = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
                st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(n))));
                st.store(ctx, id);
                ctx.memory_mut(|m| m.request_focus(id));
            }

        }
        ctx.request_repaint();
        if !self.shot_requested && self.frames > 40 && self.busy.is_none() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
            self.shot_requested = true;
        }
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let rgb: Vec<u8> = img.pixels.iter().flat_map(|p| [p.r(), p.g(), p.b()]).collect();
            let im = Image { width: img.size[0] as u32, height: img.size[1] as u32, rgb };
            if let Ok(f) = std::fs::File::create(&path) {
                let _ = screen::write_png(&im, std::io::BufWriter::new(f), 1);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        // scripted keystroke for the completion screenshot (arrives like real typing)
        if self.shot.is_some() && self.frames == 8 && std::env::var("CALLINK_GUI_TAB").as_deref() == Ok("complete") {
            raw.events.push(egui::Event::Text("d".into()));
        }
        if self.shot.is_some() && self.frames == 14 && std::env::var("CALLINK_GUI_ACCEPT").is_ok() {
            raw.events.push(egui::Event::Key {
                key: egui::Key::Tab,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        self.handle_drops(ctx);
        self.tick_debugger(ctx);
        self.tick_sim(ctx);
        self.top_bar(ctx);
        self.status_bar(ctx);
        match self.tab {
            Tab::Explorer => self.explorer(ctx),
            Tab::Screen => self.screen_tab(ctx),
            Tab::Editor => self.editor_tab(ctx),
            Tab::Simulator => self.simulator_tab(ctx),
        }
        self.windows(ctx);
        self.scripted_screenshot(ctx);
    }
}

fn open_url(url: &str) {
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    let _ = r;
}

fn has_ext(p: &Path, exts: &[&str]) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

fn human_size(n: u32) -> String {
    if n < 1024 {
        format!("{n} B")
    } else {
        format!("{:.1} KB", n as f64 / 1024.0)
    }
}

/// egui's bundled fonts lack many TI glyphs (θ ₁ ᴇ ⁻¹ ► 𝑖 ℯ …); add system fonts as fallbacks.
fn setup_fonts(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        // Linux
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansMath-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansMath-Regular.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf",
        // Windows
        "C:\\Windows\\Fonts\\consola.ttf",
        "C:\\Windows\\Fonts\\seguisym.ttf",
        "C:\\Windows\\Fonts\\cambria.ttc",
        // macOS
        "/System/Library/Fonts/Menlo.ttc",
        "/System/Library/Fonts/Apple Symbols.ttf",
        "/System/Library/Fonts/Supplemental/STIXTwoMath.otf",
    ];
    let mut fonts = egui::FontDefinitions::default();
    for (i, p) in CANDIDATES.iter().enumerate() {
        if let Ok(bytes) = std::fs::read(p) {
            let name = format!("fallback-{i}");
            fonts.font_data.insert(name.clone(), egui::FontData::from_owned(bytes));
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(fam).or_default().push(name.clone());
            }
        }
    }
    ctx.set_fonts(fonts);
}

fn window_icon() -> Option<egui::IconData> {
    let dec = png::Decoder::new(&include_bytes!("../../packaging/callink.png")[..]);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    (info.color_type == png::ColorType::Rgba).then(|| egui::IconData {
        rgba: buf[..info.buffer_size()].to_vec(),
        width: info.width,
        height: info.height,
    })
}

fn main() -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("CalLink")
        .with_app_id("callink")
        .with_inner_size([1040.0, 680.0])
        .with_min_inner_size([680.0, 420.0])
        .with_drag_and_drop(true);
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let opts = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("CalLink", opts, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
