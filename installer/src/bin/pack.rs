//! `pdp-pack` - turns the freshly built `pdp-setup.exe` into a shippable
//! single-file installer by appending a zip of everything that should be
//! installed, plus the footer `payload.rs` looks for.
//!
//!     cargo build --release                 # the tools, from the repo root
//!     cd installer
//!     cargo build --release                 # pdp-setup.exe + pdp-pack.exe
//!     cargo run --release --bin pdp-pack    # dist/proton-dicom-package-setup.exe
//!
//! The tools are found, not listed: every crate under `crates/` that builds
//! an executable (a `src/main.rs`, or `[[bin]]` tables) is packed, so a new
//! tool needs no change here.
//!
//! With `--winget <DIR>` it also writes the three winget manifests for the
//! installer it just built (version, installer, default locale), hashed and
//! pointing at the GitHub release asset - what a first submission to
//! microsoft/winget-pkgs, or `winget install --manifest`, needs.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;

#[path = "../product.rs"]
#[allow(dead_code)]
mod product;
use product::*;

const USAGE: &str = "\
pdp-pack - build the shippable installer

USAGE:
    pdp-pack [OPTIONS]

OPTIONS:
    --repo <DIR>       repository root (default: the parent of this crate)
    --bin-dir <DIR>    folder with the built tools (default: <repo>/target/release)
    --setup <FILE>     setup binary to wrap (default: target/release/pdp-setup.exe)
    --out <FILE>       installer to write (default: dist/proton-dicom-package-setup.exe)
    --no-docs          leave the docs/ folder out
    --list             print the tools that would be packed and exit
    --winget <DIR>     also write the winget manifests for this installer into DIR
    --url <URL>        download URL recorded in them (default: the installer
                       asset of this version's GitHub release)
    --release-date <YYYY-MM-DD>
                       release date recorded in them (default: none)
    -h, --help         this text
";

struct Opts {
    repo: PathBuf,
    bin_dir: Option<PathBuf>,
    setup: Option<PathBuf>,
    out: Option<PathBuf>,
    docs: bool,
    list: bool,
    winget: Option<PathBuf>,
    url: Option<String>,
    release_date: Option<String>,
}

fn main() -> Result<()> {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut o = Opts {
        repo: crate_dir.parent().unwrap_or(&crate_dir).to_path_buf(),
        bin_dir: None,
        setup: None,
        out: None,
        docs: true,
        list: false,
        winget: None,
        url: None,
        release_date: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut val = |flag: &str| -> Result<String> {
            it.next()
                .ok_or_else(|| anyhow::anyhow!("{flag} needs a value"))
        };
        match a.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "--repo" => o.repo = PathBuf::from(val("--repo")?),
            "--bin-dir" => o.bin_dir = Some(PathBuf::from(val("--bin-dir")?)),
            "--setup" => o.setup = Some(PathBuf::from(val("--setup")?)),
            "--out" => o.out = Some(PathBuf::from(val("--out")?)),
            "--no-docs" => o.docs = false,
            "--list" => o.list = true,
            "--winget" => o.winget = Some(PathBuf::from(val("--winget")?)),
            "--url" => o.url = Some(val("--url")?),
            "--release-date" => o.release_date = Some(val("--release-date")?),
            other => bail!("unknown option '{other}'\n\n{USAGE}"),
        }
    }

    let tools = discover_tools(&o.repo)?;
    if tools.is_empty() {
        bail!(
            "no tools found: no crate under {} builds an executable",
            o.repo.join("crates").display()
        );
    }
    if o.list {
        for t in &tools {
            println!("{}\t{}", t.name, t.description);
        }
        return Ok(());
    }

    let bin_dir = o
        .bin_dir
        .clone()
        .unwrap_or_else(|| o.repo.join("target/release"));
    let setup = o
        .setup
        .unwrap_or_else(|| crate_dir.join("target/release/pdp-setup.exe"));
    let out = o
        .out
        .unwrap_or_else(|| crate_dir.join("dist/proton-dicom-package-setup.exe"));

    if !setup.is_file() {
        bail!(
            "{} not found - build the installer first:\n    cargo build --release   (in {})",
            setup.display(),
            crate_dir.display()
        );
    }

    // ---- collect the payload ---------------------------------------------
    let cargo_toml = o.repo.join("Cargo.toml");
    let version = read_version(&cargo_toml).unwrap_or_else(|| "0.0.0".to_string());
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for t in &tools {
        let exe = bin_dir.join(format!("{}.exe", t.name));
        if !exe.is_file() {
            bail!(
                "{} not found - build the tools first:\n    cargo build --release   (in {})",
                exe.display(),
                o.repo.display()
            );
        }
        files.push((format!("{}.exe", t.name), exe));
    }
    for name in ["LICENSE.txt", "README.md"] {
        let p = o.repo.join(name);
        if p.is_file() {
            files.push((name.to_string(), p));
        }
    }
    if o.docs {
        collect_dir(&o.repo, "docs", &mut files)?;
    }

    let info = format!("# generated by pdp-pack\nversion = {version}\n");
    let generated = [
        ("payload-info.txt", info),
        (TOOLS_FILE, render_tools(&tools)),
        (SHELL_CMD, shell_script(&version, &tools)),
    ];

    // ---- write the zip ----------------------------------------------------
    println!("Packing {} files", files.len() + generated.len());
    let mut zip_bytes = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_bytes));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true);
        for (name, text) in &generated {
            zip.start_file(*name, opts)?;
            zip.write_all(text.as_bytes())?;
            println!("  {name}");
        }
        for (name, path) in &files {
            zip.start_file(name.as_str(), opts)?;
            let mut f =
                std::fs::File::open(path).with_context(|| format!("read {}", path.display()))?;
            std::io::copy(&mut f, &mut zip)?;
            println!("  {name}");
        }
        zip.finish()?;
    }

    // ---- base setup binary, minus any payload from an earlier run ---------
    let base_len = match read_existing_footer(&setup)? {
        Some(offset) => offset,
        None => setup.metadata()?.len(),
    };

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut w = std::io::BufWriter::new(
        std::fs::File::create(&out).with_context(|| format!("write {}", out.display()))?,
    );
    let mut src = std::fs::File::open(&setup)?;
    let mut left = base_len;
    let mut buf = vec![0u8; 1 << 20];
    while left > 0 {
        let want = buf.len().min(left as usize);
        let n = src.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        w.write_all(&buf[..n])?;
        left -= n as u64;
    }
    w.write_all(&zip_bytes)?;
    w.write_all(MAGIC)?;
    w.write_all(&base_len.to_le_bytes())?;
    w.write_all(&(zip_bytes.len() as u64).to_le_bytes())?;
    w.flush()?;
    drop(w);

    let total = out.metadata()?.len();
    println!(
        "\n{}\n  version {version}, {} of payload, {} in total",
        out.display(),
        human(zip_bytes.len() as u64),
        human(total)
    );

    if let Some(dir) = &o.winget {
        let sha = sha256_file(&out)?;
        let url = o
            .url
            .clone()
            .unwrap_or_else(|| release_download_url(&version, &release_asset_name(&version)));
        let m = Winget {
            version: &version,
            url: &url,
            sha256: &sha,
            release_date: o.release_date.as_deref(),
            tools: &tools,
        };
        std::fs::create_dir_all(dir)?;
        for (name, text) in m.files() {
            let path = dir.join(&name);
            std::fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
            println!("  {}", path.display());
        }
        println!("  winget: {WINGET_ID} {version}, installer SHA-256 {sha}");
    }
    Ok(())
}

