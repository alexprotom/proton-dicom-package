# Installs a packed Proton DICOM Package installer silently for the current
# user, checks what it did, runs a tool from the installation, and uninstalls
# it again. Used by the CI and release workflows on a Windows runner.
#
#   pwsh .github/scripts/smoke-test-installer.ps1 -Installer <setup.exe>

param(
    [Parameter(Mandatory = $true)] [string] $Installer
)

$ErrorActionPreference = 'Stop'
$ProductKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\ProtonDicomPackage'

function Invoke-Setup([string] $Exe, [string[]] $Arguments) {
    # Start-Process waits for a GUI-subsystem program too, which `&` does not.
    $p = Start-Process -FilePath $Exe -ArgumentList $Arguments -Wait -PassThru -NoNewWindow
    return $p.ExitCode
}

function Fail([string] $Message) {
    Write-Host "::error::$Message"
    exit 1
}

# ---- install -----------------------------------------------------------------
Write-Host "Installing $Installer"
$code = Invoke-Setup $Installer @('--silent', '--just-me')
if ($code -ne 0) { Fail "the silent installation exited with $code" }

$entry = Get-ItemProperty -Path $ProductKey -ErrorAction SilentlyContinue
if (-not $entry) { Fail 'no Apps & features entry was written' }
$dir = $entry.InstallLocation
Write-Host "Installed $($entry.DisplayVersion) into $dir"

foreach ($f in @('pdp-setup.exe', 'pdp-shell.cmd', 'install-manifest.txt', 'tools.txt', 'LICENSE.txt')) {
    if (-not (Test-Path (Join-Path $dir $f))) { Fail "$f is missing from $dir" }
}
$tools = Get-Content (Join-Path $dir 'tools.txt') |
    Where-Object { $_ -match '^\s*[^#\s][^=]*=' } |
    ForEach-Object { ($_ -split '=', 2)[0].Trim() }
if (-not $tools) { Fail 'tools.txt lists no tools' }
foreach ($t in $tools) {
    $exe = Join-Path $dir "$t.exe"
    if (-not (Test-Path $exe)) { Fail "$t.exe is missing" }
    & $exe --version
    if ($LASTEXITCODE -ne 0) { Fail "$t --version exited with $LASTEXITCODE" }
}

$userPath = (Get-ItemProperty -Path 'HKCU:\Environment').Path
if (($userPath -split ';') -notcontains $dir) { Fail "$dir was not added to the user PATH" }
$menu = 'HKCU:\Software\Classes\Directory\shell\ProtonDicomPackage\command'
if (-not (Test-Path $menu)) { Fail 'the folder context-menu entry is missing' }
Write-Host "PATH entry, folder menu and Apps & features entry are in place"

# ---- a conversion with the installed tool -----------------------------------
$work = Join-Path $env:RUNNER_TEMP 'pdp-smoke'
New-Item -ItemType Directory -Force -Path $work | Out-Null
$lines = @(
    '************************************************',
    '*    Row   *       x.x *       y.y * WEPL.WEPL *',
    '************************************************'
)
$row = 0
foreach ($x in 0..3) {
    foreach ($y in 0..2) {
        $w = (0.1 * ($x + 10 * $y)).ToString('F7', [Globalization.CultureInfo]::InvariantCulture)
        $lines += ('* {0,8} * {1,9} * {2,9} * {3,9} *' -f $row, $x, $y, $w)
        $row++
    }
}
$lines += '************************************************'
Set-Content -Path (Join-Path $work 'RG_Output_0.txt') -Value $lines -Encoding ascii
& (Join-Path $dir 'prad-rti-gen.exe') (Join-Path $work 'RG_Output_0.txt') -o (Join-Path $work 'dicom')
if ($LASTEXITCODE -ne 0) { Fail "prad-rti-gen exited with $LASTEXITCODE" }
$dcm = Join-Path $work 'dicom\RG_Output_0.dcm'
$bytes = [System.IO.File]::ReadAllBytes($dcm)
if ([System.Text.Encoding]::ASCII.GetString($bytes, 128, 4) -ne 'DICM') { Fail "$dcm is not a DICOM file" }
Write-Host "Converted a radiograph with the installed prad-rti-gen ($($bytes.Length) bytes)"

# ---- uninstall ---------------------------------------------------------------
# The setup program in the folder hands over to a copy in %TEMP% and returns,
# so wait for the folder to go.
$code = Invoke-Setup (Join-Path $dir 'pdp-setup.exe') @('--uninstall', '--silent')
if ($code -ne 0) { Fail "the silent uninstallation exited with $code" }
$deadline = (Get-Date).AddSeconds(90)
while ((Test-Path $dir) -and ((Get-Date) -lt $deadline)) { Start-Sleep -Seconds 2 }
if (Test-Path $dir) {
    Get-ChildItem -Recurse $dir | ForEach-Object { Write-Host "  left: $($_.FullName)" }
    Fail "$dir was not removed"
}
if (Test-Path $ProductKey) { Fail 'the Apps & features entry was not removed' }
if (Test-Path $menu) { Fail 'the folder context-menu entry was not removed' }
$userPath = (Get-ItemProperty -Path 'HKCU:\Environment').Path
if (($userPath -split ';') -contains $dir) { Fail "$dir is still on the user PATH" }
Write-Host 'Uninstalled cleanly'
