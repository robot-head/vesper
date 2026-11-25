$ErrorActionPreference = 'Stop'

$expected = $env:EXPECTED_SHA
if (!$expected) {
    [Console]::Error.WriteLine('EXPECTED_SHA is required')
    exit 2
}

$actual = & git -C (Join-Path $PSScriptRoot '../..') rev-parse HEAD
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if ($actual.Trim() -cne $expected) {
    [Console]::Error.WriteLine("expected checkout $expected, found $actual")
    exit 2
}