/// The payload footer magic; must match `payload::MAGIC`.
const MAGIC: &[u8; 8] = b"PDPPAY01";

/// Every executable built by a crate under `<repo>/crates/`, sorted by name.
///
/// A crate's executables are its `[[bin]]` tables, or - without any - the
/// package name when it has a `src/main.rs`. Library crates have neither
/// and are skipped.
fn discover_tools(repo: &Path) -> Result<Vec<Tool>> {
    let crates = repo.join("crates");
    let mut tools = Vec::new();
    let entries =
        std::fs::read_dir(&crates).with_context(|| format!("read {}", crates.display()))?;
    for e in entries {
        let dir = e?.path();
        let manifest = dir.join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&manifest)
            .with_context(|| format!("read {}", manifest.display()))?;
        let m = parse_manifest(&text);
        let names = if !m.bins.is_empty() {
            m.bins
        } else if dir.join("src/main.rs").is_file() {
            m.package_name.into_iter().collect()
        } else {
            Vec::new()
        };
        for name in names {
            tools.push(Tool {
                name,
                description: m.description.clone(),
            });
        }
    }
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(tools)
}

#[derive(Default, Debug)]
struct CrateManifest {
    package_name: Option<String>,
    description: String,
    bins: Vec<String>,
}

/// The few keys `discover_tools` needs from a crate's `Cargo.toml`: the
/// package name and description, and the names in `[[bin]]` tables.
fn parse_manifest(text: &str) -> CrateManifest {
    let mut m = CrateManifest::default();
    let mut table = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            table = line.to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        // `description.workspace = true` and the like are not strings.
        let Some(value) = value
            .strip_prefix('"')
            .and_then(|v| v.split_once('"'))
            .map(|(v, _)| v.to_string())
        else {
            continue;
        };
        match (table.as_str(), key) {
            ("[package]", "name") => m.package_name = Some(value),
            ("[package]", "description") => m.description = value,
            ("[[bin]]", "name") => m.bins.push(value),
            _ => {}
        }
    }
    m
}

