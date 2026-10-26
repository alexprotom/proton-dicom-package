//! prad-rti-gen — convert proton radiography WEPL maps (ROOT TTree::Scan text
//! output) into DICOM RT Image files.
//!
//! Usage:
//!   prad-rti-gen RG_Output_0.txt                 -> RG_Output_0.dcm next to the input
//!   prad-rti-gen data\*.txt -o dicom_out         -> one RT Image per file, same series
//!   prad-rti-gen data_folder -o dicom_out        -> all *.txt files in the folder
//!   prad-rti-gen --init-config                   -> write prad-rti-gen.toml template

mod config;
mod reader;
mod rtimage;

use clap::Parser;
use config::Config;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    version,
    about = "Convert proton radiograph WEPL text files (ROOT TTree::Scan output) to DICOM RT Image"
)]
struct Cli {
    /// Input .txt files and/or folders (folders: all *.txt files inside)
    #[arg(required_unless_present = "init_config")]
    inputs: Vec<PathBuf>,

    /// Output folder (default: next to each input file)
    #[arg(short, long)]
    out_dir: Option<PathBuf>,

    /// Configuration file (default: ./prad-rti-gen.toml, then next to the executable)
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Write a commented default configuration file and exit
    #[arg(long, value_name = "FILE", num_args = 0..=1, default_missing_value = config::CONFIG_FILE_NAME)]
    init_config: Option<PathBuf>,

    /// Overwrite existing output files
    #[arg(short, long)]
    force: bool,
}

/// Sort key that orders embedded numbers numerically:
/// RG_Output_2 < RG_Output_10 (plain string order would put 10 first).
fn natural_key(p: &Path) -> Vec<(String, u64)> {
    let name = p.to_string_lossy().to_lowercase();
    let mut key = Vec::new();
    let mut text = String::new();
    let mut digits = String::new();
    for c in name.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            if !digits.is_empty() {
                key.push((
                    std::mem::take(&mut text),
                    digits.parse().unwrap_or(u64::MAX),
                ));
                digits.clear();
            }
            text.push(c);
        }
    }
    let n = if digits.is_empty() {
        0
    } else {
        digits.parse().unwrap_or(u64::MAX)
    };
    key.push((text, n));
    key
}

/// `*` / `?` matching of one file name (case-insensitive), so that
/// `RG_Output_*.txt` also works in cmd.exe, which does not expand wildcards.
fn wildcard_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    let (mut pi, mut ni, mut star, mut mark) = (0, 0, None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

fn list_dir(dir: &Path, keep: impl Fn(&Path) -> bool) -> Result<Vec<PathBuf>, String> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|f| f.is_file() && keep(f))
        .collect();
    v.sort_by_cached_key(|p| natural_key(p));
    Ok(v)
}

fn collect_inputs(inputs: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for p in inputs {
        if p.is_dir() {
            let in_dir = list_dir(p, |f| {
                f.extension().is_some_and(|x| x.eq_ignore_ascii_case("txt"))
            })?;
            if in_dir.is_empty() {
                eprintln!("warning: no .txt files in {}", p.display());
            }
            files.extend(in_dir);
        } else if p.is_file() {
            files.push(p.clone());
        } else if let Some(pattern) = p
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .filter(|f| f.contains(['*', '?']))
        {
            let dir = match p.parent() {
                Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
                _ => PathBuf::from("."),
            };
            let matched = list_dir(&dir, |f| {
                f.file_name()
                    .is_some_and(|n| wildcard_match(&pattern, &n.to_string_lossy()))
            })?;
            if matched.is_empty() {
                return Err(format!("no files match {}", p.display()));
            }
            files.extend(matched);
        } else {
            return Err(format!("input not found: {}", p.display()));
        }
    }
    Ok(files)
}

fn output_path(input: &Path, out_dir: Option<&Path>) -> PathBuf {
    let name = input.with_extension("dcm");
    match out_dir {
        Some(d) => d.join(name.file_name().unwrap()),
        None => name,
    }
}

