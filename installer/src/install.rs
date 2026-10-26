//! The install itself: copy the payload, leave the setup program behind (it
//! uninstalls and updates), create the prompt shortcuts, and write the
//! registry entries - Apps & features, the folder context menu and `PATH`.
//!
//! The tools need nothing from outside the payload: they link the C runtime
//! statically (`.cargo/config.toml`), so there is no Visual C++ runtime to
//! fetch.
//!
//! Every filesystem and registry change is recorded in `install-manifest.txt`
//! so the uninstaller can undo exactly what was done and nothing else.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context, Result};

use crate::payload::Payload;
use crate::plan::*;
use crate::win::registry::{self, Key};
use crate::win::shortcut::{self, Shortcut};

/// Progress and log messages from the worker thread to whatever UI is driving.
pub enum Event {
    /// Overall progress in `0..=1` plus a one-line status.
    Progress(f32, String),
    /// A line for the detail log.
    Log(String),
}

pub type Sink<'a> = &'a (dyn Fn(Event) + Sync);

/// What the uninstaller needs to know, in the same order it should undo it.
/// An update reads the previous one to find what the new version no longer
/// ships and which choices were made last time.
#[derive(Clone, Debug, Default)]
pub struct Manifest {
    pub version: String,
    pub install_dir: PathBuf,
    pub machine_wide: bool,
    pub path_added: bool,
    pub folder_menu: bool,
    /// Relative to the install directory, `/`-separated.
    pub files: Vec<String>,
    /// Absolute paths of `.lnk` files.
    pub shortcuts: Vec<PathBuf>,
}

impl Manifest {
    pub fn render(&self) -> String {
        let mut out = String::from(
            "# proton-dicom-package install manifest - used by pdp-setup.exe to update and uninstall.\n\
             # Editing this file changes what the uninstaller removes.\n",
        );
        out.push_str(&format!("version = {}\n", self.version));
        out.push_str(&format!("install_dir = {}\n", self.install_dir.display()));
        out.push_str(&format!("machine_wide = {}\n", self.machine_wide));
        out.push_str(&format!("path_added = {}\n", self.path_added));
        out.push_str(&format!("folder_menu = {}\n", self.folder_menu));
        for s in &self.shortcuts {
            out.push_str(&format!("shortcut = {}\n", s.display()));
        }
        for f in &self.files {
            out.push_str(&format!("file = {f}\n"));
        }
        out
    }

    pub fn parse(text: &str) -> Manifest {
        let mut m = Manifest::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "version" => m.version = value.to_string(),
                "install_dir" => m.install_dir = PathBuf::from(value),
                "machine_wide" => m.machine_wide = value == "true",
                "path_added" => m.path_added = value == "true",
                "folder_menu" => m.folder_menu = value == "true",
                "shortcut" => m.shortcuts.push(PathBuf::from(value)),
                "file" => m.files.push(value.to_string()),
                _ => {}
            }
        }
        m
    }
}

