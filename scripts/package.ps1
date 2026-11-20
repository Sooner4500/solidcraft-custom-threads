param(
    [switch]$Online,
    [switch]$SkipPluginTest
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

    $rawWasm = Join-Path $repoRoot "target\wasm32-unknown-unknown\release\custom_threads.wasm"
    if (-not (Test-Path $rawWasm)) {
        throw "Missing raw wasm binary at $rawWasm"
    }

    $distRoot = Join-Path $repoRoot "dist"
    $pkgDir = Join-Path $distRoot "custom-threads"
    $assetsOut = Join-Path $pkgDir "assets"
    $zipPath = Join-Path $distRoot "custom-threads.zip"

    foreach ($stale in @((Join-Path $distRoot "smooth-fdm-threads"), (Join-Path $distRoot "smooth-fdm-threads.zip"), $pkgDir, $zipPath)) {
        if (Test-Path $stale) {
            Remove-Item $stale -Recurse -Force
        }
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

    # Minify plugin.json without BOM and verify SolidCraft's 16 KiB manifest cap.
    $manifestRaw = Get-Content (Join-Path $repoRoot "plugin.json") -Raw -Encoding UTF8
    $manifestObj = $manifestRaw | ConvertFrom-Json
    $manifestMin = $manifestObj | ConvertTo-Json -Depth 32 -Compress
    $manifestOut = Join-Path $pkgDir "plugin.json"
    $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($manifestOut, $manifestMin, $utf8NoBom)
    $manifestBytes = (Get-Item $manifestOut).Length
    Write-Host "Minified plugin.json: $manifestBytes bytes (limit: 16384)"
    if ($manifestBytes -gt 16384) {
        throw "Minified plugin.json exceeds SolidCraft 16 KiB limit ($manifestBytes > 16384)"
    }

    $assetFiles = Get-ChildItem (Join-Path $repoRoot "assets") -Filter "*.svg" -File
    if (-not $assetFiles) {
        throw "No SVG icon files found in assets\"
    }
    foreach ($asset in $assetFiles) {
        Copy-Item $asset.FullName (Join-Path $assetsOut $asset.Name) -Force
    }

    $testsSrc = Join-Path $repoRoot "tests"
    if (-not (Test-Path $testsSrc)) {
        throw "Missing tests directory at $testsSrc"
    }
    $topCases = Get-ChildItem $testsSrc -Filter "*.json" -File
    if (-not $topCases) {
        throw "No *.json test cases found in tests\"
    }
    $testsOut = Join-Path $pkgDir "tests"
    Copy-Item $testsSrc $testsOut -Recurse -Force
    $testFiles = Get-ChildItem $testsOut -Recurse -File
    $testRelFiles = $testFiles | ForEach-Object {
        "tests/" + ($_.FullName.Substring($testsOut.Length + 1) -replace "\\", "/")
    }

    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::Open($zipPath, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        $relFiles = @("plugin.json", "plugin.wasm") + ($assetFiles | ForEach-Object { "assets/$($_.Name)" }) + $testRelFiles
        foreach ($rel in $relFiles) {
            $src = Join-Path $pkgDir ($rel -replace "/", "\")
            $entryName = "custom-threads/$rel"
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
    Write-Host "Packaged component: $wasmSize bytes | Zip: $zipSize bytes | SVGs: $($assetFiles.Count) | Test files: $($testFiles.Count)"

    $releasesDir = Join-Path $repoRoot "releases"
    New-Item -ItemType Directory -Force -Path $releasesDir | Out-Null
    Copy-Item $zipPath (Join-Path $releasesDir "custom-threads.zip") -Force
    Copy-Item $zipPath (Join-Path $releasesDir ("custom-threads-v" + $manifestObj.version + ".zip")) -Force

    $scCli = "C:\Program Files\SolidCraft\solidcraft-cli.exe"
    if (Test-Path $scCli) {
        Write-Host "Checking package with solidcraft-cli plugin check..."
        $checkOut = (& $scCli plugin check $zipPath --json) -join "`n"
        if ($LASTEXITCODE -ne 0) {
            throw "plugin check failed: $checkOut"
        }
        Write-Output $checkOut

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

        if (-not $SkipPluginTest) {
            Write-Host "Running solidcraft-cli plugin test --require-cases..."
            $testOut = (& $scCli plugin test $zipPath --json --require-cases) -join "`n"
            if ($LASTEXITCODE -ne 0) {
                throw "plugin test failed: $testOut"
            }
            Write-Output $testOut
        }
    }
} finally {
    Pop-Location
}
