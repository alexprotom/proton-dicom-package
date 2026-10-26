//! Builds and writes a DICOM RT Image (SOP class 1.2.840.10008.5.1.4.1.1.481.1).
//!
//! The WEPL map is stored as unsigned 16-bit pixels with
//! `WEPL = pixel * RescaleSlope + RescaleIntercept` (intercept = 0).

use crate::config::Config;
use crate::reader::Image;
use dicom_core::{DataElement, PrimitiveValue, Tag, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::{FileMetaTableBuilder, InMemDicomObject};
use std::path::Path;

/// UIDs and timestamps shared by all images written in one run.
pub struct SeriesContext {
    pub study_uid: String,
    pub series_uid: String,
    pub frame_of_reference_uid: String,
    pub date: String,
    pub time: String,
}

impl SeriesContext {
    pub fn new(cfg: &Config) -> Self {
        let now = chrono::Local::now();
        let pick = |s: &str| {
            if s.is_empty() {
                new_uid()
            } else {
                s.to_string()
            }
        };
        SeriesContext {
            study_uid: pick(&cfg.study.study_instance_uid),
            series_uid: pick(&cfg.study.series_instance_uid),
            frame_of_reference_uid: pick(&cfg.study.frame_of_reference_uid),
            date: now.format("%Y%m%d").to_string(),
            time: now.format("%H%M%S").to_string(),
        }
    }
}

/// What was written, for the console report.
pub struct Summary {
    pub min: f64,
    pub max: f64,
    pub slope: f64,
    pub clipped_negative: usize,
    pub slope_increased: bool,
}

/// New globally unique UID derived from a random UUID (PS3.5 B.2).
pub fn new_uid() -> String {
    format!("2.25.{}", uuid::Uuid::new_v4().as_u128())
}

/// Format a number as a DICOM Decimal String (max. 16 characters).
fn ds(v: f64) -> String {
    let s = format!("{v}");
    if s.len() <= 16 {
        return s;
    }
    // fixed point with as many decimals as fit (e.g. 103.92799999999998 -> "103.928")
    for prec in (1..=14).rev() {
        let s = format!("{v:.prec$}");
        let s = s.trim_end_matches('0').trim_end_matches('.');
        if s.len() <= 16 && s.parse::<f64>().is_ok_and(|p| p != 0.0) {
            return s.to_string();
        }
    }
    // very large or very small magnitudes
    for prec in (0..=9).rev() {
        let s = format!("{v:.prec$e}");
        if s.len() <= 16 {
            return s;
        }
    }
    format!("{v:e}")
}

/// Implementation Class UID identifying this program (fixed, 2.25 UUID-derived).
const IMPLEMENTATION_CLASS_UID: &str = "2.25.296171474470618474126498346346427302511";

fn ds_multi(v: &[f64]) -> String {
    v.iter().map(|x| ds(*x)).collect::<Vec<_>>().join("\\")
}

fn truncate(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

pub fn write_rt_image(
    img: &Image,
    cfg: &Config,
    ctx: &SeriesContext,
    instance_number: u32,
    source: &Path,
    out: &Path,
) -> Result<Summary, String> {
    let ic = &cfg.image;
    let g = &cfg.geometry;
    let st = &cfg.study;

    // ---- pixel values -------------------------------------------------------
    let factor = cfg.input.wepl_unit.to_mm() / ic.output_unit.to_mm();
    let values: Vec<f64> = img.data.iter().map(|v| v * factor).collect();
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);

    let mut slope = ic.rescale_slope;
    let mut slope_increased = false;
    if max / slope > 65535.0 {
        slope = 10f64.powf((max / 65535.0).log10().ceil());
        slope_increased = true;
    }

    let mut clipped_negative = 0;
    let mut bytes = Vec::with_capacity(values.len() * 2);
    for v in &values {
        let mut p = (v / slope).round();
        if p < 0.0 {
            p = 0.0;
            clipped_negative += 1;
        }
        bytes.extend_from_slice(&(p.min(65535.0) as u16).to_le_bytes());
    }

    // ---- geometry -----------------------------------------------------------
    let [row_sp, col_sp] = ic.pixel_spacing_mm;
    let position = if ic.rt_image_position_mm.len() == 2 {
        [ic.rt_image_position_mm[0], ic.rt_image_position_mm[1]]
    } else {
        // centre of the top-left pixel; image centred on the beam axis, y up
        [
            -((img.cols as f64 - 1.0) / 2.0) * col_sp,
            ((img.rows as f64 - 1.0) / 2.0) * row_sp,
        ]
    };
    let (wc, ww) = if ic.window.len() == 2 {
        (ic.window[0], ic.window[1])
    } else {
        ((max + min) / 2.0, (max - min).max(slope))
    };

    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let label = if ic.rt_image_label.is_empty() {
        &stem
    } else {
        &ic.rt_image_label
    };
    let rescale_type = if ic.rescale_type.is_empty() {
        format!("WEPL_{}", ic.output_unit.name().to_uppercase())
    } else {
        ic.rescale_type.clone()
    };
    let sop_instance_uid = new_uid();

    // ---- dataset ------------------------------------------------------------
    let mut o = InMemDicomObject::new_empty();
    let mut s = |tag: Tag, vr: VR, v: &str| {
        o.put_str(tag, vr, v.to_string());
    };

    // SOP Common
    s(
        tags::SPECIFIC_CHARACTER_SET,
        VR::CS,
        &st.specific_character_set,
    );
    s(tags::INSTANCE_CREATION_DATE, VR::DA, &ctx.date);
    s(tags::INSTANCE_CREATION_TIME, VR::TM, &ctx.time);
    s(tags::SOP_CLASS_UID, VR::UI, uids::RT_IMAGE_STORAGE);
    s(tags::SOP_INSTANCE_UID, VR::UI, &sop_instance_uid);

    // Patient
    s(tags::PATIENT_NAME, VR::PN, &cfg.patient.name);
    s(tags::PATIENT_ID, VR::LO, &cfg.patient.id);
    s(tags::PATIENT_BIRTH_DATE, VR::DA, &cfg.patient.birth_date);
    s(tags::PATIENT_SEX, VR::CS, &cfg.patient.sex);

    // General Study
    s(tags::STUDY_INSTANCE_UID, VR::UI, &ctx.study_uid);
    s(tags::STUDY_DATE, VR::DA, &ctx.date);
    s(tags::STUDY_TIME, VR::TM, &ctx.time);
    s(
        tags::REFERRING_PHYSICIAN_NAME,
        VR::PN,
        &st.referring_physician,
    );
    s(tags::STUDY_ID, VR::SH, &st.study_id);
    s(tags::ACCESSION_NUMBER, VR::SH, &st.accession_number);
    s(tags::STUDY_DESCRIPTION, VR::LO, &st.study_description);

    // RT Series
    s(tags::MODALITY, VR::CS, "RTIMAGE");
    s(tags::SERIES_INSTANCE_UID, VR::UI, &ctx.series_uid);
    s(tags::SERIES_NUMBER, VR::IS, &st.series_number.to_string());
    s(tags::SERIES_DATE, VR::DA, &ctx.date);
    s(tags::SERIES_TIME, VR::TM, &ctx.time);
    s(tags::SERIES_DESCRIPTION, VR::LO, &st.series_description);
    s(tags::OPERATORS_NAME, VR::PN, &st.operator);

    // Frame of Reference
    s(
        tags::FRAME_OF_REFERENCE_UID,
        VR::UI,
        &ctx.frame_of_reference_uid,
    );
    s(tags::POSITION_REFERENCE_INDICATOR, VR::LO, "");

    // General Equipment
    s(tags::MANUFACTURER, VR::LO, &st.manufacturer);
    s(tags::INSTITUTION_NAME, VR::LO, &st.institution_name);
    s(tags::MANUFACTURER_MODEL_NAME, VR::LO, "prad-rti-gen");
    s(tags::SOFTWARE_VERSIONS, VR::LO, env!("CARGO_PKG_VERSION"));

    // General Image
    s(tags::INSTANCE_NUMBER, VR::IS, &instance_number.to_string());
    s(tags::PATIENT_ORIENTATION, VR::CS, "");
    s(tags::CONTENT_DATE, VR::DA, &ctx.date);
    s(tags::CONTENT_TIME, VR::TM, &ctx.time);
    s(
        tags::DERIVATION_DESCRIPTION,
        VR::ST,
        &format!(
            "WEPL map converted from ROOT TTree scan '{}' ({} x {} bins, WEPL in {})",
            source
                .file_name()
                .map(|f| f.to_string_lossy())
                .unwrap_or_default(),
            img.cols,
            img.rows,
            cfg.input.wepl_unit.name()
        ),
    );

    // RT Image
    s(tags::IMAGE_TYPE, VR::CS, &ic.image_type.join("\\"));
    s(tags::CONVERSION_TYPE, VR::CS, &ic.conversion_type);
    s(tags::RT_IMAGE_LABEL, VR::SH, &truncate(label, 16));
    s(
        tags::RT_IMAGE_NAME,
        VR::LO,
        &truncate(&ic.rt_image_name, 64),
    );
    s(tags::RT_IMAGE_DESCRIPTION, VR::ST, &ic.rt_image_description);
    s(tags::RT_IMAGE_PLANE, VR::CS, "NORMAL");
    s(
        tags::X_RAY_IMAGE_RECEPTOR_ANGLE,
        VR::DS,
        &ds(g.x_ray_image_receptor_angle_deg),
    );
    if g.x_ray_image_receptor_translation_mm.len() == 3 {
        s(
            tags::X_RAY_IMAGE_RECEPTOR_TRANSLATION,
            VR::DS,
            &ds_multi(&g.x_ray_image_receptor_translation_mm),
        );
    }
    s(
        tags::IMAGE_PLANE_PIXEL_SPACING,
        VR::DS,
        &ds_multi(&[row_sp, col_sp]),
    );
    s(tags::RT_IMAGE_POSITION, VR::DS, &ds_multi(&position));
    s(
        tags::RADIATION_MACHINE_NAME,
        VR::SH,
        &g.radiation_machine_name,
    );
    s(
        tags::PRIMARY_DOSIMETER_UNIT,
        VR::CS,
        &g.primary_dosimeter_unit,
    );
    s(
        tags::RADIATION_MACHINE_SAD,
        VR::DS,
        &ds(g.radiation_machine_sad_mm),
    );
    s(tags::RT_IMAGE_SID, VR::DS, &ds(g.rt_image_sid_mm));
    s(tags::GANTRY_ANGLE, VR::DS, &ds(g.gantry_angle_deg));
    s(
        tags::BEAM_LIMITING_DEVICE_ANGLE,
        VR::DS,
        &ds(g.beam_limiting_device_angle_deg),
    );
    s(
        tags::PATIENT_SUPPORT_ANGLE,
        VR::DS,
        &ds(g.patient_support_angle_deg),
    );

    // Modality LUT / VOI LUT
    s(tags::RESCALE_INTERCEPT, VR::DS, "0");
    s(tags::RESCALE_SLOPE, VR::DS, &ds(slope));
    s(tags::RESCALE_TYPE, VR::LO, &rescale_type);
    s(tags::WINDOW_CENTER, VR::DS, &ds(wc));
    s(tags::WINDOW_WIDTH, VR::DS, &ds(ww));

    // Image Pixel
    let us = |o: &mut InMemDicomObject, tag: Tag, v: u16| {
        o.put(DataElement::new(tag, VR::US, PrimitiveValue::from(v)));
    };
    o.put_str(tags::PHOTOMETRIC_INTERPRETATION, VR::CS, "MONOCHROME2");
    us(&mut o, tags::SAMPLES_PER_PIXEL, 1);
    us(&mut o, tags::ROWS, img.rows as u16);
    us(&mut o, tags::COLUMNS, img.cols as u16);
    us(&mut o, tags::BITS_ALLOCATED, 16);
    us(&mut o, tags::BITS_STORED, 16);
    us(&mut o, tags::HIGH_BIT, 15);
    us(&mut o, tags::PIXEL_REPRESENTATION, 0);
    o.put(DataElement::new(
        tags::PIXEL_DATA,
        VR::OW,
        PrimitiveValue::from(bytes),
    ));

    // ---- write --------------------------------------------------------------
    let file = o
        .with_meta(
            FileMetaTableBuilder::new()
                .transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN)
                .implementation_class_uid(IMPLEMENTATION_CLASS_UID)
                .implementation_version_name(format!("PRADRTI_{}", env!("CARGO_PKG_VERSION"))),
        )
        .map_err(|e| format!("building file meta: {e}"))?;
    file.write_to_file(out)
        .map_err(|e| format!("writing {}: {e}", out.display()))?;

    Ok(Summary {
        min,
        max,
        slope,
        clipped_negative,
        slope_increased,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_strings_fit_sixteen_characters() {
        for v in [
            0.0,
            0.01,
            1000.0,
            -199.5,
            103.92799999999998,
            1e-12,
            123456789012345678.0,
            -0.000123456789,
        ] {
            let s = ds(v);
            assert!(s.len() <= 16, "{v} -> {s}");
            let back: f64 = s.parse().unwrap();
            assert!((back - v).abs() <= v.abs() * 1e-6 + 1e-15, "{v} -> {s}");
        }
        assert_eq!(ds(103.92799999999998), "103.928");
        assert_eq!(ds(0.01), "0.01");
    }

    #[test]
    fn generated_uids_are_valid() {
        let uid = new_uid();
        assert!(crate::config::is_valid_uid(&uid), "{uid}");
        assert!(crate::config::is_valid_uid(IMPLEMENTATION_CLASS_UID));
    }
}
