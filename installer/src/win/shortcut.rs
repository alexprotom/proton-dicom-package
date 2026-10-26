//! Windows shell links (`.lnk`), written through the shell's own COM
//! interfaces so the files are byte-for-byte what Explorer would produce.

use std::path::Path;

use anyhow::{Context, Result};
use windows::core::{Interface, HSTRING, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

/// Everything a shortcut needs. `args` and `description` may be empty.
pub struct Shortcut<'a> {
    pub link: &'a Path,
    pub target: &'a Path,
    pub args: &'a str,
    pub working_dir: &'a Path,
    pub description: &'a str,
    /// The file whose first icon the shortcut shows; `None` for the
    /// target's own. The prompt shortcuts start `cmd.exe` but show the
    /// package's icon, which the setup program carries.
    pub icon: Option<&'a Path>,
}

/// Initialise COM for the calling thread (idempotent; a second call on an
/// already-initialised thread is harmless).
pub fn init_com() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

/// Create (or overwrite) a `.lnk` file.
pub fn create(s: &Shortcut) -> Result<()> {
    init_com();
    if let Some(parent) = s.link.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .context("create ShellLink COM object")?;
        let target = HSTRING::from(s.target.as_os_str());
        link.SetPath(PCWSTR(target.as_ptr())).context("SetPath")?;
        if !s.args.is_empty() {
            let args = HSTRING::from(s.args);
            link.SetArguments(PCWSTR(args.as_ptr()))
                .context("SetArguments")?;
        }
        let dir = HSTRING::from(s.working_dir.as_os_str());
        link.SetWorkingDirectory(PCWSTR(dir.as_ptr()))
            .context("SetWorkingDirectory")?;
        if !s.description.is_empty() {
            let desc = HSTRING::from(s.description);
            link.SetDescription(PCWSTR(desc.as_ptr()))
                .context("SetDescription")?;
        }
        let icon = s.icon.map(|p| HSTRING::from(p.as_os_str()));
        link.SetIconLocation(PCWSTR(icon.as_ref().unwrap_or(&target).as_ptr()), 0)
            .context("SetIconLocation")?;
        let persist: IPersistFile = link.cast().context("IPersistFile")?;
        let path = HSTRING::from(s.link.as_os_str());
        persist
            .Save(PCWSTR(path.as_ptr()), true)
            .with_context(|| format!("save {}", s.link.display()))?;
    }
    Ok(())
}
