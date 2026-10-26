//! `pdp-setup` - the Windows installer for the Proton DICOM Package
//! command-line tools.
//!
//! Run without arguments it shows a small wizard; `--silent` and `--console`
//! drive the same code from a terminal, and `--uninstall` (the form recorded
//! in Apps & features) removes an installation again.
//!
//! The binary is built as a GUI-subsystem program so double-clicking it does
//! not flash a console window; the text interface attaches to the parent
//! console explicitly.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
compile_error!("pdp-setup installs a Windows application and only builds for Windows targets");

mod console;
mod existing;
mod install;
mod payload;
mod plan;
mod product;
mod ui;
mod uninstall;
mod update;
mod win;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Result};

use payload::Payload;
use plan::*;

const USAGE: &str = "\
Proton DICOM Package setup

USAGE:
    pdp-setup [OPTIONS]
    pdp-setup --update [--silent | --console]
    pdp-setup --uninstall [--silent]

Run over an existing installation, the setup updates it in place: same
folder, same choices. Options given here override the choices the
installation was made with.

INSTALL OPTIONS:
    --dir <PATH>          destination folder
    --all-users           install for all users (needs administrator rights)
    --just-me             install for the current user only (default)
    --no-path             do not add the program folder to PATH
    --add-to-path         add the program folder to PATH (default)
    --no-start-menu       skip the Start-menu shortcuts
    --desktop-shortcut    add a desktop shortcut to the Proton DICOM prompt
    --no-desktop-shortcut no desktop shortcut (default)
    --no-folder-menu      skip \"Open Proton DICOM prompt here\" on folders
    --no-launch           do not offer to open the prompt afterwards
    --keep-others         keep installations in other folders or the other
                          scope (by default they are removed: one copy remains)
    --allow-downgrade     let --silent or --passive replace a newer version

UPDATE:
    --update              download the newest release from GitHub, check it
                          against the release's SHA256SUMS and install it over
                          the existing installation (exit code 0 when already
                          up to date)

UNINSTALL OPTIONS:
    --uninstall           remove an existing installation
    --from <PATH>         installation folder (default: the setup program's own)

GENERAL:
    --silent              no window and no questions
    --passive             a progress window only: no questions, closes itself
    --console             text interface instead of the wizard
    -h, --help            show this help

EXIT CODES:
    0 done or already up to date, 1 failed, 2 a tool is running,
    3 cancelled, 4 a newer version is installed, 5 no connection to GitHub
";

#[derive(Default)]
struct Args {
    help: bool,
    silent: bool,
    passive: bool,
    console: bool,
    elevated: bool,
    /// Begin at once, in a window (the hand-over from `--update`).
    autostart: bool,
    uninstall: bool,
    update: bool,
    allow_downgrade: bool,
    from: Option<PathBuf>,
    /// The install options given on the command line, in order. They are
    /// applied on top of whatever the installation being updated was made
    /// with, so they cannot be resolved into [`Options`] until that is known.
    install_flags: Vec<String>,
    /// `install_flags` applied to the defaults - what a first installation
    /// gets, and proof at parse time that every flag is valid.
    opts: Option<Options>,
}

fn parse_args() -> Result<Args> {
    parse_from(std::env::args().skip(1))
}

/// Install options that take a value.
const VALUE_FLAGS: [&str; 1] = ["--dir"];
/// Install options that stand alone.
const SWITCHES: [&str; 10] = [
    "--all-users",
    "--just-me",
    "--no-start-menu",
    "--desktop-shortcut",
    "--no-desktop-shortcut",
    "--no-folder-menu",
    "--add-to-path",
    "--no-path",
    "--no-launch",
    "--keep-others",
];

/// The command line, taken from an iterator so the round trip through
/// [`args_for_relaunch`] can be tested: everything the user chose in the
/// first window has to survive the elevated re-launch, and a flag quietly
/// dropped there loses a choice without any sign of it.
fn parse_from(args: impl Iterator<Item = String>) -> Result<Args> {
    let mut a = Args::default();
    let mut it = args;
    let next = |it: &mut dyn Iterator<Item = String>, flag: &str| -> Result<String> {
        it.next()
            .ok_or_else(|| anyhow::anyhow!("{flag} needs a value"))
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" | "/?" => a.help = true,
            "--silent" | "/S" | "/silent" => a.silent = true,
            "--passive" => a.passive = true,
            "--console" => a.console = true,
            "--elevated" => a.elevated = true,
            "--autostart" => a.autostart = true,
            "--uninstall" | "/uninstall" => a.uninstall = true,
            "--update" => a.update = true,
            "--allow-downgrade" => a.allow_downgrade = true,
            "--from" => a.from = Some(PathBuf::from(next(&mut it, "--from")?)),
            flag if VALUE_FLAGS.contains(&flag) => {
                let value = next(&mut it, flag)?;
                a.install_flags.push(arg);
                a.install_flags.push(value);
            }
            flag if SWITCHES.contains(&flag) => a.install_flags.push(arg),
            other => bail!("unknown option '{other}'\n\n{USAGE}"),
        }
    }
    let mut opts = Options::default();
    apply_install_flags(&mut opts, &a.install_flags)?;
    a.opts = Some(opts);
    Ok(a)
}

