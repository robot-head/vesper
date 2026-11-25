$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

$root = Resolve-Path (Join-Path $PSScriptRoot '../..')
Push-Location $root
try {
    if ($env:VESPER_PORT_HARNESS_SHARD) {
        throw 'The export ABI tier takes no shard.'
    }
    if (!$env:VESPER_PORT_COMPILER) {
        & cargo build --locked --bin vesper
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }
        $env:VESPER_PORT_COMPILER = (Resolve-Path (Join-Path $target 'debug/vesper.exe')).Path
    }

    $selected = if ($env:VESPER_PORT_HARNESS_CRATES) { $env:VESPER_PORT_HARNESS_CRATES -split '\s+' } else { @('all') }
    $plan = Get-Content -Raw -LiteralPath 'scripts/port-abi-plan.json' | ConvertFrom-Json
    foreach ($group in $plan) {
        if ('all' -notin $selected -and !($group.crates | Where-Object { $_ -cin $selected })) {
            continue
        }
        foreach ($check in $group.checks) {
            $cargoArgs = @('test', '--locked') + $check.args
            if (!$check.description) {
                & cargo @cargoArgs
                if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
                continue
            }
            $log = (& cargo @cargoArgs 2>&1) -join "`n"
            $status = $LASTEXITCODE
            $expected = @(
                $check.failed | ForEach-Object { if ($_) { "test $_ ... FAILED" } }
                $check.passed | ForEach-Object { if ($_) { "test $_ ... ok" } }
                if ($check.diagnostic) { $check.diagnostic }
            )
            if ($status -eq 0 -or ($expected | Where-Object { !$log.Contains($_) })) {
                Write-Output $log
                throw "Divergence check did not fail as expected: $($check.description)"
            }
            Write-Output "Detected divergent $($check.description)"
        }
    }
    Write-Output 'export ABI tier done; ports run on Linux'
} finally {
    Pop-Location
}
exit 0
