param(
    [switch]$Online
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $repoRoot
try {
    if (-not $env:RUSTUP_HOME -and (Test-Path "D:\Rust\.rustup")) {
        $env:RUSTUP_HOME = "D:\Rust\.rustup"
    }
    if (-not $env:CARGO_HOME -and (Test-Path "D:\Rust\.cargo")) {
        $env:CARGO_HOME = "D:\Rust\.cargo"
    }
    if ((Test-Path "D:\Rust\.cargo\bin") -and ($env:Path -notlike "*D:\Rust\.cargo\bin*")) {
        $env:Path = "D:\Rust\.cargo\bin;" + $env:Path
    }

    $cargoArgs = @("build", "--target", "wasm32-unknown-unknown", "--release")
    if (-not $Online) {
        $cargoArgs += "--offline"
    }
    Write-Host "Building Wasm binary..."
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with exit code $LASTEXITCODE"
    }

    $rawWasm = Join-Path $repoRoot "target\wasm32-unknown-unknown\release\smooth_fdm_threads.wasm"
    if (-not (Test-Path $rawWasm)) {
        throw "Missing raw wasm binary at $rawWasm"
    }

    $distRoot = Join-Path $repoRoot "dist"
    $pkgDir = Join-Path $distRoot "smooth-fdm-threads"
    $assetsOut = Join-Path $pkgDir "assets"
    $zipPath = Join-Path $distRoot "smooth-fdm-threads.zip"

    if (Test-Path $pkgDir) {
        Remove-Item $pkgDir -Recurse -Force
    }
    if (Test-Path $zipPath) {
        Remove-Item $zipPath -Force
    }
    New-Item -ItemType Directory -Force -Path $assetsOut | Out-Null

    $compWasm = Join-Path $pkgDir "plugin.wasm"
    Write-Host "Wrapping Wasm component..."
    & wasm-tools component new $rawWasm -o $compWasm
    if ($LASTEXITCODE -ne 0) {
        throw "wasm-tools component new failed"
    }

    & wasm-tools validate $compWasm
    if ($LASTEXITCODE -ne 0) {
        throw "wasm-tools validate failed"
    }

    Copy-Item (Join-Path $repoRoot "plugin.json") (Join-Path $pkgDir "plugin.json") -Force
    $assetFiles = Get-ChildItem (Join-Path $repoRoot "assets") -Filter "*.svg" -File
    if (-not $assetFiles) {
        throw "No SVG icon files found in assets\"
    }
    foreach ($asset in $assetFiles) {
        Copy-Item $asset.FullName (Join-Path $assetsOut $asset.Name) -Force
    }

    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::Open($zipPath, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        $relFiles = @("plugin.json", "plugin.wasm") + ($assetFiles | ForEach-Object { "assets/$($_.Name)" })
        foreach ($rel in $relFiles) {
            $src = Join-Path $pkgDir ($rel -replace "/", "\")
            $entryName = "smooth-fdm-threads/$rel"
            [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
                $zip,
                $src,
                $entryName,
                [System.IO.Compression.CompressionLevel]::Optimal
            ) | Out-Null
        }
    } finally {
        $zip.Dispose()
    }

    $wasmSize = (Get-Item $compWasm).Length
    $zipSize = (Get-Item $zipPath).Length
    Write-Host "Packaged component: $wasmSize bytes | Zip: $zipSize bytes"

    $scCli = "C:\Program Files\SolidCraft\solidcraft-cli.exe"
    if (Test-Path $scCli) {
        Write-Host "Inspecting package with solidcraft-cli..."
        $pkgForward = $pkgDir -replace "\\", "/"
        $zipForward = $zipPath -replace "\\", "/"
        $inspectArg = "{`"path`":`"$pkgForward`"}" -replace '"', '\"'
        $inspectJson = (& $scCli exec plugin.inspect $inspectArg) -join "`n"
        if ($LASTEXITCODE -ne 0) {
            throw "plugin.inspect failed: $inspectJson"
        }
        Write-Output $inspectJson

        Write-Host "Dry-running zip installation with solidcraft-cli..."
        $dryArg = "{`"path`":`"$zipForward`",`"dry_run`":true}" -replace '"', '\"'
        $dryJson = (& $scCli exec plugin.install $dryArg) -join "`n"
        if ($LASTEXITCODE -ne 0) {
            throw "plugin.install dry_run failed: $dryJson"
        }
        Write-Output $dryJson
    }
} finally {
    Pop-Location
}