/// Apply install options, as collected by [`parse_from`], on top of `opts`.
fn apply_install_flags(opts: &mut Options, flags: &[String]) -> Result<()> {
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        let mut value = || -> Result<&String> {
            it.next()
                .ok_or_else(|| anyhow::anyhow!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--dir" => opts.set_dir(PathBuf::from(value()?)),
            "--all-users" => opts.set_scope(Scope::AllUsers),
            "--just-me" => opts.set_scope(Scope::CurrentUser),
            "--no-start-menu" => opts.start_menu_shortcut = false,
            "--desktop-shortcut" => opts.desktop_shortcut = true,
            "--no-desktop-shortcut" => opts.desktop_shortcut = false,
            "--no-folder-menu" => opts.folder_menu = false,
            "--add-to-path" => opts.add_to_path = true,
            "--no-path" => opts.add_to_path = false,
            "--no-launch" => opts.launch_after = false,
            "--keep-others" => opts.remove_others = false,
            other => bail!("unknown option '{other}'"),
        }
    }
    Ok(())
}

/// The scope and folder the command line asks for, if it does - which
/// existing installation to start from depends on them.
fn explicit_target(flags: &[String]) -> (Option<Scope>, Option<PathBuf>) {
    let mut scope = None;
    let mut dir = None;
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--all-users" => scope = Some(Scope::AllUsers),
            "--just-me" => scope = Some(Scope::CurrentUser),
            "--dir" => dir = it.next().map(PathBuf::from),
            f if VALUE_FLAGS.contains(&f) => {
                it.next();
            }
            _ => {}
        }
    }
    (scope, dir)
}

/// Re-serialise the chosen options for the elevated re-launch.
pub fn args_for_relaunch(o: &Options) -> String {
    let mut s = format!("--elevated --dir \"{}\"", o.dir.display());
    s.push_str(match o.scope {
        Scope::AllUsers => " --all-users",
        Scope::CurrentUser => " --just-me",
    });
    if !o.start_menu_shortcut {
        s.push_str(" --no-start-menu");
    }
    s.push_str(if o.desktop_shortcut {
        " --desktop-shortcut"
    } else {
        " --no-desktop-shortcut"
    });
    if !o.folder_menu {
        s.push_str(" --no-folder-menu");
    }
    s.push_str(if o.add_to_path {
        " --add-to-path"
    } else {
        " --no-path"
    });
    if !o.remove_others {
        s.push_str(" --keep-others");
    }
    // The elevated process must not open the prompt: it would inherit the
    // administrator token. The first (unelevated) window is gone by then, so
    // the user opens it from the shortcut instead.
    s.push_str(" --no-launch");
    // The user already saw the downgrade warning in the first window and
    // went on; the elevated run must not refuse what they confirmed.
    s.push_str(" --allow-downgrade");
    s
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            win::attach_console();
            eprintln!("{e:#}");
            return ExitCode::FAILURE;
        }
    };
    if args.help {
        win::attach_console();
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let text_mode = args.silent || args.console;
    if text_mode {
        win::attach_console();
    }
    let result = if args.uninstall {
        do_uninstall(&args).map(|()| EXIT_OK)
    } else if args.update {
        do_update(&args)
    } else {
        do_install(&args)
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            let msg = format!("{e:#}");
            if text_mode {
                eprintln!("\nError: {msg}");
            } else {
                win::message_box(&format!("{APP_NAME} setup"), &msg);
            }
            ExitCode::from(exit_code_of(&e))
        }
    }
}

