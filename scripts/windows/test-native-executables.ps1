$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

$toolRoot = Resolve-Path (Join-Path $PSScriptRoot '../..')
$root = if ($env:PLATFORM_WORKSPACE) { Resolve-Path -LiteralPath $env:PLATFORM_WORKSPACE } else { $toolRoot }
Push-Location $root
try {
    if (!$env:VESPER_NATIVE_BINARY) {
        throw 'Set VESPER_NATIVE_BINARY to the compiler executable.'
    }
    $compiler = (Resolve-Path -LiteralPath $env:VESPER_NATIVE_BINARY).Path
    $casesPath = 'scripts/native-example-cases.txt'
    if (!(Test-Path -LiteralPath $casesPath -PathType Leaf)) {
        $casesPath = Join-Path $toolRoot $casesPath
    }
    $cases = @(Get-Content -LiteralPath $casesPath | Where-Object { $_ })
    if (!$cases.Count) {
        throw 'No native example cases.'
    }
    $artifacts = [IO.Directory]::CreateTempSubdirectory('vesper-native-')
    try {
        foreach ($case in $cases) {
            $name, $expected = $case -split ':', 2
            $output = Join-Path $artifacts.FullName "$name${env:VESPER_NATIVE_SUFFIX}"
            & $compiler build "examples/$name.ves" -o $output
            if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
            & $output
            if ($LASTEXITCODE -ne [int]$expected) {
                throw "${name}: expected $expected, got $LASTEXITCODE"
            }
            Write-Output "${name}: host exit $expected"
        }
    } finally {
        $artifacts.Delete($true)
    }
} finally {
    Pop-Location
}
exit 0
