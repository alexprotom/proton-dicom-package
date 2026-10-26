# Release and versioning

## Version

The whole package has one version, in the root `Cargo.toml`:

```toml
[workspace.package]
version = "0.1.0"
```

Every tool inherits it (`version.workspace = true`). GitHub Actions turns it
into the release tag:

```text
Cargo.toml version 0.1.0 -> Git tag v0.1.0
```

The version is **not incremented automatically**. It must be raised before a
release is merged into `main`, and each version can be released only once:
the [release workflow](../.github/workflows/release.yml) stops if the tag or
the GitHub Release already exists. `Cargo.lock` is committed; run
`cargo build` after changing the version so it is updated too.

## Branches

```text
feature/* -> develop -> release -> main
```

* `develop` - day-to-day integration; every pull request into it runs CI.
* `release` - release candidate; raise the version here and test.
* `main` - what has been released. A push to `main` *is* a release.

## CI

[ci.yml](../.github/workflows/ci.yml) runs on every pull request:

* `cargo fmt --check` and `cargo clippy -D warnings` (Linux and Windows),
* `cargo test --workspace` on Linux and Windows,
* a check that every tool is also in the snap recipe,
* the Windows installer: its format, lints and tests, then a debug build of
  the tools packed into an installer that is installed silently, used for a
  conversion, and uninstalled again
  ([smoke-test-installer.ps1](../.github/scripts/smoke-test-installer.ps1)).

*Actions ▶ CI ▶ Run workflow* runs the same on any branch.

## Creating a release

1. Merge the release-ready work from `develop` into `release`.
2. Raise the version in the root `Cargo.toml` (e.g. `0.1.0` → `0.2.0`),
   run `cargo build` to refresh `Cargo.lock`, commit.
3. Test the candidate - *Actions ▶ Snap ▶ Run workflow* on `release` builds
   the snap and can put it on the `edge` or `beta` channel.
4. Open a pull request `release` → `main` and merge it.

The push to `main` starts [release.yml](../.github/workflows/release.yml),
which

1. reads the version and checks that it has not been released;
2. builds the tools and the installer for Windows, packs them
   (`pdp-pack`, with the winget manifests), and installs, uses and
   uninstalls the result on the runner;
3. builds the Linux tarball;
4. builds and tests the snap and releases it to the Snap Store, when
   `SNAPCRAFT_STORE_CREDENTIALS` is set ([snap.md](snap.md));
5. writes `SHA256SUMS` and creates the GitHub Release `v<version>` with the
   files below;
6. submits the version to winget, when `WINGET_TOKEN` is set.

Afterwards merge `main` back into `develop`, so both carry the new version.

## Release files

```text
proton-dicom-package-X.Y.Z-windows-x86_64.exe          Windows installer
proton-dicom-package-X.Y.Z-linux-x86_64.tar.gz         Linux executables, README, docs
proton-dicom-package-X.Y.Z-winget-manifests.zip        for the first winget submission
SHA256SUMS
```

The installed program updates itself from these (*Start ▶ Update Proton
DICOM Package*, `pdp-setup.exe --update`): it follows GitHub's "latest
release" link, downloads the Windows installer by the name above and runs it
only if its hash matches the entry in `SHA256SUMS`. So:

* keep the file names and `SHA256SUMS` exactly as the workflow writes them;
* a release marked as a pre-release, or left as a draft, is never offered as
  an update.

## winget

The package identifier is `PDP.ProtonDICOMPackage`.

### First submission (once, by hand)

1. Let the release workflow publish a version, download
   `proton-dicom-package-X.Y.Z-winget-manifests.zip` from the release and
   unpack it into a folder `manifests`.
2. Optionally test it on Windows, from an elevated prompt:

   ```text
   winget settings --enable LocalManifestFiles
   winget validate --manifest .\manifests
   winget install --manifest .\manifests
   ```

3. Submit it with [wingetcreate](https://github.com/microsoft/winget-create):

   ```text
   wingetcreate submit --token <personal access token> .\manifests
   ```

   or by a pull request to `microsoft/winget-pkgs` adding the files under
   `manifests/p/PDP/ProtonDICOMPackage/X.Y.Z/`.
4. Wait for validation and the merge.

### Every later release (automatic)

1. Fork `microsoft/winget-pkgs` to your GitHub account.
2. Create a classic personal access token with the `public_repo` scope.
3. Save it as the repository secret `WINGET_TOKEN`.

Add the secret only after the first version has been merged - the job
updates an existing package. Without it the job is skipped.

## Adding a tool

1. Create the crate: `cargo new --bin crates/<tool>`; in its `Cargo.toml`
   inherit the workspace fields (`version.workspace = true`, …), give it a
   `description` (the installer and the prompt show it) and take shared
   dependencies with `<dep>.workspace = true`.
2. Add it to the snap: its path under `rust-path` and an app
   `<tool>: command: bin/<tool>` in [snap/snapcraft.yaml](../snap/snapcraft.yaml)
   (CI fails until both are there).
3. Document it in `docs/<tool>.md` and in the table of the README.

The Windows installer needs nothing: `pdp-pack` finds every crate under
`crates/` that builds an executable, and the release workflow builds the
whole workspace.