/// `pdp-shell.cmd`: a command prompt with the program folder first on
/// `PATH`, in the folder given as its argument (the context-menu entry) or
/// in the user's home folder (the Start-menu shortcut). It works whether or
/// not the folder was also added to `PATH` permanently.
fn shell_script(version: &str, tools: &[Tool]) -> String {
    let width = tools.iter().map(|t| t.name.len()).max().unwrap_or(0);
    let mut lines = vec![
        "@echo off".to_string(),
        "rem The Proton DICOM Package prompt - generated by pdp-pack.".to_string(),
        "set \"PATH=%~dp0;%PATH%\"".to_string(),
        format!("title {} {}", cmd_escape(APP_NAME), cmd_escape(version)),
        "if not \"%~1\"==\"\" (cd /d \"%~1\") else (cd /d \"%USERPROFILE%\")".to_string(),
        "echo.".to_string(),
        format!("echo  {} {}", cmd_escape(APP_NAME), cmd_escape(version)),
        "echo.".to_string(),
    ];
    for t in tools {
        lines.push(format!(
            "echo    {:<width$}   {}",
            cmd_escape(&t.name),
            cmd_escape(&t.description)
        ));
    }
    lines.push("echo.".to_string());
    lines.push("echo  Every tool lists its options with --help.".to_string());
    lines.push("echo.".to_string());
    let mut s = lines.join("\r\n");
    s.push_str("\r\n");
    s
}

/// Make text safe after `echo` / `title` in a batch file.
fn cmd_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' => out.push_str("%%"),
            '^' | '&' | '|' | '<' | '>' => {
                out.push('^');
                out.push(c);
            }
            '\r' | '\n' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path).with_context(|| format!("read {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02X}")).collect())
}

/// The winget manifests (schema 1.12.0) for one installer.
///
/// Two installer entries share the one file: `user` scope passes
/// `--just-me`, `machine` scope passes `--all-users` and is run elevated by
/// winget. The Apps & features key the setup writes is the product code, so
/// winget recognises an installation made by the wizard too, and `winget
/// upgrade` runs the new setup over it - which updates it in place.
struct Winget<'a> {
    version: &'a str,
    url: &'a str,
    sha256: &'a str,
    release_date: Option<&'a str>,
    tools: &'a [Tool],
}

/// The package identifier in the winget community repository. The
/// `winget` job of the release workflow names it too.
const WINGET_ID: &str = "PDP.ProtonDICOMPackage";
const WINGET_SCHEMA: &str = "1.12.0";

