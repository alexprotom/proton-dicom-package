//! Configuration file (TOML). The commented template `prad-rti-gen.toml` is
//! compiled in and serves as the built-in defaults; a user file only needs to
//! contain the keys it wants to change.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The commented default configuration (also written by `--init-config`).
pub const DEFAULT_CONFIG: &str = include_str!("../prad-rti-gen.toml");
pub const CONFIG_FILE_NAME: &str = "prad-rti-gen.toml";

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    Mm,
    Cm,
}

impl Unit {
    /// Factor that converts a value in this unit to mm.
    pub fn to_mm(self) -> f64 {
        match self {
            Unit::Mm => 1.0,
            Unit::Cm => 10.0,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Unit::Mm => "mm",
            Unit::Cm => "cm",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub input: InputCfg,
    pub image: ImageCfg,
    pub geometry: GeometryCfg,
    pub patient: PatientCfg,
    pub study: StudyCfg,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputCfg {
    pub x_column: String,
    pub y_column: String,
    pub value_column: String,
    pub wepl_unit: Unit,
    pub y_axis_up: bool,
    pub x_axis_right: bool,
    pub missing_value: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageCfg {
    pub output_unit: Unit,
    pub rescale_slope: f64,
    pub rescale_type: String,
    pub pixel_spacing_mm: [f64; 2],
    pub rt_image_position_mm: Vec<f64>,
    pub image_type: Vec<String>,
    pub conversion_type: String,
    pub rt_image_label: String,
    pub rt_image_name: String,
    pub rt_image_description: String,
    pub window: Vec<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeometryCfg {
    pub radiation_machine_name: String,
    pub primary_dosimeter_unit: String,
    pub radiation_machine_sad_mm: f64,
    pub rt_image_sid_mm: f64,
    pub gantry_angle_deg: f64,
    pub beam_limiting_device_angle_deg: f64,
    pub patient_support_angle_deg: f64,
    pub x_ray_image_receptor_angle_deg: f64,
    pub x_ray_image_receptor_translation_mm: Vec<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatientCfg {
    pub name: String,
    pub id: String,
    pub birth_date: String,
    pub sex: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StudyCfg {
    pub study_instance_uid: String,
    pub series_instance_uid: String,
    pub frame_of_reference_uid: String,
    pub study_id: String,
    pub study_description: String,
    pub accession_number: String,
    pub referring_physician: String,
    pub series_number: i32,
    pub series_description: String,
    pub operator: String,
    pub institution_name: String,
    pub manufacturer: String,
    pub specific_character_set: String,
}

/// Recursively overlay `user` on top of `base` (tables are merged, everything else replaced).
fn merge(base: &mut toml::Table, user: toml::Table) {
    for (k, v) in user {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(u)) => merge(b, u),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

impl Config {
    /// Built-in defaults.
    pub fn default_config() -> Config {
        toml::from_str(DEFAULT_CONFIG).expect("built-in default config must be valid")
    }

    /// Load a config file and overlay it on the built-in defaults.
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read config {}: {e}", path.display()))?;
        let mut base: toml::Table = toml::from_str(DEFAULT_CONFIG).expect("valid default config");
        let user: toml::Table =
            toml::from_str(&text).map_err(|e| format!("config {}: {e}", path.display()))?;
        merge(&mut base, user);
        let cfg: Config = toml::Value::Table(base)
            .try_into()
            .map_err(|e| format!("config {}: {e}", path.display()))?;
        cfg.validate()
            .map_err(|e| format!("config {}: {e}", path.display()))?;
        Ok(cfg)
    }

    /// Find the config file to use when none is given on the command line.
    pub fn locate() -> Option<PathBuf> {
        let cwd = PathBuf::from(CONFIG_FILE_NAME);
        if cwd.is_file() {
            return Some(cwd);
        }
        let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
        let p = exe_dir.join(CONFIG_FILE_NAME);
        p.is_file().then_some(p)
    }

    fn validate(&self) -> Result<(), String> {
        let im = &self.image;
        if im.rescale_slope.is_nan() || im.rescale_slope <= 0.0 {
            return Err("image.rescale_slope must be > 0".into());
        }
        if im.pixel_spacing_mm.iter().any(|v| v.is_nan() || *v <= 0.0) {
            return Err("image.pixel_spacing_mm values must be > 0".into());
        }
        if !matches!(im.rt_image_position_mm.len(), 0 | 2) {
            return Err("image.rt_image_position_mm must be [] or [x, y]".into());
        }
        if !matches!(im.window.len(), 0 | 2) {
            return Err("image.window must be [] or [centre, width]".into());
        }
        if im.image_type.len() < 3 {
            return Err("image.image_type needs at least 3 values, e.g. [\"DERIVED\", \"SECONDARY\", \"RADIOGRAPH\"]".into());
        }
        if !matches!(
            self.geometry.x_ray_image_receptor_translation_mm.len(),
            0 | 3
        ) {
            return Err(
                "geometry.x_ray_image_receptor_translation_mm must be [] or [x, y, z]".into(),
            );
        }
        for (name, uid) in [
            ("study.study_instance_uid", &self.study.study_instance_uid),
            ("study.series_instance_uid", &self.study.series_instance_uid),
            (
                "study.frame_of_reference_uid",
                &self.study.frame_of_reference_uid,
            ),
        ] {
            if !uid.is_empty() && !is_valid_uid(uid) {
                return Err(format!("{name} is not a valid DICOM UID: {uid}"));
            }
        }
        Ok(())
    }
}

pub fn is_valid_uid(uid: &str) -> bool {
    uid.len() <= 64
        && !uid.is_empty()
        && uid.split('.').all(|c| {
            !c.is_empty()
                && c.bytes().all(|b| b.is_ascii_digit())
                && (c == "0" || !c.starts_with('0'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_defaults_parse_and_validate() {
        let cfg = Config::default_config();
        cfg.validate().unwrap();
        assert_eq!(cfg.input.wepl_unit, Unit::Cm);
        assert_eq!(cfg.image.output_unit, Unit::Mm);
    }

    #[test]
    fn a_partial_file_overrides_only_its_keys() {
        let dir = std::env::temp_dir().join(format!("prad-rti-gen-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("partial.toml");
        std::fs::write(&p, "[image]\npixel_spacing_mm = [0.5, 0.25]\n").unwrap();
        let cfg = Config::load(&p).unwrap();
        assert_eq!(cfg.image.pixel_spacing_mm, [0.5, 0.25]);
        assert_eq!(cfg.image.rescale_slope, 0.01);
        assert_eq!(cfg.input.wepl_unit, Unit::Cm);
    }

    #[test]
    fn unknown_keys_are_errors() {
        let dir = std::env::temp_dir().join(format!("prad-rti-gen-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("typo.toml");
        std::fs::write(&p, "[image]\npixel_spacing = [1, 1]\n").unwrap();
        assert!(Config::load(&p).unwrap_err().contains("unknown field"));
    }

    #[test]
    fn uid_syntax() {
        assert!(is_valid_uid("1.2.840.10008.5.1.4.1.1.481.1"));
        assert!(!is_valid_uid("1.02.3"));
        assert!(!is_valid_uid("1..2"));
        assert!(!is_valid_uid("1.2.a"));
    }
}