/// Run the whole installation. Long steps check `cancel` between chunks.
pub fn run(opts: &Options, payload: &Payload, sink: Sink, cancel: &AtomicBool) -> Result<()> {
    let log = |s: String| sink(Event::Log(s));
    let step = |f: f32, s: &str| sink(Event::Progress(f, s.to_string()));

    // ---- preflight -------------------------------------------------------
    step(0.0, "Preparing");
    std::fs::create_dir_all(&opts.dir)
        .with_context(|| format!("create install directory {}", opts.dir.display()))?;
    check_writable(&opts.dir)?;
    let tools = payload_executables(payload);
    if let Some(busy) = running_in(&opts.dir, &tools) {
        return Err(failure(
            EXIT_IN_USE,
            format!(
                "{busy} is running from {} - wait for it to finish (or close it) and start \
                 the installer again",
                opts.dir.display()
            ),
        ));
    }
    // A setup program an earlier update had to move aside while it ran.
    let _ = std::fs::remove_file(opts.dir.join(SETUP_EXE_OLD));

    let version = payload_version(payload);
    let installed = crate::existing::find_all();
    let (same, others) = crate::existing::split(&installed, &opts.dir);
    match &same {
        Some(prev) => log(format!("Updating {} to {version}", prev.describe())),
        None => log(format!("Installing {version} into {}", opts.dir.display())),
    }

    let mut manifest = Manifest {
        version,
        install_dir: opts.dir.clone(),
        machine_wide: opts.scope == Scope::AllUsers,
        ..Default::default()
    };

    // ---- other installations ----------------------------------------------
    // One copy per machine: a previous installation in another folder, or in
    // the other scope, goes before this one is written - including its
    // Apps & features entry, which may be the very key this one is about to
    // create.
    if opts.remove_others {
        for other in &others {
            if other.machine_wide() && !crate::win::is_elevated() {
                log(format!(
                    "Kept {}: removing an installation for all users needs administrator rights",
                    other.describe()
                ));
                continue;
            }
            step(0.02, "Removing the other installation");
            log(format!("Removing {}", other.describe()));
            let target = crate::uninstall::Target {
                dir: other.dir.clone(),
                manifest: other.manifest.clone(),
            };
            let nested = |ev: Event| {
                if let Event::Log(line) = ev {
                    sink(Event::Log(format!("  {line}")));
                }
            };
            crate::uninstall::run(&target, &nested)
                .with_context(|| format!("remove {}", other.describe()))?;
        }
    } else {
        for other in &others {
            log(format!("Leaving {} in place", other.describe()));
        }
    }

    // ---- files -----------------------------------------------------------
    let total_bytes = payload.total_size().unwrap_or(0);
    log(format!(
        "Copying {} of program files",
        human_size(total_bytes)
    ));
    let mut last = 0.0f32;
    manifest.files = payload.extract_to(&opts.dir, &[], &mut |frac, name| {
        // The copy owns 0.05..0.75 of the bar.
        let f = 0.05 + 0.70 * frac;
        if f - last > 0.005 || frac >= 1.0 {
            last = f;
            sink(Event::Progress(f, format!("Copying {name}")));
        }
    })?;
    if cancel.load(Ordering::Relaxed) {
        return Err(failure(EXIT_CANCELLED, "cancelled"));
    }

    // The manifest is saved after every step that creates something, so an
    // installation interrupted half-way can still be uninstalled cleanly.
    let save_manifest = |m: &Manifest| -> Result<()> {
        std::fs::write(opts.manifest_path(), m.render())
            .with_context(|| format!("write {}", opts.manifest_path().display()))
    };
    // On an update, the files the new version no longer ships stay listed
    // until they are gone, so an update interrupted right here still leaves
    // an uninstaller that knows about them.
    let obsolete = same
        .as_ref()
        .map(|prev| obsolete_files(&prev.manifest.files, &manifest.files))
        .unwrap_or_default();
    let mut interim = manifest.clone();
    interim.files.extend(obsolete.iter().cloned());
    save_manifest(&interim)?;

    // ---- files the new version no longer ships ----------------------------
    // Extracting over the old files replaced everything the new version
    // carries; what it does not carry any more would otherwise stay behind
    // for good, since the new manifest no longer lists it.
    if !obsolete.is_empty() {
        step(0.76, "Removing files of the previous version");
        let removed = remove_relative(&opts.dir, &obsolete, &log);
        log(format!(
            "Removed {removed} file(s) the new version no longer ships"
        ));
    }

    // ---- the setup program ------------------------------------------------
    step(0.78, "Writing the setup program");
    write_setup_exe(payload, &opts.setup_path())?;
    manifest.files.push(SETUP_EXE.to_string());
    log(format!(
        "Setup program (uninstall, update): {}",
        opts.setup_path().display()
    ));

    save_manifest(&manifest)?;

    // ---- shortcuts --------------------------------------------------------
    step(0.80, "Creating shortcuts");
    shortcut::init_com();
    let cmd = cmd_exe();
    let prompt_args = shell_args(&opts.shell_path(), None);
    let icon = opts.setup_path();
    let prompt_name = format!("{APP_NAME} prompt.lnk");
    let prompt_description = "Command prompt with the Proton DICOM Package tools on PATH";
    if opts.start_menu_shortcut {
        let link = crate::win::start_menu_programs()?.join(&prompt_name);
        shortcut::create(&Shortcut {
            link: &link,
            target: &cmd,
            args: &prompt_args,
            working_dir: &opts.dir,
            description: prompt_description,
            icon: Some(&icon),
        })?;
        log(format!("Start menu: {}", link.display()));
        manifest.shortcuts.push(link);
        let link = crate::win::start_menu_programs()?.join(format!("Update {APP_NAME}.lnk"));
        shortcut::create(&Shortcut {
            link: &link,
            target: &opts.setup_path(),
            args: "--update",
            working_dir: &opts.dir,
            description: "Download and install the newest release",
            icon: None,
        })?;
        log(format!("Start menu: {}", link.display()));
        manifest.shortcuts.push(link);
    }
    if opts.desktop_shortcut {
        let link = crate::win::desktop_dir()?.join(&prompt_name);
        shortcut::create(&Shortcut {
            link: &link,
            target: &cmd,
            args: &prompt_args,
            working_dir: &opts.dir,
            description: prompt_description,
            icon: Some(&icon),
        })?;
        log(format!("Desktop: {}", link.display()));
        manifest.shortcuts.push(link);
    }
    // Shortcuts the previous installation had and this one does not want.
    if let Some(prev) = &same {
        for link in &prev.manifest.shortcuts {
            let kept = manifest
                .shortcuts
                .iter()
                .any(|l| crate::existing::paths_equal(l, link));
            if !kept && link.exists() {
                match std::fs::remove_file(link) {
                    Ok(()) => log(format!("Removed {}", link.display())),
                    Err(e) => log(format!("Could not remove {}: {e}", link.display())),
                }
            }
        }
    }

    save_manifest(&manifest)?;

    // ---- registry ---------------------------------------------------------
    step(0.85, "Registering the application");
    let hive = opts.scope.hive();
    // The same folder, but switched to the other scope: the old scope's
    // entries would otherwise list a second installation.
    let same_hive_prev = match &same {
        Some(prev) if prev.scope != opts.scope => {
            let prev_hive = prev.scope.hive();
            match crate::uninstall::remove_registration(&prev.manifest, prev_hive, &log) {
                Ok(()) => log(format!("Removed the entries in {}", prev_hive.label())),
                Err(e) => log(format!(
                    "Could not remove the entries in {}: {e:#}",
                    prev_hive.label()
                )),
            }
            None
        }
        other => other.as_ref().map(|p| &p.manifest),
    };
    write_uninstall_entry(opts, hive, total_bytes, &manifest.version)?;
    log(format!("Listed in Apps & features ({})", hive.label()));
    if opts.folder_menu {
        write_folder_menu(opts, hive)?;
        manifest.folder_menu = true;
        log("Added \"Open Proton DICOM prompt here\" to folders".into());
    } else if same_hive_prev.is_some_and(|m| m.folder_menu) {
        crate::uninstall::remove_folder_menu(hive)?;
        log("Removed the folder context-menu entry".into());
    }
    // `path_add` reports whether it changed anything, and on an update the
    // folder is usually on PATH already - put there by the previous
    // installation, which the manifest has to go on remembering or the
    // uninstaller would leave the entry behind.
    let path_was_ours = same_hive_prev.is_some_and(|m| m.path_added);
    let dir = opts.dir.to_string_lossy().to_string();
    if opts.add_to_path {
        if registry::path_add(hive, &dir)? {
            crate::win::broadcast_environment_change();
            log("Added the program folder to PATH".into());
            manifest.path_added = true;
        } else {
            manifest.path_added = path_was_ours;
        }
    } else if path_was_ours && registry::path_remove(hive, &dir)? {
        crate::win::broadcast_environment_change();
        log("Removed the program folder from PATH".into());
    }

    save_manifest(&manifest)?;

    step(1.0, "Installation complete");
    Ok(())
}