impl Winget<'_> {
    fn files(&self) -> Vec<(String, String)> {
        vec![
            (format!("{WINGET_ID}.yaml"), self.version_manifest()),
            (
                format!("{WINGET_ID}.installer.yaml"),
                self.installer_manifest(),
            ),
            (
                format!("{WINGET_ID}.locale.en-US.yaml"),
                self.locale_manifest(),
            ),
        ]
    }

    fn header(kind: &str) -> String {
        format!(
            "# Created by pdp-pack\n\
             # yaml-language-server: $schema=https://aka.ms/winget-manifest.{kind}.{WINGET_SCHEMA}.schema.json\n\n"
        )
    }

    fn version_manifest(&self) -> String {
        let mut s = Self::header("version");
        s += &format!("PackageIdentifier: {WINGET_ID}\n");
        s += &format!("PackageVersion: {}\n", q(self.version));
        s += "DefaultLocale: en-US\n";
        s += "ManifestType: version\n";
        s += &format!("ManifestVersion: {WINGET_SCHEMA}\n");
        s
    }

    fn installer_manifest(&self) -> String {
        let mut s = Self::header("installer");
        s += &format!("PackageIdentifier: {WINGET_ID}\n");
        s += &format!("PackageVersion: {}\n", q(self.version));
        s += "InstallerType: exe\n";
        s += "InstallModes:\n- interactive\n- silent\n- silentWithProgress\n";
        s += "UpgradeBehavior: install\n";
        // The codes `pdp-setup` exits with (plan.rs, EXIT_*).
        s += "ExpectedReturnCodes:\n";
        for (code, response) in [
            (2, "packageInUse"),
            (3, "cancelledByUser"),
            (4, "downgrade"),
            (5, "noNetwork"),
        ] {
            s += &format!("- InstallerReturnCode: {code}\n  ReturnResponse: {response}\n");
        }
        // The command-line tools the installer puts on PATH.
        s += "Commands:\n";
        for t in self.tools {
            s += &format!("- {}\n", t.name);
        }
        s += &format!("ProductCode: {PRODUCT_ID}\n");
        if let Some(date) = self.release_date {
            s += &format!("ReleaseDate: {date}\n");
        }
        s += "AppsAndFeaturesEntries:\n";
        s += &format!("- DisplayName: {}\n", q(APP_NAME));
        s += &format!("  Publisher: {}\n", q(PUBLISHER));
        s += &format!("  ProductCode: {PRODUCT_ID}\n");
        s += "Installers:\n";
        for (scope, flag) in [("user", "--just-me"), ("machine", "--all-users")] {
            s += "- Architecture: x64\n";
            s += &format!("  Scope: {scope}\n");
            s += &format!("  InstallerUrl: {}\n", self.url);
            s += &format!("  InstallerSha256: {}\n", self.sha256);
            if scope == "machine" {
                s += "  ElevationRequirement: elevationRequired\n";
            }
            // Every switch in every entry: an entry's own InstallerSwitches
            // are not merged with ones written at the top level by every
            // tool that edits these files.
            s += "  InstallerSwitches:\n";
            s += "    Silent: --silent\n";
            s += "    SilentWithProgress: --passive\n";
            s += "    InstallLocation: --dir \"<INSTALLPATH>\"\n";
            s += &format!("    Custom: {flag} --no-launch\n");
        }
        s += "ManifestType: installer\n";
        s += &format!("ManifestVersion: {WINGET_SCHEMA}\n");
        s
    }

    fn locale_manifest(&self) -> String {
        let repo = format!("https://github.com/{RELEASE_REPO}");
        let owner = RELEASE_REPO.split('/').next().unwrap_or(RELEASE_REPO);
        let mut s = Self::header("defaultLocale");
        s += &format!("PackageIdentifier: {WINGET_ID}\n");
        s += &format!("PackageVersion: {}\n", q(self.version));
        s += "PackageLocale: en-US\n";
        s += &format!("Publisher: {}\n", q(PUBLISHER));
        s += &format!("PublisherUrl: https://github.com/{owner}\n");
        s += &format!("PublisherSupportUrl: {repo}/issues\n");
        s += &format!("PackageName: {}\n", q(APP_NAME));
        s += &format!("PackageUrl: {repo}\n");
        s += "License: MIT\n";
        s += &format!("LicenseUrl: {repo}/blob/main/LICENSE.txt\n");
        s += &format!(
            "ShortDescription: {}\n",
            q("Command-line tools for proton radiography and CT data in DICOM")
        );
        let mut description = String::from(
            "Open-source command-line utilities for proton radiography and CT in DICOM, \
             for research and QA. Not a medical device. Tools:",
        );
        for t in self.tools {
            description.push_str(&format!(" {} - {};", t.name, t.description));
        }
        s += &format!("Description: {}\n", q(description.trim_end_matches(';')));
        s += "Moniker: proton-dicom-package\n";
        s += "Tags:\n";
        for tag in [
            "dicom",
            "medical-physics",
            "proton-therapy",
            "proton-radiography",
            "rt-image",
            "command-line",
        ] {
            s += &format!("- {tag}\n");
        }
        s += &format!("ReleaseNotesUrl: {repo}/releases/tag/v{}\n", self.version);
        s += "ManifestType: defaultLocale\n";
        s += &format!("ManifestVersion: {WINGET_SCHEMA}\n");
        s
    }
}

