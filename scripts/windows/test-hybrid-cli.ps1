$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$testArgs = $args

function Invoke-Build([string[]] $cargoArgs) {
    & cargo build --locked @cargoArgs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

$root = Resolve-Path (Join-Path $PSScriptRoot '../..')
Push-Location $root
try {
    $selected = if ($env:CLI_SUITES) { $env:CLI_SUITES -split '\s+' } else { @() }
    $suites = @(Get-ChildItem -LiteralPath tests -Filter '*cli*.rs' -File |
        Where-Object { !$selected -or $_.BaseName -cin $selected } |
        Sort-Object BaseName | ForEach-Object { $_.BaseName })
    if (!$suites.Count) { throw 'No CLI test suites selected.' }
    $filters = foreach ($suite in $suites) { '--test'; $suite }

    Invoke-Build @('-p', 'vesper', '--bin', 'vesper')
    Invoke-Build @('-p', 'vesper-tools')
    # The hybrid through its test build (#2395): `cargo build` resolves its
    # features without the dev-dependencies, so it reran the build script
    # that compiles every Vesper module after the crate's tests.
    & cargo test --locked --no-run -p vesper-hybrid
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $messages = & cargo test --locked -p vesper --no-run --message-format=json-render-diagnostics @filters
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $executables = @($messages | ConvertFrom-Json | Where-Object {
        $_.reason -eq 'compiler-artifact' -and $_.executable -and
        'test' -in $_.target.kind -and $_.target.name -cin $suites
    } | ForEach-Object { $_.executable })
    if ($executables.Count -ne $suites.Count) {
        throw 'Cargo did not return an executable for every selected CLI suite.'
    }

    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }
    $binary = Join-Path $target 'debug/vesper.exe'
    $backup = [IO.Directory]::CreateTempSubdirectory('vesper-hybrid-cli-')
    $saved = Join-Path $backup.FullName 'vesper.exe'
    try {
        Copy-Item -LiteralPath $binary -Destination $saved
        try {
            Copy-Item -LiteralPath (Join-Path $target 'debug/vesper-hybrid.exe') -Destination $binary -Force
            $failed = @(foreach ($executable in $executables) {
                & $executable @testArgs | Out-Host
                if ($LASTEXITCODE -ne 0) { $executable }
            })
        } finally {
            Copy-Item -LiteralPath $saved -Destination $binary -Force
        }
    } finally {
        $backup.Delete($true)
    }
    Write-Output "cli-suites: $($executables.Count) suites on the hybrid, $($failed.Count) failed"
    foreach ($path in $failed) { Write-Output "  failed: $path" }
    if ($failed.Count) { exit 1 }
} finally {
    Pop-Location
}
exit 0