/// The setup program kept in the folder is this very binary with the
/// appended payload cut off, so it stays a few megabytes instead of carrying
/// a copy of the program.
fn write_setup_exe(payload: &Payload, dest: &Path) -> Result<()> {
    use std::io::{Read, Write};
    // The copy being replaced may still be running: the Update shortcut
    // starts it, and it hands over to the setup it downloaded. Windows will
    // not overwrite a running program but will rename one, so wait a moment
    // for it to exit and otherwise move it aside; the next run deletes it.
    if dest.exists() {
        let mut freed = false;
        for _ in 0..20 {
            if std::fs::remove_file(dest).is_ok() {
                freed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        if !freed {
            let aside = dest.with_file_name(SETUP_EXE_OLD);
            let _ = std::fs::remove_file(&aside);
            std::fs::rename(dest, &aside).with_context(|| format!("replace {}", dest.display()))?;
        }
    }
    let exe = std::env::current_exe()?;
    let base_len = payload.base_exe_len()?;
    let mut src = std::fs::File::open(&exe)?;
    let mut out = std::io::BufWriter::new(
        std::fs::File::create(dest).with_context(|| format!("write {}", dest.display()))?,
    );
    let mut left = base_len;
    let mut buf = vec![0u8; 1 << 20];
    while left > 0 {
        let want = buf.len().min(left as usize);
        let n = src.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        left -= n as u64;
    }
    out.flush()?;
    Ok(())
}

fn write_uninstall_entry(
    opts: &Options,
    hive: registry::Hive,
    size_bytes: u64,
    version: &str,
) -> Result<()> {
    let key = Key::create(hive.hkey(), &uninstall_key_path())?;
    let uninst = opts.setup_path();
    key.set_str("DisplayName", APP_NAME)?;
    key.set_str("DisplayVersion", version)?;
    key.set_str("Publisher", PUBLISHER)?;
    key.set_str("InstallLocation", &opts.dir.to_string_lossy())?;
    key.set_str("DisplayIcon", &format!("{},0", uninst.display()))?;
    key.set_str(
        "UninstallString",
        &format!("\"{}\" --uninstall", uninst.display()),
    )?;
    key.set_str(
        "QuietUninstallString",
        &format!("\"{}\" --uninstall --silent", uninst.display()),
    )?;
    if !HOMEPAGE.is_empty() {
        key.set_str("URLInfoAbout", HOMEPAGE)?;
    }
    key.set_u32("NoModify", 1)?;
    key.set_u32("NoRepair", 1)?;
    key.set_u32("EstimatedSize", (size_bytes / 1024) as u32)?;
    Ok(())
}

/// "Open Proton DICOM prompt here" on a folder and on the empty space inside
/// one: a command prompt in that folder with every tool on `PATH` - the way
/// a folder of radiographs is usually converted.
fn write_folder_menu(opts: &Options, hive: registry::Hive) -> Result<()> {
    let root = hive.hkey();
    let command = format!(
        "\"{}\" {}",
        cmd_exe().display(),
        shell_args(&opts.shell_path(), Some("%V"))
    );
    let icon = format!("{},0", opts.setup_path().display());
    for base in folder_menu_keys() {
        let k = Key::create(root, &base)?;
        k.set_str("", "Open Proton DICOM prompt here")?;
        k.set_str("Icon", &icon)?;
        Key::create(root, &format!(r"{base}\command"))?.set_str("", &command)?;
    }
    Ok(())
}

/// The executables at the top of the payload - the tools, which must not be
/// running while they are replaced.
pub fn payload_executables(payload: &Payload) -> Vec<String> {
    payload
        .entries()
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.name)
        .filter(|n| !n.contains('/') && n.to_ascii_lowercase().ends_with(".exe"))
        .collect()
}

/// The first of `exes` (file names in `dir`) that is running, if any.
pub fn running_in(dir: &Path, exes: &[String]) -> Option<String> {
    exes.iter()
        .find(|name| !name.eq_ignore_ascii_case(SETUP_EXE) && is_running(&dir.join(name)))
        .cloned()
}

/// Product version: from the payload if `pdp-pack` recorded one, else ours.
pub fn payload_version(payload: &Payload) -> String {
    payload
        .read_text("payload-info.txt")
        .and_then(|t| {
            t.lines()
                .filter_map(|l| l.split_once('='))
                .find(|(k, _)| k.trim() == "version")
                .map(|(_, v)| v.trim().to_string())
        })
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

/// Files the previous installation listed that the new one does not, less
/// the few this installation writes itself after the copy.
fn obsolete_files(old: &[String], new: &[String]) -> Vec<String> {
    let norm = |f: &str| f.replace('\\', "/").to_ascii_lowercase();
    let keep: std::collections::HashSet<String> = new
        .iter()
        .map(|f| norm(f))
        .chain([MANIFEST_FILE, SETUP_EXE].map(norm))
        .collect();
    old.iter()
        .filter(|f| !keep.contains(&norm(f)))
        .cloned()
        .collect()
}

/// Delete `rels` (relative to `dir`, `/`-separated) and then the folders they
/// leave empty, never `dir` itself or anything outside it. Returns how many
/// files went.
fn remove_relative(dir: &Path, rels: &[String], log: &dyn Fn(String)) -> usize {
    let mut removed = 0;
    let mut parents: Vec<PathBuf> = Vec::new();
    for rel in rels {
        let Some(path) = safe_relative(dir, rel) else {
            continue;
        };
        if path.is_file() {
            match std::fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(e) => log(format!("Could not remove {}: {e}", path.display())),
            }
        }
        let mut p = path.parent().map(Path::to_path_buf);
        while let Some(d) = p {
            if d == dir || !d.starts_with(dir) {
                break;
            }
            if !parents.contains(&d) {
                parents.push(d.clone());
            }
            p = d.parent().map(Path::to_path_buf);
        }
    }
    parents.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for d in parents {
        let _ = std::fs::remove_dir(&d);
    }
    removed
}

/// `dir` joined with a manifest entry, refusing anything that would climb
/// out of it - the manifest is a text file anyone can edit.
fn safe_relative(dir: &Path, rel: &str) -> Option<PathBuf> {
    let mut out = dir.to_path_buf();
    for part in rel.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || part.contains(':') {
            return None;
        }
        out.push(part);
    }
    (out != dir).then_some(out)
}