/// A YAML double-quoted scalar.
fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Add every file under `<repo>/<rel>` to the payload list.
fn collect_dir(repo: &Path, rel: &str, files: &mut Vec<(String, PathBuf)>) -> Result<()> {
    let root = repo.join(rel);
    if !root.is_dir() {
        return Ok(());
    }
    for e in walkdir::WalkDir::new(&root).sort_by_file_name() {
        let e = e?;
        if !e.file_type().is_file() {
            continue;
        }
        let name = e
            .path()
            .strip_prefix(repo)?
            .to_string_lossy()
            .replace('\\', "/");
        files.push((name, e.path().to_path_buf()));
    }
    Ok(())
}

/// `version = "x.y.z"` of the root `Cargo.toml` - the `[workspace.package]`
/// version every tool inherits.
fn read_version(cargo_toml: &Path) -> Option<String> {
    read_key(cargo_toml, "version")
}

/// The first `key = "value"` line of a `Cargo.toml`, before its dependency
/// tables.
fn read_key(cargo_toml: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(cargo_toml).ok()?;
    text.lines()
        .take_while(|l| !l.trim_start().starts_with("[workspace.dependencies"))
        .take_while(|l| !l.trim_start().starts_with("[dependencies"))
        .find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
        })
}

/// If `exe` was already packed, return the length of its payload-free prefix.
fn read_existing_footer(exe: &Path) -> Result<Option<u64>> {
    use std::io::{Seek, SeekFrom};
    let mut f = std::fs::File::open(exe)?;
    let len = f.metadata()?.len();
    if len < 24 {
        return Ok(None);
    }
    let mut buf = [0u8; 24];
    f.seek(SeekFrom::End(-24))?;
    f.read_exact(&mut buf)?;
    if &buf[..8] != MAGIC {
        return Ok(None);
    }
    Ok(Some(u64::from_le_bytes(buf[8..16].try_into().unwrap())))
}

