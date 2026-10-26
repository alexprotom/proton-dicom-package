//! The wizard. The same UI stack (egui/eframe on wgpu) and the same pages as
//! the Rust DICOM Station installer, so both setups look and behave alike.
//!
//! Every screen is one function; the long-running work happens on a worker
//! thread that reports through a shared progress slot.

use std::cmp::Ordering as VersionOrder;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use egui::{Color32, RichText};

use crate::existing::{self, Installed};
use crate::install::{self, Event};
use crate::payload::Payload;
use crate::plan::*;
use crate::uninstall::{self, Target};
use crate::update::{self, HandOff, Release};

const AMBER: Color32 = Color32::from_rgb(230, 170, 60);
const GREEN: Color32 = Color32::from_rgb(120, 200, 120);
const RED: Color32 = Color32::from_rgb(230, 100, 100);

#[derive(PartialEq, Eq, Clone, Copy)]
enum Screen {
    Welcome,
    Options,
    Working,
    Done,
    ConfirmUninstall,
    /// The window of `--update`: what is installed, what is newest.
    Update,
}

enum Job {
    Install {
        payload: Arc<Payload>,
    },
    Uninstall {
        target: Arc<Target>,
    },
    /// `--update`: nothing to install from this binary itself.
    Update,
}

/// What the worker thread is doing.
#[derive(Clone)]
enum Work {
    Install,
    Uninstall,
    /// Download a release, check it and start it; `elevate` when the
    /// installation it will update is machine-wide.
    Fetch {
        release: Release,
        how: HandOff,
        elevate: bool,
    },
}

/// How the wizard was started.
#[derive(Clone, Copy, Default)]
pub struct Mode {
    /// Begin at once with the options given (the elevated re-launch, the
    /// hand-over from `--update`, `--passive`).
    pub autostart: bool,
    /// No questions, and the window closes itself after a success.
    pub passive: bool,
}

/// How the worker's run ended: the error carries its exit code.
type Outcome = Result<(), (u8, String)>;

/// The newest release, as far as this run knows.
#[derive(Clone)]
enum Online {
    Checking,
    Latest(Release),
    Failed(String),
}

pub struct SetupApp {
    job: Job,
    screen: Screen,
    opts: Options,
    version: String,
    license: Option<String>,
    accepted: bool,
    dir_text: String,
    payload_size: u64,
    /// The tools in the payload, listed on the first page.
    tools: Vec<Tool>,
    /// Registered installations, found when the setup started.
    installed: Vec<Installed>,
    /// The newest release on GitHub; looked up in the background.
    online: Arc<Mutex<Online>>,
    /// What the worker is doing, once it runs.
    work: Option<Work>,
    progress: Arc<Mutex<(f32, String)>>,
    log: Arc<Mutex<Vec<String>>>,
    outcome: Arc<Mutex<Option<Outcome>>>,
    cancel: Arc<AtomicBool>,
    error: Option<String>,
    /// The process exit code, read after the window closes. Starts as
    /// "cancelled": closing the wizard before it did anything is that.
    exit: Arc<Mutex<u8>>,
    /// Begin immediately instead of showing the first page (used by the
    /// elevated re-launch, which inherits the options on the command line).
    autostart: bool,
    passive: bool,
    /// When a passive run finished, to close the window a moment later.
    done_at: Option<Instant>,
    /// Set once the user asks to close after a finished run.
    quit: bool,
}

/// Show the install wizard. Returns `Err` when no window could be created
/// (head-less session, no usable GPU adapter) so the caller can fall back to
/// the text interface; otherwise the exit code to finish with.
pub fn run_install(
    payload: Payload,
    opts: Options,
    mode: Mode,
    installed: Vec<Installed>,
) -> Result<u8> {
    let license = payload.read_text("LICENSE.txt");
    let version = install::payload_version(&payload);
    let payload_size = payload.total_size().unwrap_or(0);
    let tools = payload
        .read_text(TOOLS_FILE)
        .map(|t| parse_tools(&t))
        .unwrap_or_default();
    let mut app = SetupApp::new(
        Job::Install {
            payload: Arc::new(payload),
        },
        Screen::Welcome,
        opts,
        version,
        license,
        payload_size,
    );
    app.tools = tools;
    app.installed = installed;
    // An update was accepted when the installation was first made; the
    // license stays one click away on the first page.
    if app.same().is_some() {
        app.accepted = true;
    }
    // After an elevation re-launch the user has already made every choice in
    // the first window; go straight to work.
    app.autostart = mode.autostart;
    app.passive = mode.passive;
    if !mode.autostart {
        app.check_online();
    }
    let exit = app.exit.clone();
    launch(app, &format!("{APP_NAME}: Setup"))?;
    let code = *exit.lock().unwrap();
    Ok(code)
}

