# The snap package

The tools are published to the [Snap Store](https://snapcraft.io/proton-dicom-package)
as `proton-dicom-package`. The recipe is [snap/snapcraft.yaml](../snap/snapcraft.yaml).

## Installing

```text
sudo snap install proton-dicom-package
```

Each tool is a command named `proton-dicom-package.<tool>`:

```text
proton-dicom-package.prad-rti-gen --help
```

A short name is one alias away (per machine, once):

```text
sudo snap alias proton-dicom-package.prad-rti-gen prad-rti-gen
```

Updates arrive by themselves; `sudo snap refresh proton-dicom-package`
fetches one at once, `sudo snap revert proton-dicom-package` goes back.

## What the tools may read and write

A snap runs confined: it sees what its interfaces grant and nothing else.

| Interface | Connected | Grants |
|---|---|---|
| `home` | automatically | your home folder, **except hidden files and folders** at its top level |
| `removable-media` | **by hand, once** | `/media`, `/run/media` and `/mnt`: USB drives, mounted shares |

```text
sudo snap connect proton-dicom-package:removable-media
```

Data elsewhere (`/data`, a second disk mounted somewhere else) is not
reachable from a strictly confined snap: mount it under `/mnt` or `/media`,
or bind-mount it into your home folder. `/tmp` is the snap's own private
`/tmp`, not the system's. The configuration file is read from the current
folder (`prad-rti-gen.toml`) or given with `-c`, so it has to be in a place
the snap can read too.

## The recipe

* **`base: core24`, strict confinement** - the tools need nothing but files.
* **One part, `plugin: rust`**: `cargo install --locked --path <crate>` for
  every crate in `rust-path`, with the workspace's `Cargo.lock` and release
  profile, using the current stable toolchain from rustup. The version is
  read from the root `Cargo.toml`, with the same expression the release
  workflow uses.
* **One app per tool.** A new tool is added to `rust-path` and as an app;
  CI checks that none is forgotten
  ([check-tool-lists.sh](../.github/scripts/check-tool-lists.sh)).
* **Only `amd64`.** Nothing is x86-specific; an `arm64:` entry under
  `platforms` and an `ubuntu-24.04-arm` runner are all an ARM build needs.

## Building it locally

```text
sudo snap install snapcraft --classic
sudo snap install lxd && sudo lxd init --auto
snapcraft pack
sudo snap install --dangerous ./proton-dicom-package_<version>_amd64.snap
```

Run it from a fresh clone without the LFS test data (`GIT_LFS_SKIP_SMUDGE=1
git clone …`): snapcraft copies the whole folder into its build container.
Without a Linux machine, *Actions ▶ Snap ▶ Run workflow* builds any branch
and attaches the snap to the run (`linux-snap`), and can release that build
to `edge`, `beta` or `candidate`:

```text
sudo snap install proton-dicom-package --edge
```

## Publishing

[snap.yml](../.github/workflows/snap.yml) builds the snap, installs it on the
runner, checks that every tool starts inside the confinement and that
`prad-rti-gen` converts a radiograph, uploads the snap as an artifact and -
when the secret `SNAPCRAFT_STORE_CREDENTIALS` is set - to the store. The
release workflow calls it on every push to `main` with the `stable` channel
(or the one in the repository variable `SNAP_CHANNEL`).

### Once, by hand

1. Create an account at <https://snapcraft.io> (Ubuntu One).
2. On a machine with snapcraft: `snapcraft login`, then
   `snapcraft register proton-dicom-package`.
3. Upload the first build yourself - the `linux-snap` artifact of a *Snap*
   workflow run:

   ```text
   snapcraft upload --release=edge proton-dicom-package_<version>_amd64.snap
   ```

4. Fill in the store listing (category *Science*) at
   <https://snapcraft.io/proton-dicom-package/listing>.
5. Export a login for the workflow, limited to this snap:

   ```text
   snapcraft export-login --snaps=proton-dicom-package \
     --acls package_access,package_push,package_update,package_release \
     snap-credentials.txt
   ```

   Save the file's content as the repository secret
   `SNAPCRAFT_STORE_CREDENTIALS` and delete the file. The login expires;
   when uploads start failing with an authentication error, export a new
   one.
6. Optional: set the repository variable `SNAP_CHANNEL` to `candidate` to
   have releases land there first and promote them by hand
   (`snapcraft release proton-dicom-package <revision> stable`).

Automatic aliases (`prad-rti-gen` without the prefix) and an automatically
connected `removable-media` are granted by the store reviewers on request:
a post in the [store-requests](https://forum.snapcraft.io/c/store-requests/19)
forum category explaining the use.
