//! Reader for ROOT `TTree::Scan()` text output, e.g.
//!
//! ```text
//! ************************************************
//! *    Row   *       x.x *       y.y * WEPL.WEPL *
//! ************************************************
//! *        0 *         0 *         0 *         0 *
//! *        1 *         0 *         1 * 0.3162373 *
//! ```
//!
//! x and y are integer bin indices; the value column is the WEPL.

use crate::config::InputCfg;
use std::path::Path;

/// WEPL map in DICOM pixel order: `data[row * cols + col]`, row 0 = top.
pub struct Image {
    pub rows: usize,
    pub cols: usize,
    /// WEPL in the input unit.
    pub data: Vec<f64>,
    pub x_range: (i64, i64),
    pub y_range: (i64, i64),
    pub entries: usize,
    pub missing: usize,
    pub duplicates: usize,
}

fn fields(line: &str) -> Option<Vec<&str>> {
    let t = line.trim();
    if !t.starts_with('*') || !t.ends_with('*') || t.len() < 2 {
        return None;
    }
    let inner = &t[1..t.len() - 1];
    if inner.chars().all(|c| c == '*') {
        return None; // separator line
    }
    Some(inner.split('*').map(str::trim).collect())
}

fn as_index(s: &str, what: &str, line_no: usize) -> Result<i64, String> {
    let v: f64 = s
        .parse()
        .map_err(|_| format!("line {line_no}: cannot parse {what} value '{s}'"))?;
    let r = v.round();
    if !v.is_finite() || (v - r).abs() > 1e-6 {
        return Err(format!(
            "line {line_no}: {what} = '{s}' is not an integer bin index"
        ));
    }
    Ok(r as i64)
}

pub fn read_root_scan(path: &Path, cfg: &InputCfg) -> Result<Image, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read file: {e}"))?;

    let mut cols_idx: Option<(usize, usize, usize, usize)> = None; // (x, y, v, n_fields)
    let mut points: Vec<(i64, i64, f64)> = Vec::new();

    for (i, line) in text.lines().enumerate() {
        let line_no = i + 1;
        let Some(f) = fields(line) else { continue };

        match cols_idx {
            None => {
                let find = |name: &str| f.iter().position(|c| *c == name);
                if let (Some(x), Some(y), Some(v)) = (
                    find(&cfg.x_column),
                    find(&cfg.y_column),
                    find(&cfg.value_column),
                ) {
                    cols_idx = Some((x, y, v, f.len()));
                }
            }
            Some((xi, yi, vi, n)) => {
                if f.len() != n {
                    continue; // e.g. a repeated header after "Type <CR> to continue"
                }
                if f[xi] == cfg.x_column {
                    continue; // repeated header
                }
                let x = as_index(f[xi], &cfg.x_column, line_no)?;
                let y = as_index(f[yi], &cfg.y_column, line_no)?;
                let v: f64 = f[vi].parse().map_err(|_| {
                    format!(
                        "line {line_no}: cannot parse {} value '{}'",
                        cfg.value_column, f[vi]
                    )
                })?;
                points.push((x, y, v));
            }
        }
    }

    if cols_idx.is_none() {
        return Err(format!(
            "no header line with columns '{}', '{}', '{}' found (check [input] column names)",
            cfg.x_column, cfg.y_column, cfg.value_column
        ));
    }
    if points.is_empty() {
        return Err("no data rows found".into());
    }

    let (mut x0, mut x1, mut y0, mut y1) = (i64::MAX, i64::MIN, i64::MAX, i64::MIN);
    for &(x, y, _) in &points {
        x0 = x0.min(x);
        x1 = x1.max(x);
        y0 = y0.min(y);
        y1 = y1.max(y);
    }
    let cols = (x1 - x0 + 1) as usize;
    let rows = (y1 - y0 + 1) as usize;
    if cols > 65535 || rows > 65535 {
        return Err(format!("grid {cols} x {rows} is too large for DICOM"));
    }

    let mut data = vec![f64::NAN; rows * cols];
    let mut filled = vec![false; rows * cols];
    let mut duplicates = 0;
    for &(x, y, v) in &points {
        let ix = (x - x0) as usize;
        let iy = (y - y0) as usize;
        let col = if cfg.x_axis_right { ix } else { cols - 1 - ix };
        let row = if cfg.y_axis_up { rows - 1 - iy } else { iy };
        let k = row * cols + col;
        if filled[k] {
            duplicates += 1;
        }
        filled[k] = true;
        data[k] = v;
    }

    let mut missing = 0;
    for v in data.iter_mut() {
        if !v.is_finite() {
            *v = cfg.missing_value;
            missing += 1;
        }
    }

    Ok(Image {
        rows,
        cols,
        data,
        x_range: (x0, x1),
        y_range: (y0, y1),
        entries: points.len(),
        missing,
        duplicates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn write(name: &str, text: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("prad-rti-gen-reader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    const SCAN: &str = "\
************************************************
*    Row   *       x.x *       y.y * WEPL.WEPL *
************************************************
*        0 *         0 *         0 *         1 *
*        1 *         0 *         1 *         2 *
*        2 *         1 *         0 *         3 *
Type <CR> to continue or q to quit ==>
************************************************
*    Row   *       x.x *       y.y * WEPL.WEPL *
************************************************
*        3 *         1 *         1 *       nan *
*        4 *         2 *         0 *       4.5 *
************************************************
==> 5 selected entries
";

    #[test]
    fn reads_a_paged_scan_with_y_up() {
        let cfg = Config::default_config().input;
        let img = read_root_scan(&write("a.txt", SCAN), &cfg).unwrap();
        assert_eq!((img.cols, img.rows), (3, 2));
        assert_eq!(img.entries, 5);
        // y up: y = 1 is the top row
        assert_eq!(img.data, vec![2.0, 0.0, 0.0, 1.0, 3.0, 4.5]);
        // (2, 1) is absent and (1, 1) is NaN
        assert_eq!(img.missing, 2);
    }

    #[test]
    fn y_down_and_mirrored_x() {
        let mut cfg = Config::default_config().input;
        cfg.y_axis_up = false;
        cfg.x_axis_right = false;
        let img = read_root_scan(&write("b.txt", SCAN), &cfg).unwrap();
        assert_eq!(img.data, vec![4.5, 3.0, 1.0, 0.0, 0.0, 2.0]);
    }

    #[test]
    fn wrong_column_names_are_reported() {
        let mut cfg = Config::default_config().input;
        cfg.value_column = "dose".into();
        let err = read_root_scan(&write("c.txt", SCAN), &cfg).err().unwrap();
        assert!(err.contains("dose"), "{err}");
    }

    #[test]
    fn non_integer_indices_are_rejected() {
        let cfg = Config::default_config().input;
        let text = SCAN.replace(
            "*         2 *         0 *       4.5 *",
            "*       2.5 *         0 *       4.5 *",
        );
        let err = read_root_scan(&write("d.txt", &text), &cfg).err().unwrap();
        assert!(err.contains("not an integer"), "{err}");
    }
}