/// The window of `--update`.
pub fn run_update() -> Result<u8> {
    let mut app = SetupApp::new(
        Job::Update,
        Screen::Update,
        Options::default(),
        env!("CARGO_PKG_VERSION").to_string(),
        None,
        0,
    );
    app.installed = existing::find_all();
    *app.exit.lock().unwrap() = EXIT_OK;
    app.check_online();
    let exit = app.exit.clone();
    launch(app, &format!("{APP_NAME}: Update"))?;
    let code = *exit.lock().unwrap();
    Ok(code)
}

/// Show the uninstall confirmation window.
pub fn run_uninstall(target: Target) -> Result<()> {
    let opts = Options {
        dir: target.manifest.install_dir.clone(),
        ..Default::default()
    };
    let version = target.manifest.version.clone();
    let app = SetupApp::new(
        Job::Uninstall {
            target: Arc::new(target),
        },
        Screen::ConfirmUninstall,
        opts,
        version,
        None,
        0,
    );
    launch(app, &format!("{APP_NAME}: Uninstall"))
}

/// Open the wizard window.
///
/// A handful of Windows machines advertise a Vulkan driver that cannot
/// create a device, so the window is *attempted* rather than opened: first
/// whatever `wgpu` would pick on its own (which already honours
/// `WGPU_BACKEND`), then Direct3D 12, then Vulkan. A backend that fails - by
/// error or by panicking inside the driver - costs a line on standard error
/// instead of the installation; if none works, the caller falls back to the
/// text interface.
fn launch(app: SetupApp, title: &str) -> Result<()> {
    // The same picture as the setup executable's icon resource (build.rs).
    let icon = Arc::new(
        eframe::icon_data::from_png_bytes(include_bytes!("../../assets/proton-dicom-package.png"))
            .unwrap_or_default(),
    );
    // `run_native` consumes the app, so it waits here and the creation
    // closure takes it out. An attempt that dies before creating anything
    // never gets that far, and the next one finds it still in place.
    let slot = Arc::new(Mutex::new(Some(app)));
    let order = attempts();
    let mut last = "no graphics backend could open a window".to_string();
    for (i, attempt) in order.iter().enumerate() {
        if i > 0 {
            eprintln!(
                "{APP_NAME} setup: {} did not work, trying {}",
                order[i - 1].label,
                attempt.label
            );
        }
        match attempt.open(&slot, title, &icon) {
            Ok(()) => return Ok(()),
            Err(e) => {
                eprintln!("{APP_NAME} setup: {} failed: {e}", attempt.label);
                last = e;
            }
        }
    }
    Err(anyhow::anyhow!(
        "could not open the installer window: {last}"
    ))
}

/// One backend to try for the wizard's own window.
struct Attempt {
    label: &'static str,
    /// `None` leaves `wgpu`'s own default in place, which is what an
    /// unremarkable machine - and anyone who set `WGPU_BACKEND` - should get.
    bits: Option<eframe::wgpu::Backends>,
}

fn attempts() -> Vec<Attempt> {
    use eframe::wgpu::Backends;
    vec![
        Attempt {
            label: "the default backend",
            bits: None,
        },
        Attempt {
            label: "DirectX 12",
            bits: Some(Backends::DX12),
        },
        Attempt {
            label: "Vulkan",
            bits: Some(Backends::VULKAN),
        },
    ]
}