fn human(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{:.0} kB", b / 1e3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
    }

    /// The updater follows `RELEASE_REPO`; the root `Cargo.toml` names the
    /// repository too. If they ever disagreed, the installed program would
    /// look for updates somewhere else than its source.
    #[test]
    fn the_release_repository_is_the_one_cargo_names() {
        assert_eq!(
            read_key(&repo().join("Cargo.toml"), "repository"),
            Some(format!("https://github.com/{RELEASE_REPO}"))
        );
    }

    #[test]
    fn the_version_is_the_workspace_version() {
        let v = read_version(&repo().join("Cargo.toml")).unwrap();
        assert!(v.chars().next().unwrap().is_ascii_digit(), "{v}");
    }

    #[test]
    fn the_tools_of_this_repository_are_found() {
        let tools = discover_tools(&repo()).unwrap();
        assert!(
            tools
                .iter()
                .any(|t| t.name == "prad-rti-gen" && !t.description.is_empty()),
            "{tools:?}"
        );
    }

    #[test]
    fn bin_tables_win_over_the_package_name() {
        let m = parse_manifest(
            "[package]\nname = \"ct-tools\"\ndescription = \"CT things\"\n\
             version.workspace = true\n\n[[bin]]\nname = \"ct-a\"\npath = \"src/a.rs\"\n\n\
             [[bin]]\nname = \"ct-b\"\n\n[dependencies]\nname = \"not-a-bin\"\n",
        );
        assert_eq!(m.package_name.as_deref(), Some("ct-tools"));
        assert_eq!(m.bins, ["ct-a", "ct-b"]);
        let m = parse_manifest("[package]\nname = \"x\"\ndescription.workspace = true\n");
        assert_eq!(
            m.description, "",
            "an inherited description is not a string"
        );
    }

    #[test]
    fn the_prompt_script_is_safe_batch() {
        let tools = vec![Tool {
            name: "prad-rti-gen".into(),
            description: "WEPL (cm) -> RT Image & 100% <fun>".into(),
        }];
        let s = shell_script("0.1.0", &tools);
        assert!(s.contains("\r\n"), "batch files want CRLF");
        assert!(!s.contains("\n\n"));
        assert!(s.contains("set \"PATH=%~dp0;%PATH%\""));
        assert!(s.contains("echo    prad-rti-gen   WEPL (cm) -^> RT Image ^& 100%% ^<fun^>"));
    }

    fn manifests() -> Vec<(String, String)> {
        let tools = vec![Tool {
            name: "prad-rti-gen".into(),
            description: "A \"quoted\" tool".into(),
        }];
        Winget {
            version: "0.1.0",
            url: "https://example.org/setup.exe",
            sha256: "AB",
            release_date: Some("2026-10-26"),
            tools: &tools,
        }
        .files()
    }

    #[test]
    fn the_three_manifests_agree_on_identity_and_schema() {
        let files = manifests();
        assert_eq!(files.len(), 3);
        for (name, text) in &files {
            assert!(name.starts_with(WINGET_ID), "{name}");
            assert!(
                text.contains(&format!("PackageIdentifier: {WINGET_ID}\n")),
                "{name}"
            );
            assert!(text.contains("PackageVersion: \"0.1.0\"\n"), "{name}");
            assert!(
                text.ends_with(&format!("ManifestVersion: {WINGET_SCHEMA}\n")),
                "{name}"
            );
            assert!(!text.contains('\t'), "YAML has no tabs: {name}");
        }
    }

    #[test]
    fn both_scopes_install_the_same_file_with_their_own_switch() {
        let installer = &manifests()[1].1;
        assert_eq!(
            installer
                .matches("InstallerUrl: https://example.org/setup.exe")
                .count(),
            2
        );
        assert_eq!(installer.matches("InstallerSha256: AB").count(), 2);
        assert!(installer.contains("  Scope: user\n"));
        assert!(installer.contains("  Scope: machine\n  InstallerUrl"));
        assert!(installer.contains("    Custom: --just-me --no-launch\n"));
        assert!(installer.contains("    Custom: --all-users --no-launch\n"));
        assert_eq!(
            installer
                .matches("ElevationRequirement: elevationRequired")
                .count(),
            1
        );
        assert!(installer.contains("Commands:\n- prad-rti-gen\n"));
        assert!(installer.contains(&format!("ProductCode: {PRODUCT_ID}\n")));
        assert!(installer.contains("ReleaseDate: 2026-10-26\n"));
        assert!(
            !installer.contains("Dependencies"),
            "static CRT: nothing to depend on"
        );
    }

    #[test]
    fn text_is_quoted_so_yaml_reads_it_back_verbatim() {
        let locale = &manifests()[2].1;
        assert!(locale.contains("prad-rti-gen - A \\\"quoted\\\" tool\"\n"));
        assert_eq!(q(r"C:\x"), r#""C:\\x""#);
    }
}