fn run(cli: Cli) -> Result<usize, String> {
    if let Some(path) = &cli.init_config {
        if path.exists() && !cli.force {
            return Err(format!(
                "{} already exists (use --force to overwrite)",
                path.display()
            ));
        }
        std::fs::write(path, config::DEFAULT_CONFIG)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        println!("Wrote default configuration to {}", path.display());
        return Ok(0);
    }

    let cfg = match cli.config.clone().or_else(Config::locate) {
        Some(p) => {
            println!("Config : {}", p.display());
            Config::load(&p)?
        }
        None => {
            println!("Config : built-in defaults (run with --init-config to create a file)");
            Config::default_config()
        }
    };
    println!(
        "         WEPL unit in file: {}, stored as {} | pixel spacing {} x {} mm | SAD {} mm, SID {} mm",
        cfg.input.wepl_unit.name(),
        cfg.image.output_unit.name(),
        cfg.image.pixel_spacing_mm[0],
        cfg.image.pixel_spacing_mm[1],
        cfg.geometry.radiation_machine_sad_mm,
        cfg.geometry.rt_image_sid_mm
    );

    let files = collect_inputs(&cli.inputs)?;
    if files.is_empty() {
        return Err("no input files".into());
    }
    if let Some(d) = &cli.out_dir {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }

    let ctx = rtimage::SeriesContext::new(&cfg);
    let mut ok = 0;
    let mut failed = 0;
    for (i, input) in files.iter().enumerate() {
        let out = output_path(input, cli.out_dir.as_deref());
        print!(
            "[{}/{}] {} -> {} ... ",
            i + 1,
            files.len(),
            input.display(),
            out.display()
        );
        let _ = std::io::stdout().flush();
        if out.exists() && !cli.force {
            println!("skipped (exists, use --force)");
            continue;
        }
        let result = reader::read_root_scan(input, &cfg.input).and_then(|img| {
            rtimage::write_rt_image(&img, &cfg, &ctx, (i + 1) as u32, input, &out).map(|s| (img, s))
        });
        match result {
            Ok((img, s)) => {
                ok += 1;
                let u = cfg.image.output_unit.name();
                println!(
                    "OK\n        {} x {} px (x {}..{}, y {}..{}), {} entries, WEPL {:.3}..{:.3} {u}, slope {} {u}",
                    img.cols,
                    img.rows,
                    img.x_range.0,
                    img.x_range.1,
                    img.y_range.0,
                    img.y_range.1,
                    img.entries,
                    s.min,
                    s.max,
                    s.slope
                );
                if img.missing > 0 {
                    println!(
                        "        note: {} pixels missing/NaN set to {}",
                        img.missing, cfg.input.missing_value
                    );
                }
                if img.duplicates > 0 {
                    println!(
                        "        warning: {} duplicate (x, y) entries, last value kept",
                        img.duplicates
                    );
                }
                if s.slope_increased {
                    println!(
                        "        warning: rescale_slope raised to {} {u} to fit 16 bits",
                        s.slope
                    );
                }
                if s.clipped_negative > 0 {
                    println!(
                        "        warning: {} negative values clipped to 0",
                        s.clipped_negative
                    );
                }
            }
            Err(e) => {
                failed += 1;
                println!("FAILED\n        {e}");
            }
        }
    }
    println!(
        "\nDone: {ok} written, {failed} failed. Study UID {}\n                                Series UID {}",
        ctx.study_uid, ctx.series_uid
    );
    if failed > 0 {
        return Err(format!("{failed} file(s) failed"));
    }
    Ok(ok)
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_files_sort_numerically() {
        let mut v: Vec<PathBuf> = [
            "RG_Output_10.txt",
            "RG_Output_2.txt",
            "RG_Output_1.txt",
            "RG_Output_0.txt",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        v.sort_by_cached_key(|p| natural_key(p));
        let names: Vec<_> = v.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert_eq!(
            names,
            [
                "RG_Output_0.txt",
                "RG_Output_1.txt",
                "RG_Output_2.txt",
                "RG_Output_10.txt"
            ]
        );
    }

    #[test]
    fn wildcards_match_file_names() {
        assert!(wildcard_match("RG_Output_*.txt", "RG_Output_12.txt"));
        assert!(wildcard_match("rg_output_?.TXT", "RG_Output_3.txt"));
        assert!(!wildcard_match("RG_Output_?.txt", "RG_Output_12.txt"));
        assert!(!wildcard_match("*.txt", "RG_Output_0.dcm"));
        assert!(wildcard_match("*", "anything"));
    }

    #[test]
    fn the_cli_definition_is_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
