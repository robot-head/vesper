$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$testArgs = $args

function Invoke-Build([string[]] $cargoArgs) {
    & cargo build --locked @cargoArgs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

function Select-Suites([string] $directory, [string[]] $selected) {
    if (!(Test-Path -LiteralPath $directory -PathType Container)) { return @() }
    Get-ChildItem -LiteralPath $directory -Filter '*cli*.rs' -File |
        Where-Object { !$selected -or $_.BaseName -cin $selected } |
        Sort-Object BaseName | ForEach-Object { $_.BaseName }
}

function Get-TestExecutables([string] $package, [string[]] $names) {
    if (!$names.Count) { return @() }
    $filters = foreach ($name in $names) { '--test'; $name }
    $messages = & cargo test --locked -p $package --no-run --message-format=json-render-diagnostics @filters
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $found = @($messages | ConvertFrom-Json | Where-Object {
        $_.reason -eq 'compiler-artifact' -and $_.executable -and
        'test' -in $_.target.kind -and $_.target.name -cin $names
    } | ForEach-Object { $_.executable })
    if ($found.Count -ne $names.Count) {
        throw 'Cargo did not return an executable for every selected CLI suite.'
    }
    $found
}

$root = Resolve-Path (Join-Path $PSScriptRoot '../..')
Push-Location $root
try {
    $selected = if ($env:CLI_SUITES) { $env:CLI_SUITES -split '\s+' } else { @() }
    $suites = @(Select-Suites 'tests' $selected)
    # The CLI suites that moved out of the root package (#2743) find the
    # Cargo-built hybrid on their own, so they need no binary swap.
    $movedCrate = 'vesper-cli-tests'
    $moved = @(Select-Suites "crates/$movedCrate/tests" $selected)
    if (!$suites.Count -and !$moved.Count) { throw 'No CLI test suites selected.' }

    # The hybrid first, through the same command as the crate's tests
    # (#2395): building vesper-tools first rebuilt dependencies the hybrid
    # shares, which reran its build script over every Vesper module.
    & cargo test --locked --no-run -p vesper-hybrid
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    Invoke-Build @('-p', 'vesper', '--bin', 'vesper')
    Invoke-Build @('-p', 'vesper-tools')
    $executables = @(Get-TestExecutables 'vesper' $suites) + @(Get-TestExecutables $movedCrate $moved)

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
