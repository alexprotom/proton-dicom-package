//! End-to-end: run the executable on synthetic ROOT scans and read the DICOM back.

use dicom_dictionary_std::{tags, uids};
use dicom_object::{OpenFileOptions, open_file};
use std::path::{Path, PathBuf};
use std::process::Command;

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prad-rti-gen-it-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A 4 x 3 ROOT TTree::Scan with WEPL = 0.1 * (x + 10 y) cm.
fn write_scan(path: &Path) {
    let mut s = String::from(
        "************************************************\n\
         *    Row   *       x.x *       y.y * WEPL.WEPL *\n\
         ************************************************\n",
    );
    let mut row = 0;
    for x in 0..4 {
        for y in 0..3 {
            let w = 0.1 * (x + 10 * y) as f64;
            s.push_str(&format!("* {row:>8} * {x:>9} * {y:>9} * {w:>9.7} *\n"));
            row += 1;
        }
    }
    s.push_str("************************************************\n");
    std::fs::write(path, s).unwrap();
}

fn run(args: &[&std::ffi::OsStr], cwd: &Path) -> std::process::Output {
    let out = Command::new(env!("CARGO_BIN_EXE_prad-rti-gen"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn str_of(obj: &dicom_object::DefaultDicomObject, tag: dicom_core::Tag) -> String {
    obj.element(tag)
        .unwrap()
        .to_str()
        .unwrap()
        .trim()
        .to_string()
}

#[test]
fn converts_a_scan_to_an_rt_image() {
    let dir = temp_dir("single");
    write_scan(&dir.join("RG_Output_0.txt"));
    std::fs::write(
        dir.join("prad-rti-gen.toml"),
        "[image]\npixel_spacing_mm = [0.5, 0.25]\n[geometry]\ngantry_angle_deg = 90.0\n",
    )
    .unwrap();

    run(&["RG_Output_0.txt".as_ref()], &dir);

    let obj = open_file(dir.join("RG_Output_0.dcm")).unwrap();
    assert_eq!(
        obj.meta().media_storage_sop_class_uid(),
        uids::RT_IMAGE_STORAGE
    );
    assert_eq!(str_of(&obj, tags::MODALITY), "RTIMAGE");
    assert_eq!(str_of(&obj, tags::RT_IMAGE_LABEL), "RG_Output_0");
    assert_eq!(str_of(&obj, tags::IMAGE_PLANE_PIXEL_SPACING), "0.5\\0.25");
    assert_eq!(str_of(&obj, tags::GANTRY_ANGLE), "90");
    assert_eq!(str_of(&obj, tags::RESCALE_TYPE), "WEPL_MM");

    let rows = obj.element(tags::ROWS).unwrap().to_int::<u16>().unwrap();
    let cols = obj.element(tags::COLUMNS).unwrap().to_int::<u16>().unwrap();
    assert_eq!((rows, cols), (3, 4));

    // centre of the top-left pixel, image centred on the beam axis
    let pos = obj
        .element(tags::RT_IMAGE_POSITION)
        .unwrap()
        .to_multi_float64()
        .unwrap();
    assert_eq!(pos, vec![-0.375, 0.5]);

    let slope = obj
        .element(tags::RESCALE_SLOPE)
        .unwrap()
        .to_float64()
        .unwrap();
    let bytes = obj.element(tags::PIXEL_DATA).unwrap().to_bytes().unwrap();
    let px: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    for r in 0..3usize {
        for c in 0..4usize {
            let y = 2 - r; // y up: the top row is the highest y
            let expected_mm = 10.0 * 0.1 * (c + 10 * y) as f64;
            let got = px[r * 4 + c] as f64 * slope;
            assert!(
                (got - expected_mm).abs() < 1e-9,
                "row {r} col {c}: {got} != {expected_mm}"
            );
        }
    }
}

#[test]
fn a_wildcard_run_is_one_series_in_numeric_order() {
    let dir = temp_dir("series");
    for i in [0, 1, 2, 10] {
        write_scan(&dir.join(format!("RG_Output_{i}.txt")));
    }
    let out_dir = dir.join("dicom");
    run(
        &[
            "RG_Output_*.txt".as_ref(),
            "-o".as_ref(),
            out_dir.as_os_str(),
        ],
        &dir,
    );

    let mut series = Vec::new();
    for (expected_instance, i) in [0, 1, 2, 10].iter().enumerate() {
        let obj = OpenFileOptions::new()
            .open_file(out_dir.join(format!("RG_Output_{i}.dcm")))
            .unwrap();
        let instance = obj
            .element(tags::INSTANCE_NUMBER)
            .unwrap()
            .to_int::<u32>()
            .unwrap();
        assert_eq!(instance as usize, expected_instance + 1, "RG_Output_{i}");
        series.push(str_of(&obj, tags::SERIES_INSTANCE_UID));
    }
    assert!(series.windows(2).all(|w| w[0] == w[1]));
}

#[test]
fn init_config_writes_the_template() {
    let dir = temp_dir("init");
    run(&["--init-config".as_ref()], &dir);
    let text = std::fs::read_to_string(dir.join("prad-rti-gen.toml")).unwrap();
    assert!(text.contains("wepl_unit = \"cm\""));
}