/// Writability probe - cheaper and more honest than inspecting ACLs.
fn check_writable(dir: &Path) -> Result<()> {
    let probe = dir.join(".pdp-write-test");
    match std::fs::write(&probe, b"x") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => bail!(
            "cannot write to {} ({e}).\nChoose another folder, or re-run the installer as \
             administrator for a machine-wide installation.",
            dir.display()
        ),
    }
}

/// A running executable cannot be overwritten on Windows; detect that early
/// by trying to open the file for writing.
fn is_running(exe: &Path) -> bool {
    if !exe.exists() {
        return false;
    }
    std::fs::OpenOptions::new().write(true).open(exe).is_err()
}

#[cfg(test)]
mod update_tests {
    use super::*;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_update_removes_what_the_new_version_no_longer_ships() {
        let old = strings(&[
            "prad-rti-gen.exe",
            "retired-tool.exe",
            "docs/old-page.md",
            "docs/prad-rti-gen.md",
            "pdp-setup.exe",
        ]);
        let new = strings(&["prad-rti-gen.exe", "DOCS/prad-rti-gen.md", "new-tool.exe"]);
        assert_eq!(
            obsolete_files(&old, &new),
            strings(&["retired-tool.exe", "docs/old-page.md"]),
            "a retired tool goes; the setup program is rewritten, not removed"
        );
    }

    #[test]
    fn nothing_is_obsolete_when_the_file_list_is_unchanged() {
        let files = strings(&[
            "a.exe",
            "docs/b.md",
            "pdp-setup.exe",
            "install-manifest.txt",
        ]);
        assert!(obsolete_files(&files, &files).is_empty());
    }

    #[test]
    fn a_manifest_entry_cannot_reach_outside_the_folder() {
        let dir = Path::new("/inst");
        assert_eq!(
            safe_relative(dir, "docs/a.md"),
            Some(PathBuf::from("/inst/docs/a.md"))
        );
        assert_eq!(safe_relative(dir, "../other/file"), None);
        assert_eq!(safe_relative(dir, "C:/Windows/x.dll"), None);
        assert_eq!(
            safe_relative(dir, "./"),
            None,
            "and never names the folder itself"
        );
    }
}