impl Attempt {
    /// Try to run the wizard to completion on this backend. `Err` carries a
    /// message for the next attempt to report; the app is left in `slot`
    /// unless a window was actually created.
    fn open(
        &self,
        slot: &Arc<Mutex<Option<SetupApp>>>,
        title: &str,
        icon: &Arc<egui::IconData>,
    ) -> std::result::Result<(), String> {
        let mut wgpu_options = eframe::WgpuConfiguration::default();
        if let Some(bits) = self.bits {
            if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
                setup.instance_descriptor.backends = bits;
            }
        }
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title(title)
                .with_icon(Arc::clone(icon))
                .with_inner_size([720.0, 520.0])
                .with_min_inner_size([600.0, 420.0]),
            renderer: eframe::Renderer::Wgpu,
            wgpu_options,
            ..Default::default()
        };
        let slot = slot.clone();
        // `NativeOptions` holds boxed callbacks that are not `UnwindSafe`,
        // which is a fair warning in general and irrelevant here: nothing is
        // read back after a failed attempt - the next one builds its own.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            eframe::run_native(
                title,
                options,
                Box::new(move |cc| {
                    cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
                    let app = slot
                        .lock()
                        .unwrap()
                        .take()
                        .ok_or("the installer window was created twice")?;
                    Ok(Box::new(app) as Box<dyn eframe::App>)
                }),
            )
        }));
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("{e}")),
            Err(_) => Err(format!("the {} driver crashed while starting", self.label)),
        }
    }
}

impl SetupApp {
    fn new(
        job: Job,
        screen: Screen,
        opts: Options,
        version: String,
        license: Option<String>,
        payload_size: u64,
    ) -> SetupApp {
        SetupApp {
            dir_text: opts.dir.to_string_lossy().to_string(),
            job,
            screen,
            opts,
            version,
            license,
            accepted: false,
            payload_size,
            tools: Vec::new(),
            installed: Vec::new(),
            online: Arc::new(Mutex::new(Online::Checking)),
            work: None,
            progress: Arc::new(Mutex::new((0.0, String::new()))),
            log: Arc::new(Mutex::new(Vec::new())),
            outcome: Arc::new(Mutex::new(None)),
            cancel: Arc::new(AtomicBool::new(false)),
            error: None,
            exit: Arc::new(Mutex::new(EXIT_CANCELLED)),
            autostart: false,
            passive: false,
            done_at: None,
            quit: false,
        }
    }

    /// Look up the newest release in the background. Being offline is
    /// normal for a clinical workstation, so a failure is only remembered.
    fn check_online(&mut self) {
        *self.online.lock().unwrap() = Online::Checking;
        let slot = self.online.clone();
        std::thread::spawn(move || {
            let found = match update::latest() {
                Ok(r) => Online::Latest(r),
                Err(e) => Online::Failed(format!("{e:#}")),
            };
            *slot.lock().unwrap() = found;
        });
    }

    /// The installation in the chosen folder, which this run updates.
    fn same(&self) -> Option<Installed> {
        existing::split(&self.installed, &self.opts.dir).0
    }

    /// Every other registered installation.
    fn others(&self) -> Vec<Installed> {
        existing::split(&self.installed, &self.opts.dir).1
    }

    /// Whether the run has to happen as administrator: a machine-wide
    /// target, or a machine-wide installation to remove or to move.
    fn needs_elevation(&self) -> bool {
        self.opts.scope == Scope::AllUsers
            || self.same().is_some_and(|p| p.machine_wide())
            || (self.opts.remove_others && self.others().iter().any(|o| o.machine_wide()))
    }

    /// Install or update, asking for administrator rights first when needed.
    fn begin(&mut self, ctx: &egui::Context) {
        if self.needs_elevation() && !crate::win::is_elevated() {
            let mut args = crate::args_for_relaunch(&self.opts);
            if self.passive {
                args.push_str(" --passive");
            }
            match crate::win::relaunch_elevated(&args) {
                Ok(()) => {
                    // The elevated copy carries on, and reports on its own.
                    *self.exit.lock().unwrap() = EXIT_OK;
                    self.quit = true;
                }
                Err(e) => self.error = Some(format!("{e:#}")),
            }
        } else {
            self.start(ctx, Work::Install);
        }
    }