fn do_install(args: &Args) -> Result<u8> {
    let payload = Payload::locate()?;
    let version = install::payload_version(&payload);
    // Start from the installation this run will update, if there is one, so
    // an update changes nothing the command line does not ask to change.
    let installed = existing::find_all();
    let (want_scope, want_dir) = explicit_target(&args.install_flags);
    // A folder with no installation in it is a move: the choices still come
    // from the installation being replaced, only the folder is new.
    let mut opts = existing::preferred(&installed, want_scope, want_dir.as_deref())
        .or_else(|| existing::preferred(&installed, want_scope, None))
        .map(existing::options_from)
        .unwrap_or_default();
    apply_install_flags(&mut opts, &args.install_flags)?;
    if args.passive {
        opts.launch_after = false;
    }
    // Nobody is asked in these modes, so nobody can confirm replacing a
    // newer version with this older one.
    if (args.silent || args.passive) && !args.allow_downgrade {
        let (same, _) = existing::split(&installed, &opts.dir);
        if let Some(prev) = same {
            if compare_versions(&version, &prev.version) == std::cmp::Ordering::Less {
                return Err(failure(
                    EXIT_DOWNGRADE,
                    format!(
                        "{} is newer than this setup ({version}); pass --allow-downgrade \
                         to replace it anyway",
                        prev.describe()
                    ),
                ));
            }
        }
    }
    // A machine-wide installation needs elevation. The wizard asks for it when
    // the user presses Install; the silent and console paths have no way to ask,
    // so they fail here instead.
    if opts.scope == Scope::AllUsers && !win::is_elevated() && (args.silent || args.console) {
        bail!(
            "a machine-wide installation needs administrator rights - start the installer \
             from an elevated prompt, or install with --just-me"
        );
    }
    if args.silent || args.console {
        return console::run_install(payload, opts, args.silent, args.allow_downgrade);
    }
    let mode = ui::Mode {
        autostart: args.elevated || args.autostart || args.passive,
        passive: args.passive,
    };
    match ui::run_install(payload, opts.clone(), mode, installed) {
        Ok(code) => Ok(code),
        Err(gui_err) => {
            // No window? Carry on in the terminal rather than failing outright.
            win::attach_console();
            eprintln!("{gui_err:#}\nFalling back to the text interface.\n");
            let payload = Payload::locate()?;
            console::run_install(payload, opts, args.passive, args.allow_downgrade)
        }
    }
}

fn do_update(args: &Args) -> Result<u8> {
    if args.silent || args.console {
        return console::run_update(args.silent);
    }
    match ui::run_update() {
        Ok(code) => Ok(code),
        Err(gui_err) => {
            win::attach_console();
            eprintln!("{gui_err:#}\nFalling back to the text interface.\n");
            console::run_update(false)
        }
    }
}

