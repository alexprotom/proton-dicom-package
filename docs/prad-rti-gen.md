# prad-rti-gen

Converts proton radiography WEPL maps into DICOM **RT Image** files
(SOP class 1.2.840.10008.5.1.4.1.1.481.1).

## Input

ROOT `TTree::Scan()` text output with the bin indices and the
water-equivalent path length per bin:

```text
************************************************
*    Row   *       x.x *       y.y * WEPL.WEPL *
************************************************
*        0 *         0 *         0 *         0 *
*        1 *         0 *         1 * 0.3162373 *
...
```

* The columns are found by name (`x.x`, `y.y`, `WEPL.WEPL` by default, see
  `[input]` below), so extra columns such as `Instance` do not matter.
* `x` and `y` are integer bin indices; the grid size comes from their range,
  so any rectangular grid works. Bins missing from the file, and `nan`
  values, become `input.missing_value` (0).
* The pager lines of an interactive scan (`Type <CR> to continue …`, repeated
  headers, `==> N selected entries`) are skipped.

## Running it

```text
prad-rti-gen RG_Output_0.txt                  # -> RG_Output_0.dcm next to the input
prad-rti-gen RG_Output_*.txt -o dicom         # several files -> folder "dicom"
prad-rti-gen D:\runs\phantom -o dicom         # every *.txt in a folder
prad-rti-gen -c phantomA.toml RG_Output_0.txt # explicit configuration file
prad-rti-gen --init-config                    # write a commented prad-rti-gen.toml
```

| option | |
|---|---|
| `-o, --out-dir <DIR>` | output folder (default: next to each input file) |
| `-c, --config <FILE>` | configuration file |
| `-f, --force` | overwrite existing output files (otherwise they are skipped) |
| `--init-config [FILE]` | write the commented default configuration and exit |
| `-V, --version`, `-h, --help` | |

* Wildcards (`*`, `?`) work in `cmd.exe` too, which does not expand them
  itself.
* Files are processed in natural order - `RG_Output_2` before
  `RG_Output_10` - and all files of one run share one Study, Series and
  Frame of Reference UID, with consecutive Instance Numbers.
* The exit status is 0 when every file was converted, 1 otherwise.
* In the snap the command is `proton-dicom-package.prad-rti-gen`
  ([snap.md](snap.md)).

## Configuration

`prad-rti-gen.toml` is looked up in the current folder, then next to the
executable; without one the built-in defaults apply. `--init-config` writes
the commented template. Every key is optional - a file only needs the keys
it changes - and an unknown key is an error, so a typo does not pass
silently.

| key | default | meaning |
|---|---|---|
| `input.wepl_unit` | `"cm"` | unit of the WEPL values in the text file (`cm` or `mm`) |
| `input.x_column`, `y_column`, `value_column` | `x.x`, `y.y`, `WEPL.WEPL` | column names in the scan header |
| `input.y_axis_up` | `true` | y increases upwards (ROOT / IEC): the highest y is the top row |
| `input.x_axis_right` | `true` | x increases to the right |
| `input.missing_value` | `0.0` | WEPL for absent or NaN bins |
| `image.output_unit` | `"mm"` | unit of the WEPL stored in DICOM |
| `image.rescale_slope` | `0.01` | WEPL per stored pixel value; raised (next power of ten, with a warning) if the maximum would not fit 16 bits |
| `image.rescale_type` | `""` → `WEPL_MM` | Rescale Type (0028,1054) |
| `image.pixel_spacing_mm` | `[1.0, 1.0]` | Image Plane Pixel Spacing [row, column] at the image plane |
| `image.rt_image_position_mm` | `[]` → centred | RT Image Position of the top-left pixel |
| `image.image_type` | `DERIVED\SECONDARY\RADIOGRAPH` | Image Type |
| `image.window` | `[]` → min…max | Window Center / Width |
| `geometry.radiation_machine_sad_mm`, `rt_image_sid_mm` | `1000`, `1000` | SAD and SID; SID = SAD for an image reconstructed at the isocentre plane |
| `geometry.gantry_angle_deg`, `patient_support_angle_deg`, … | `0` | angles |
| `patient.name`, `id`, `birth_date`, `sex` | `PRAD^Phantom`, `PRAD0001` | patient module |
| `study.*` | | study and series descriptions; empty UIDs are generated |

To attach the radiographs to an existing planning CT, set
`study.study_instance_uid` and `study.frame_of_reference_uid` to the CT's
UIDs and use the same patient name and ID.

## What is written

* Pixels: 16-bit unsigned, MONOCHROME2, Explicit VR Little Endian, with
  `WEPL [output_unit] = pixel × RescaleSlope + RescaleIntercept`
  (intercept 0). With the defaults that is WEPL in mm with 0.01 mm steps.
* RT Image module: RT Image Plane `NORMAL`, Image Plane Pixel Spacing, RT
  Image Position, SAD, SID, gantry / collimator / couch angles, RT Image
  Label (the input file name, at most 16 characters).
* Derivation Description names the source file, the grid and the input unit.
* Window Center / Width span the image, so a viewer shows it sensibly at
  once.
