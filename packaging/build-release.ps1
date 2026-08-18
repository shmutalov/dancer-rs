# Assemble a release package (ROADMAP M5).
#
# Produces dist/dancer-rs-<version>/ and a zip beside it, containing the exe, the
# neutral default sheet, and the ONNX weights.
#
# The weights are fetched here rather than vendored or downloaded at runtime.
# Vendoring puts 10 MB of binary in git history for something reproducible from a
# URL; fetching at runtime means a first run that needs the network and can fail
# halfway. Fetching at packaging time gives the user a folder that simply works,
# and pins integrity with a checksum: if upstream moves, this fails loudly here
# rather than shipping different weights under the same version.
#
#   pwsh packaging/build-release.ps1

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    # --- what to ship -------------------------------------------------------
    $version = (Select-String -Path 'Cargo.toml' -Pattern '^version\s*=\s*"(.+)"').Matches[0].Groups[1].Value
    $name = "dancer-rs-$version"
    $dist = Join-Path $root "dist/$name"

    # Pinned by content, not by URL. `main` is a moving branch, so the checksum is
    # the only thing making two builds of one version comparable.
    $models = @(
        @{ File = 'mel_spectrogram.onnx';  Sha256 = 'fdd59e65c515331308e4c8841edf99972deca646bdf6197744c2a5b7755e3de9' }
        @{ File = 'beat_this_small.onnx';  Sha256 = 'a5f8d39d989f31859454ba27afe61c5317ca95e4d9373e6853e5361b8937172f' }
    )
    $modelBase = 'https://raw.githubusercontent.com/danigb/beat-this-rs/main/models'

    # --- build --------------------------------------------------------------
    Write-Host "Building $name..." -ForegroundColor Cyan
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

    if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
    New-Item -ItemType Directory -Force -Path $dist | Out-Null
    New-Item -ItemType Directory -Force -Path "$dist/assets" | Out-Null
    New-Item -ItemType Directory -Force -Path "$dist/models" | Out-Null

    Copy-Item 'target/release/dancer-rs.exe' $dist
    Copy-Item 'LICENSE' $dist

    # The neutral default sheet only. FL-Chan is Image-Line's artwork and must
    # never be redistributed (spec §1.3) -- named files, never a wildcard, so a
    # sheet dropped into assets/ cannot be swept into a release by accident.
    foreach ($f in 'default.png', 'default.txt', 'default.toml') {
        Copy-Item "assets/$f" "$dist/assets/"
    }

    # --- weights ------------------------------------------------------------
    $cache = Join-Path $root 'models'
    New-Item -ItemType Directory -Force -Path $cache | Out-Null
    foreach ($m in $models) {
        $cached = Join-Path $cache $m.File
        if (-not (Test-Path $cached)) {
            Write-Host "  fetching $($m.File)..." -ForegroundColor Cyan
            Invoke-WebRequest -Uri "$modelBase/$($m.File)" -OutFile $cached
        }
        $got = (Get-FileHash $cached -Algorithm SHA256).Hash.ToLower()
        if ($got -ne $m.Sha256) {
            # Deliberately fatal. A weights file that is not the one this version
            # was tested against produces different beat grids, silently.
            throw "checksum mismatch for $($m.File)`n  expected $($m.Sha256)`n  got      $got"
        }
        Copy-Item $cached "$dist/models/"
        Write-Host "  $($m.File) ok" -ForegroundColor DarkGray
    }

    Copy-Item 'packaging/README-release.txt' "$dist/README.txt"

    # --- zip ----------------------------------------------------------------
    $zip = Join-Path $root "dist/$name.zip"
    if (Test-Path $zip) { Remove-Item -Force $zip }
    Compress-Archive -Path "$dist/*" -DestinationPath $zip

    $mb = [math]::Round((Get-Item $zip).Length / 1MB, 1)
    Write-Host ""
    Write-Host "$zip  ($mb MB)" -ForegroundColor Green
    Get-ChildItem -Recurse $dist | Where-Object { -not $_.PSIsContainer } |
        ForEach-Object { "  " + $_.FullName.Substring($dist.Length + 1) }
}
finally {
    Pop-Location
}
