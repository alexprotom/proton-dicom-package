# Proton DICOM Package

Open-source command-line tools for proton radiography and CT data in DICOM,
written in Rust. For research and QA - **not a medical device**.

| tool | does |
|---|---|
| [`prad-rti-gen`](docs/prad-rti-gen.md) | converts proton radiography WEPL maps (ROOT `TTree::Scan` text output) into DICOM RT Image files |

More tools for proton radiographs and CT will join it; they share one
version, one installer and one snap.

## Installing

**Windows** - download `proton-dicom-package-X.Y.Z-windows-x86_64.exe` from
the [latest release](https://github.com/alexprotom/proton-dicom-package/releases/latest)
and run it. It installs per user (no administrator rights) or for all users,
puts the tools on `PATH`, and adds a *Proton DICOM Package prompt* to the
Start menu and **Open Proton DICOM prompt here** to the context menu of
folders. *Start ▶ Update Proton DICOM Package* fetches newer releases.
Details and silent-install options: [installer/README.md](installer/README.md).
Once published to winget:

```text
winget install PDP.ProtonDICOMPackage
```

**Linux** - from the Snap Store ([docs/snap.md](docs/snap.md)):

```text
sudo snap install proton-dicom-package
proton-dicom-package.prad-rti-gen --help
```

or unpack `proton-dicom-package-X.Y.Z-linux-x86_64.tar.gz` from the release
and put its `bin/` on `PATH`.

**From source** (Rust 1.85 or newer):

```text
cargo build --release          # every tool -> target/release/
cargo install --path crates/prad-rti-gen
```

## Quick start

```text
cd D:\data\cylindrical-phantom
prad-rti-gen --init-config                 # writes prad-rti-gen.toml; edit pixel size, SAD/SID, …
prad-rti-gen RG_Output_*.txt -o dicom      # one RT Image per projection, one series
```

## Repository

```text
crates/                one crate per tool (a Cargo workspace; shared version in Cargo.toml)
  prad-rti-gen/
installer/             Windows installer (pdp-setup, pdp-pack) - its own workspace
snap/snapcraft.yaml    snap recipe
docs/                  documentation, shipped with the installer and the tarball
data-test/             test radiographs (Git LFS)
assets/                icon
.github/workflows/     ci.yml (pull requests), release.yml (push to main), snap.yml
```

* [docs/release-versioning.md](docs/release-versioning.md) - branches, CI,
  how a release is made, winget, **adding a tool**.
* [docs/snap.md](docs/snap.md) - the snap: confinement, building,
  publishing.
* [installer/README.md](installer/README.md) - the Windows installer.

The test data in `data-test/` is stored with [Git LFS](https://git-lfs.com).
`git clone` fetches it when Git LFS is installed; `GIT_LFS_SKIP_SMUDGE=1 git
clone …` skips it.

## License

[MIT](LICENSE.txt).
