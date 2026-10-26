//! Product constants, install options, exit codes and the defaults the UI
//! starts from.

use std::path::{Path, PathBuf};

use crate::win::registry::Hive;

pub use crate::product::*;

/// Shown in Apps & features. Empty means "do not write the value".
pub const HOMEPAGE: &str = "https://github.com/alexprotom/proton-dicom-package";
/// The setup program itself, left in the program folder without its
/// payload: Apps & features runs it to uninstall (`--uninstall`) and the
/// *Update* shortcut runs it to fetch the newest release (`--update`).
pub const SETUP_EXE: &str = "pdp-setup.exe";
/// Where a [`SETUP_EXE`] that was still running when an update replaced it
/// is moved aside (Windows lets a running program be renamed, not
/// overwritten). Deleted by the next installation, update or uninstall.
pub const SETUP_EXE_OLD: &str = "pdp-setup.exe.old";
pub const MANIFEST_FILE: &str = "install-manifest.txt";

/// Exit codes. Anything but 0 is a failure; the specific ones are listed in
/// the winget manifest (`ExpectedReturnCodes`), so winget can tell the user
/// what happened instead of printing a number.
pub const EXIT_OK: u8 = 0;
/// Any failure without a more specific code.
pub const EXIT_FAILED: u8 = 1;
/// A tool is running from the folder being installed into or removed.
pub const EXIT_IN_USE: u8 = 2;
/// The user cancelled.
pub const EXIT_CANCELLED: u8 = 3;
/// `--silent` would replace a newer installed version (see
/// `--allow-downgrade`).
pub const EXIT_DOWNGRADE: u8 = 4;
/// The newest release could not be looked up or downloaded.
pub const EXIT_NO_NETWORK: u8 = 5;

/// An error that carries its own exit code.
#[derive(Debug)]
pub struct Failure {
    pub code: u8,
    pub message: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

/// An `anyhow` error that `main` turns into exit code `code`.
pub fn failure(code: u8, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Failure {
        code,
        message: message.into(),
    })
}

/// The exit code an error stands for: the first [`Failure`] in its chain,
/// else [`EXIT_FAILED`].
pub fn exit_code_of(e: &anyhow::Error) -> u8 {
    e.chain()
        .find_map(|c| c.downcast_ref::<Failure>())
        .map(|f| f.code)
        .unwrap_or(EXIT_FAILED)
}

/// Who the installation is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    /// `%LOCALAPPDATA%\Programs\…` - no administrator rights needed.
    CurrentUser,
    /// `C:\Program Files\…` - needs elevation.
    AllUsers,
}

impl Scope {
    pub fn hive(self) -> Hive {
        match self {
            Scope::CurrentUser => Hive::CurrentUser,
            Scope::AllUsers => Hive::LocalMachine,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Scope::CurrentUser => "Just me (no administrator rights needed)",
            Scope::AllUsers => "All users (requires administrator rights)",
        }
    }
}

/// Everything the user can decide before the copy starts.
#[derive(Clone, Debug)]
pub struct Options {
    pub scope: Scope,
    pub dir: PathBuf,
    /// Start menu: the Proton DICOM prompt and *Update*.
    pub start_menu_shortcut: bool,
    /// Desktop: the Proton DICOM prompt.
    pub desktop_shortcut: bool,
    /// Put the program folder on `PATH`, so every tool runs from any prompt.
    pub add_to_path: bool,
    /// "Open Proton DICOM prompt here" on folders.
    pub folder_menu: bool,
    /// Open the prompt when the wizard closes.
    pub launch_after: bool,
    /// Remove every other registered installation - another folder, or the
    /// other scope - so that one copy remains. The installation in [`dir`]
    /// itself is always updated in place.
    ///
    /// [`dir`]: Options::dir
    pub remove_others: bool,
}

impl Default for Options {
    fn default() -> Self {
        let scope = Scope::CurrentUser;
        Options {
            dir: default_install_dir(scope),
            scope,
            start_menu_shortcut: true,
            // Off: a command-line package lives in prompts, not on the desktop.
            desktop_shortcut: false,
            // On: command-line tools are of little use until a prompt finds them.
            add_to_path: true,
            folder_menu: true,
            launch_after: true,
            // On: a second copy is almost always a leftover, and one copy
            // per machine is what the Update shortcut and winget assume.
            remove_others: true,
        }
    }
}