fn do_uninstall(args: &Args) -> Result<()> {
    let target = uninstall::discover(args.from.clone())?;
    // A machine-wide installation lives in Program Files and owns HKLM keys;
    // Apps & features starts the uninstaller unelevated, so ask for rights.
    if target.manifest.machine_wide && !win::is_elevated() {
        let mut relaunch = format!("--uninstall --from \"{}\"", target.dir.display());
        if args.silent {
            relaunch.push_str(" --silent");
        }
        win::relaunch_elevated(&relaunch)?;
        return Ok(());
    }
    // The setup program lives in the folder it has to delete: continue from a
    // copy in %TEMP%, which can remove the original.
    if uninstall::running_from_target(&target) {
        let mut extra: Vec<&str> = Vec::new();
        if args.silent {
            extra.push("--silent");
        }
        if args.console {
            extra.push("--console");
        }
        uninstall::relaunch_from_temp(&target, &extra)?;
        return Ok(());
    }
    // Running from %TEMP%: clean the copy up at the next boot.
    if let Ok(exe) = std::env::current_exe() {
        if exe.starts_with(std::env::temp_dir()) {
            win::delete_on_reboot(&exe);
        }
    }
    if args.silent || args.console {
        return console::run_uninstall(target, args.silent);
    }
    match ui::run_uninstall(target) {
        Ok(()) => Ok(()),
        Err(gui_err) => {
            win::attach_console();
            eprintln!("{gui_err:#}\nFalling back to the text interface.\n");
            let target = uninstall::discover(args.from.clone())?;
            console::run_uninstall(target, false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Args {
        // Good enough for the strings `args_for_relaunch` produces: the only
        // quoted values are the two paths.
        let mut words: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut quoted = false;
        for c in line.chars() {
            match c {
                '"' => quoted = !quoted,
                ' ' if !quoted => {
                    if !cur.is_empty() {
                        words.push(std::mem::take(&mut cur));
                    }
                }
                _ => cur.push(c),
            }
        }
        if !cur.is_empty() {
            words.push(cur);
        }
        parse_from(words.into_iter()).expect("the installer must be able to read its own output")
    }

    /// The user answers every page in the first window; a machine-wide
    /// installation then throws that window away and starts again as
    /// administrator with nothing but this command line. Anything missing
    /// from it is a choice silently lost.
    #[test]
    fn every_choice_survives_the_elevated_relaunch() {
        for bits in 0..32u32 {
            let bit = |i: u32| bits & (1 << i) != 0;
            let before = Options {
                dir: PathBuf::from(r"D:\Apps\Proton DICOM Package"),
                scope: Scope::AllUsers,
                start_menu_shortcut: bit(0),
                desktop_shortcut: bit(1),
                add_to_path: bit(2),
                folder_menu: bit(3),
                launch_after: true,
                remove_others: bit(4),
            };
            let after = parse(&args_for_relaunch(&before))
                .opts
                .expect("parse_from always fills in the options");
            assert_eq!(after.dir, before.dir);
            assert_eq!(after.scope, before.scope);
            assert_eq!(after.start_menu_shortcut, before.start_menu_shortcut);
            assert_eq!(after.desktop_shortcut, before.desktop_shortcut);
            assert_eq!(after.add_to_path, before.add_to_path, "PATH");
            assert_eq!(after.folder_menu, before.folder_menu);
            assert_eq!(after.remove_others, before.remove_others, "other copies");
            // The one deliberate difference: the elevated process must
            // not open the prompt, or it would inherit the token.
            assert!(!after.launch_after);
        }
    }

    /// An update starts from what the installation was made with; the
    /// command line only overrides what it names.
    #[test]
    fn flags_override_only_what_they_name() {
        let mut base = Options {
            dir: PathBuf::from(r"D:\Apps\PDP"),
            desktop_shortcut: true,
            add_to_path: false,
            ..Options::default()
        };
        let a = parse_from(
            ["--silent", "--no-start-menu", "--no-folder-menu"]
                .map(String::from)
                .into_iter(),
        )
        .unwrap();
        apply_install_flags(&mut base, &a.install_flags).unwrap();
        assert_eq!(
            base.dir,
            PathBuf::from(r"D:\Apps\PDP"),
            "the folder is kept"
        );
        assert!(base.desktop_shortcut, "the desktop shortcut stays on");
        assert!(!base.add_to_path, "PATH stays off");
        assert!(!base.start_menu_shortcut, "the flag was applied");
        assert!(!base.folder_menu, "the flag was applied");
        assert!(a.silent, "general flags are not install flags");
    }

    #[test]
    fn the_target_on_the_command_line_is_found() {
        let a = parse_from(
            ["--all-users", "--dir", r"E:\x", "--just-me"]
                .map(String::from)
                .into_iter(),
        )
        .unwrap();
        let (scope, dir) = explicit_target(&a.install_flags);
        assert_eq!(scope, Some(Scope::CurrentUser), "the last scope wins");
        assert_eq!(dir, Some(PathBuf::from(r"E:\x")));
        let a = parse_from(["--update", "--silent"].map(String::from).into_iter()).unwrap();
        assert!(a.update && a.silent);
        assert_eq!(explicit_target(&a.install_flags), (None, None));
        assert!(parse_from(["--models".to_string()].into_iter()).is_err());
        assert!(parse_from(["--dir".to_string()].into_iter()).is_err());
    }
}