    fn start(&mut self, ctx: &egui::Context, work: Work) {
        self.screen = Screen::Working;
        self.error = None;
        self.cancel.store(false, Ordering::Relaxed);
        self.log.lock().unwrap().clear();
        *self.outcome.lock().unwrap() = None;
        *self.progress.lock().unwrap() = (0.0, String::new());
        self.work = Some(work.clone());
        let progress = self.progress.clone();
        let log = self.log.clone();
        let outcome = self.outcome.clone();
        let cancel = self.cancel.clone();
        let ctx = ctx.clone();
        let opts = self.opts.clone();
        let payload = match &self.job {
            Job::Install { payload } => Some(payload.clone()),
            _ => None,
        };
        let target = match &self.job {
            Job::Uninstall { target } => Some(target.clone()),
            _ => None,
        };
        std::thread::spawn(move || {
            let sink = move |ev: Event| {
                match ev {
                    Event::Progress(f, msg) => *progress.lock().unwrap() = (f, msg),
                    Event::Log(line) => log.lock().unwrap().push(line),
                }
                ctx.request_repaint();
            };
            let res = match (&work, &payload, &target) {
                (Work::Install, Some(payload), _) => install::run(&opts, payload, &sink, &cancel),
                (Work::Uninstall, _, Some(target)) => uninstall::run(target, &sink),
                (
                    Work::Fetch {
                        release,
                        how,
                        elevate,
                    },
                    _,
                    _,
                ) => update::download(
                    release,
                    &|f, msg| sink(Event::Progress(f, msg.to_string())),
                    &cancel,
                )
                .and_then(|setup| {
                    sink(Event::Log(format!("Verified {}", setup.display())));
                    sink(Event::Log("Starting it".into()));
                    update::hand_off(&setup, *how, *elevate).map(|_| ())
                }),
                _ => Err(anyhow::anyhow!("nothing to do")),
            };
            *outcome.lock().unwrap() = Some(res.map_err(|e| (exit_code_of(&e), format!("{e:#}"))));
        });
    }

    fn is_install(&self) -> bool {
        matches!(self.job, Job::Install { .. })
    }

    /// Open the Proton DICOM prompt from the installed folder.
    fn open_prompt(&self) {
        let _ = crate::win::shell_execute(
            &cmd_exe(),
            &shell_args(&self.opts.shell_path(), None),
            false,
        );
    }
}