impl Options {
    /// The setup program kept in the program folder, see [`SETUP_EXE`].
    pub fn setup_path(&self) -> PathBuf {
        self.dir.join(SETUP_EXE)
    }

    /// The prompt script, see [`SHELL_CMD`].
    pub fn shell_path(&self) -> PathBuf {
        self.dir.join(SHELL_CMD)
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.dir.join(MANIFEST_FILE)
    }

    pub fn set_dir(&mut self, dir: PathBuf) {
        self.dir = dir;
    }

    /// Switching scope moves the default folder along with it, unless the
    /// user has already typed a path of their own.
    pub fn set_scope(&mut self, scope: Scope) {
        let dir_was_default = self.dir == default_install_dir(self.scope);
        self.scope = scope;
        if dir_was_default {
            self.dir = default_install_dir(scope);
        }
    }
}

/// `%LOCALAPPDATA%\Programs\Proton DICOM Package` or `%ProgramFiles%\Proton
/// DICOM Package`, with a plain-C: fallback should the shell folder lookup
/// fail.
pub fn default_install_dir(scope: Scope) -> PathBuf {
    match scope {
        Scope::CurrentUser => crate::win::local_app_data()
            .unwrap_or_else(|_| PathBuf::from(r"C:\Users\Public"))
            .join("Programs")
            .join(APP_NAME),
        Scope::AllUsers => crate::win::program_files()
            .unwrap_or_else(|_| PathBuf::from(r"C:\Program Files"))
            .join(APP_NAME),
    }
}

/// `cmd.exe`, by its full path: registry commands and shortcuts are not run
/// through a shell that would search `PATH` for it.
pub fn cmd_exe() -> PathBuf {
    std::env::var_os("ComSpec")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
                .join("System32")
                .join("cmd.exe")
        })
}

/// Arguments that make `cmd.exe` run the prompt script, optionally in
/// `folder` (`%V` for a context-menu verb).
///
/// `cmd /k` strips the first and the last quote of what follows when there
/// are more than two, so the whole command is wrapped once more:
/// `/k ""C:\…\pdp-shell.cmd" "D:\data""` runs `"C:\…\pdp-shell.cmd" "D:\data"`.
pub fn shell_args(script: &Path, folder: Option<&str>) -> String {
    match folder {
        Some(f) => format!("/k \"\"{}\" \"{f}\"\"", script.display()),
        None => format!("/k \"\"{}\"\"", script.display()),
    }
}

/// Add/Remove Programs key path for this product.
pub fn uninstall_key_path() -> String {
    format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{PRODUCT_ID}")
}

/// The two registry keys of the folder context-menu entry, below
/// `Software\Classes`: on a folder, and on the empty space inside one.
pub fn folder_menu_keys() -> [String; 2] {
    [
        format!(r"Software\Classes\Directory\shell\{PRODUCT_ID}"),
        format!(r"Software\Classes\Directory\Background\shell\{PRODUCT_ID}"),
    ]
}

/// Human-readable byte size, e.g. `1.3 GB`.
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1_000.0;
    let b = bytes as f64;
    if b >= KB * KB * KB {
        format!("{:.1} GB", b / (KB * KB * KB))
    } else if b >= KB * KB {
        format!("{:.0} MB", b / (KB * KB))
    } else if b >= KB {
        format!("{:.0} kB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_command_survives_cmd_quote_stripping() {
        let script = Path::new(r"C:\Program Files\Proton DICOM Package\pdp-shell.cmd");
        assert_eq!(
            shell_args(script, None),
            r#"/k ""C:\Program Files\Proton DICOM Package\pdp-shell.cmd"""#
        );
        assert_eq!(
            shell_args(script, Some("%V")),
            r#"/k ""C:\Program Files\Proton DICOM Package\pdp-shell.cmd" "%V"""#
        );
    }

    #[test]
    fn a_command_line_package_goes_on_path_by_default() {
        let o = Options::default();
        assert!(o.add_to_path);
        assert!(o.start_menu_shortcut && o.folder_menu);
        assert!(!o.desktop_shortcut);
    }
}