impl eframe::App for SetupApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.autostart {
            self.autostart = false;
            self.begin(&ctx);
        }
        if self.screen == Screen::Working {
            let done = self.outcome.lock().unwrap().take();
            if let Some(res) = done {
                match res {
                    Ok(()) => {
                        *self.exit.lock().unwrap() = EXIT_OK;
                        self.error = None;
                    }
                    Err((code, msg)) => {
                        *self.exit.lock().unwrap() = code;
                        self.error = Some(msg);
                    }
                }
                let fetched = matches!(self.work, Some(Work::Fetch { .. }));
                if fetched && self.error.is_none() {
                    // The downloaded setup has its own window now.
                    self.quit = true;
                } else {
                    self.screen = Screen::Done;
                    self.done_at = Some(Instant::now());
                }
            }
            // The worker only repaints on new messages; keep the bar alive.
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        // A passive run closes itself shortly after a success, and stays
        // open on a failure so the message can be read.
        if self.passive && self.screen == Screen::Done && self.error.is_none() {
            if self
                .done_at
                .is_some_and(|t| t.elapsed() > Duration::from_secs(2))
            {
                self.quit = true;
            } else {
                ctx.request_repaint_after(Duration::from_millis(200));
            }
        }
        let title = match self.job {
            Job::Install { .. } => format!("{APP_NAME} {}", self.version),
            Job::Uninstall { .. } => format!("Uninstall {APP_NAME}"),
            Job::Update => format!("Update {APP_NAME}"),
        };
        egui::Panel::top(egui::Id::new("head")).show(ui, |ui| {
            ui.add_space(8.0);
            ui.heading(title);
            ui.label(
                RichText::new(
                    "Open-source command-line tools for proton radiography and CT in DICOM \
                     - research and QA",
                )
                .weak(),
            );
            ui.add_space(8.0);
        });
        let screen = self.screen;
        egui::CentralPanel::default_margins().show(ui, |ui| match screen {
            Screen::Welcome => self.welcome(ui, &ctx),
            Screen::Options => self.options(ui, &ctx),
            Screen::Working => self.working(ui),
            Screen::Done => self.done(ui),
            Screen::ConfirmUninstall => self.confirm_uninstall(ui, &ctx),
            Screen::Update => self.update_page(ui, &ctx),
        });
        if self.quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl SetupApp {
    fn welcome(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            let same = self.same();
            match &same {
                None => {
                    ui.label(
                        "This installs the command-line tools, puts them on PATH and adds a \
                         Proton DICOM prompt to the Start menu and to the context menu of \
                         folders.",
                    );
                }
                Some(prev) => {
                    ui.label(RichText::new(format!("{APP_NAME} is installed")).strong());
                    ui.label(prev.describe());
                    ui.add_space(4.0);
                    match compare_versions(&self.version, &prev.version) {
                        VersionOrder::Greater => {
                            ui.label(format!(
                                "This setup updates it to {}. The folder and the options \
                                 chosen at installation stay as they are; files the new \
                                 version no longer ships are removed.",
                                self.version
                            ));
                        }
                        VersionOrder::Equal => {
                            ui.label(
                                "This version is already installed. Continuing installs it \
                                 again, which restores missing or damaged files.",
                            );
                        }
                        VersionOrder::Less => {
                            ui.colored_label(
                                AMBER,
                                format!(
                                    "The installed version is newer than this setup ({}). \
                                     Continuing replaces it with the older version.",
                                    self.version
                                ),
                            );
                        }
                    }
                }
            }
            self.tools_block(ui);
            self.others_block(ui);
            self.online_block(ui, ctx);
            ui.add_space(6.0);
            match (&self.license, &same) {
                (Some(text), Some(_)) => {
                    egui::CollapsingHeader::new("License").show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(200.0)
                            .show(ui, |ui| {
                                ui.monospace(text);
                            });
                    });
                }
                (Some(text), None) => {
                    ui.label(RichText::new("License").strong());
                    egui::ScrollArea::vertical()
                        .max_height(240.0)
                        .show(ui, |ui| {
                            ui.monospace(text);
                        });
                    ui.add_space(6.0);
                    ui.checkbox(&mut self.accepted, "I accept the license terms");
                }
                (None, _) => {
                    self.accepted = true;
                    ui.label(
                        "MIT-licensed software. Not a medical device - for research and QA only.",
                    );
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if let Some(prev) = &same {
                    let label = match compare_versions(&self.version, &prev.version) {
                        VersionOrder::Greater => format!("Update to {}", self.version),
                        VersionOrder::Equal => "Reinstall".to_string(),
                        VersionOrder::Less => format!("Replace with {}", self.version),
                    };
                    if ui.button(label).clicked() {
                        self.begin(ctx);
                    }
                    if ui.button("Change options  ▶").clicked() {
                        self.screen = Screen::Options;
                    }
                } else if ui
                    .add_enabled(self.accepted, egui::Button::new("Next  ▶"))
                    .clicked()
                {
                    self.screen = Screen::Options;
                }
                if ui.button("Cancel").clicked() {
                    self.quit = true;
                }
            });
            if let Some(err) = &self.error {
                ui.add_space(6.0);
                ui.colored_label(RED, err);
            }
        });
    }

    /// What is in the package.
    fn tools_block(&self, ui: &mut egui::Ui) {
        if self.tools.is_empty() {
            return;
        }
        ui.add_space(10.0);
        ui.label(RichText::new("Tools").strong());
        egui::Grid::new("tools").num_columns(2).show(ui, |ui| {
            for t in &self.tools {
                ui.label(RichText::new(&t.name).monospace());
                ui.label(&t.description);
                ui.end_row();
            }
        });
    }

    /// The installations other than the one in the chosen folder, and the
    /// choice to remove them.
    fn others_block(&mut self, ui: &mut egui::Ui) {
        let others = self.others();
        if others.is_empty() {
            return;
        }
        ui.add_space(10.0);
        ui.label(
            RichText::new(if others.len() == 1 {
                "Another installation"
            } else {
                "Other installations"
            })
            .strong(),
        );
        for o in &others {
            ui.label(o.describe());
        }
        ui.checkbox(
            &mut self.opts.remove_others,
            "Remove it, so that one copy remains",
        );
        if self.opts.remove_others
            && others.iter().any(|o| o.machine_wide())
            && !crate::win::is_elevated()
        {
            ui.colored_label(
                AMBER,
                "Removing an installation for all users needs administrator rights; they \
                 will be requested.",
            );
        }
    }

    /// A newer release than this setup, when there is one.
    fn online_block(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let state = self.online.lock().unwrap().clone();
        match state {
            Online::Checking => {
                ctx.request_repaint_after(Duration::from_millis(300));
            }
            Online::Latest(r) if r.is_newer_than(&self.version) => {
                ui.add_space(10.0);
                ui.colored_label(
                    AMBER,
                    format!(
                        "A newer release is available: {}. This setup is {}.",
                        r.version, self.version
                    ),
                );
                if ui
                    .button(format!("Download and install {} instead", r.version))
                    .on_hover_text(
                        "Downloads the installer from the project's GitHub releases, checks \
                         it against the release's SHA256SUMS and starts it.",
                    )
                    .clicked()
                {
                    self.start(
                        ctx,
                        Work::Fetch {
                            release: r,
                            how: HandOff::Wizard,
                            elevate: false,
                        },
                    );
                }
            }
            // Up to date, or offline - which a clinical workstation often
            // is. Neither is worth a line.
            Online::Latest(_) | Online::Failed(_) => {}
        }
    }

    /// `--update`: what is installed, what is newest, and one button.
    fn update_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let current = existing::preferred(&self.installed, None, None).cloned();
        match &current {
            Some(i) => {
                ui.label(RichText::new("Installed").strong());
                ui.label(i.describe());
            }
            None => {
                ui.label(format!("{APP_NAME} is not installed on this machine."));
            }
        }
        ui.add_space(10.0);
        let state = self.online.lock().unwrap().clone();
        match state {
            Online::Checking => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Looking for the newest release");
                });
                ctx.request_repaint_after(Duration::from_millis(200));
            }
            Online::Failed(e) => {
                *self.exit.lock().unwrap() = EXIT_NO_NETWORK;
                ui.colored_label(RED, "Could not look up the newest release.");
                ui.label(RichText::new(e).weak());
                ui.add_space(6.0);
                if ui.button("Try again").clicked() {
                    *self.exit.lock().unwrap() = EXIT_OK;
                    self.check_online();
                }
            }
            Online::Latest(r) => {
                let newer = current.as_ref().is_none_or(|i| r.is_newer_than(&i.version));
                if newer {
                    ui.label(RichText::new("Newest release").strong());
                    ui.label(r.version.clone());
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "The installer is downloaded from the project's GitHub releases \
                             and checked against the release's SHA256SUMS before it runs. It \
                             updates the installation in place: same folder, same options.",
                        )
                        .weak(),
                    );
                    ui.add_space(8.0);
                    let label = if current.is_some() {
                        format!("Update to {}", r.version)
                    } else {
                        format!("Install {}", r.version)
                    };
                    if ui.button(label).clicked() {
                        let (how, elevate) = match &current {
                            Some(i) => (HandOff::Window, i.machine_wide()),
                            // Nothing to update: the new setup asks its
                            // questions as for any first installation.
                            None => (HandOff::Wizard, false),
                        };
                        self.start(
                            ctx,
                            Work::Fetch {
                                release: r,
                                how,
                                elevate,
                            },
                        );
                    }
                } else {
                    ui.colored_label(
                        GREEN,
                        format!("{} is the newest release. Nothing to do.", r.version),
                    );
                }
            }
        }
        ui.add_space(12.0);
        if ui.button("Close").clicked() {
            self.quit = true;
        }
    }

    fn options(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.label(RichText::new("Install for").strong());
            let mut scope = self.opts.scope;
            for choice in [Scope::CurrentUser, Scope::AllUsers] {
                ui.radio_value(&mut scope, choice, choice.label());
            }
            if scope != self.opts.scope {
                self.opts.set_scope(scope);
                self.dir_text = self.opts.dir.to_string_lossy().to_string();
            }
            if self.opts.scope == Scope::AllUsers && !crate::win::is_elevated() {
                ui.colored_label(
                    AMBER,
                    "Administrator rights will be requested when the installation starts.",
                );
            }

            ui.add_space(10.0);
            ui.label(RichText::new("Destination folder").strong());
            ui.horizontal(|ui| {
                let w = ui.available_width() - 90.0;
                if ui
                    .add_sized(
                        [w.max(200.0), 22.0],
                        egui::TextEdit::singleline(&mut self.dir_text),
                    )
                    .changed()
                {
                    self.opts.set_dir(PathBuf::from(self.dir_text.trim()));
                }
                if ui.button("Browse").clicked() {
                    if let Some(d) = rfd::FileDialog::new()
                        .set_title("Choose the installation folder")
                        .pick_folder()
                    {
                        self.opts.set_dir(d.join(APP_NAME));
                        self.dir_text = self.opts.dir.to_string_lossy().to_string();
                    }
                }
            });
            ui.label(
                RichText::new(format!(
                    "About {} of program files",
                    human_size(self.payload_size)
                ))
                .weak(),
            );

            self.others_block(ui);

            ui.add_space(10.0);
            ui.label(RichText::new("Integration").strong());
            ui.checkbox(
                &mut self.opts.add_to_path,
                "Add the program folder to PATH (the tools run from any command prompt)",
            );
            ui.checkbox(
                &mut self.opts.start_menu_shortcut,
                "Start menu: Proton DICOM prompt and Update",
            );
            ui.checkbox(
                &mut self.opts.desktop_shortcut,
                "Desktop shortcut to the Proton DICOM prompt",
            );
            ui.checkbox(
                &mut self.opts.folder_menu,
                "Add \"Open Proton DICOM prompt here\" to folders",
            )
            .on_hover_text(
                "Right-click a folder of radiographs or CT data to open a command prompt \
                 in it, with every tool ready to run.",
            );

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.button("◀  Back").clicked() {
                    self.screen = Screen::Welcome;
                }
                let label = if self.same().is_some() {
                    "Update"
                } else {
                    "Install"
                };
                if ui.button(label).clicked() {
                    self.opts.set_dir(PathBuf::from(self.dir_text.trim()));
                    self.begin(ctx);
                }
                if ui.button("Cancel").clicked() {
                    self.quit = true;
                }
            });
            if let Some(err) = &self.error {
                ui.add_space(6.0);
                ui.colored_label(RED, err);
            }
        });
    }

    fn working(&mut self, ui: &mut egui::Ui) {
        let (frac, msg) = self.progress.lock().unwrap().clone();
        ui.add(egui::ProgressBar::new(frac).show_percentage().animate(true));
        ui.add_space(4.0);
        ui.label(msg);
        ui.add_space(10.0);
        log_view(ui, &self.log);
        ui.add_space(8.0);
        let cancellable = !matches!(self.work, Some(Work::Uninstall) | None);
        if cancellable && ui.button("Cancel").clicked() {
            self.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn done(&mut self, ui: &mut egui::Ui) {
        let fetched = matches!(self.work, Some(Work::Fetch { .. }));
        let installed = self.is_install() && !fetched;
        let updated = installed && self.same().is_some();
        match &self.error {
            None if installed => {
                ui.colored_label(
                    GREEN,
                    RichText::new(if updated {
                        "Update complete"
                    } else {
                        "Installation complete"
                    })
                    .heading(),
                );
                ui.label(format!("Installed into {}", self.opts.dir.display()));
                if self.opts.add_to_path {
                    ui.add_space(4.0);
                    ui.label(
                        "The tools are on PATH in every command prompt opened from now on, \
                         for example:",
                    );
                    if let Some(t) = self.tools.first() {
                        ui.label(RichText::new(format!("{} --help", t.name)).monospace());
                    }
                }
                if self.passive {
                    ui.label(RichText::new("This window closes by itself.").weak());
                }
            }
            None => {
                ui.colored_label(GREEN, RichText::new("Uninstall complete").heading());
            }
            Some(err) => {
                let heading = if fetched {
                    "Download failed"
                } else if updated {
                    "Update failed"
                } else if installed {
                    "Installation failed"
                } else {
                    "Uninstall failed"
                };
                ui.colored_label(RED, RichText::new(heading).heading());
                ui.add_space(4.0);
                ui.label(err.clone());
            }
        }
        ui.add_space(8.0);
        log_view(ui, &self.log);
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if installed && self.error.is_none() {
                ui.checkbox(
                    &mut self.opts.launch_after,
                    "Open the Proton DICOM prompt now",
                );
            }
            if ui.button("Close").clicked() {
                if installed && self.error.is_none() && self.opts.launch_after {
                    self.open_prompt();
                }
                self.quit = true;
            }
        });
    }

    fn confirm_uninstall(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Job::Uninstall { target } = &self.job else {
            return;
        };
        ui.label(format!(
            "This removes {APP_NAME} from {}.",
            target.manifest.install_dir.display()
        ));
        ui.label(
            RichText::new(
                "Only what the setup installed is removed; files you put into the folder \
                 yourself stay.",
            )
            .weak(),
        );
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button("Uninstall").clicked() {
                self.start(ctx, Work::Uninstall);
            }
            if ui.button("Cancel").clicked() {
                self.quit = true;
            }
        });
    }
}

fn log_view(ui: &mut egui::Ui, log: &Arc<Mutex<Vec<String>>>) {
    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .max_height(260.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for line in log.lock().unwrap().iter() {
                ui.label(RichText::new(line).monospace().weak());
            }
        });
}
