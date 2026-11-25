$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$cwd = $PWD.ProviderPath
$target = '@wasmtime//:wasmtime'
$version = '50.0.1'
$usage = 'usage: bootstrap-stage2.ps1 <which|precompile|run|verify> ...'

function Stop-Stage2([string]$Message, [int]$Code = 1) {
    [Console]::Error.WriteLine($Message)
    exit $Code
}

function Get-Wasmtime {
    if ($env:WASMTIME_BIN) { return $env:WASMTIME_BIN }
    if ($env:WASMTIME) { return $env:WASMTIME }
    $binary = Get-Command wasmtime -CommandType Application -ErrorAction SilentlyContinue
    if ($binary) { return @($binary)[0].Source }
    $bazel = Get-Command bazel -CommandType Application -ErrorAction SilentlyContinue
    if (!$bazel) { $bazel = Get-Command bazelisk -CommandType Application -ErrorAction SilentlyContinue }
    if ($bazel) {
        $command = @($bazel)[0].Source
        Push-Location -LiteralPath $root
        try {
            & $command build $target | ForEach-Object { [Console]::Error.WriteLine($_) }
            if ($LASTEXITCODE) { exit $LASTEXITCODE }
            $outputs = & $command cquery --output=files $target 2>$null
            if (!$LASTEXITCODE -and $outputs) {
                $candidate = [IO.Path]::GetFullPath(@($outputs)[0], $root)
                if ([IO.File]::Exists($candidate)) { return $candidate }
            }
        }
        finally { Pop-Location }
    }
    $external = Join-Path $root 'bazel-bin/external'
    if ([IO.Directory]::Exists($external)) {
        $directories = @(Get-ChildItem -LiteralPath $external -Directory -Filter '+wasmtime+wasmtime_*' | Sort-Object Name)
        foreach ($name in 'wasmtime.exe', 'wasmtime') {
            foreach ($directory in $directories) {
                $candidate = Join-Path $directory.FullName $name
                if ([IO.File]::Exists($candidate)) { return $candidate }
            }
        }
    }
    Stop-Stage2 'Could not find Wasmtime (set WASMTIME_BIN, put wasmtime on PATH, or use Bazel)'
}

function Get-WasmtimeVersion([string]$Binary) {
    $reported = (& $Binary --version) -join "`n"
    if ($LASTEXITCODE) { exit $LASTEXITCODE }
    $fields = $reported -split '\s+'
    if (($fields.Count -lt 2 -or $fields[1] -ne $version) -and $env:VESPER_ALLOW_UNPINNED_WASMTIME -ne '1') {
        Stop-Stage2 "Expected wasmtime $version, got: $reported (set VESPER_ALLOW_UNPINNED_WASMTIME=1 to override)"
    }
    return $reported
}

function Compile-Stage2([string]$Binary, [string]$Source, [string]$Output) {
    $full = [IO.Path]::GetFullPath($Output, $cwd)
    [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($full))
    [IO.File]::Delete($full)
    & $Binary compile $Source -o $Output
    if ($LASTEXITCODE) { exit $LASTEXITCODE }
}

function Invoke-Stage2([string]$Binary, [string]$Source, [string]$Package, [string]$Output,
                       [string]$Entry, [bool]$Precompiled, [string]$Symbols) {
    $full = [IO.Path]::GetFullPath($Output, $cwd)
    $directory = [IO.Path]::GetDirectoryName($full)
    [void][IO.Directory]::CreateDirectory($directory)
    foreach ($suffix in '', '.err.txt', '.stage', '.syms') { [IO.File]::Delete($full + $suffix) }
    $flags = @('run')
    if ($Precompiled) { $flags += '--allow-precompiled' }
    $flags += @('--dir', ($root.Replace('\', '/') + '::.'), '--dir', ($directory.Replace('\', '/') + '::/out'))
    if ($Symbols) { $flags += @('--env', "VESPER_STAGE2_SYMS=$Symbols") }
    $flags += @('--argv0', 'stage2', $Source, $Package, ('/out/' + [IO.Path]::GetFileName($full)), $Entry)
    & $Binary @flags
    if ($LASTEXITCODE) { Stop-Stage2 "$Binary failed" $LASTEXITCODE }
    $diagnostic = [IO.FileInfo]::new($full + '.err.txt')
    if ($diagnostic.Exists -and $diagnostic.Length) {
        [Console]::Error.WriteLine([IO.File]::ReadAllText($diagnostic.FullName))
        Stop-Stage2 "Stage 2 reported a diagnostic at $($diagnostic.FullName)"
    }
    $component = [IO.FileInfo]::new($full)
    if (!$component.Exists -or !$component.Length) {
        if ([IO.File]::Exists($full + '.stage')) {
            [Console]::Error.WriteLine('stage log:')
            [Console]::Error.WriteLine([IO.File]::ReadAllText($full + '.stage'))
        }
        Stop-Stage2 "stage2 produced no output component at $full"
    }
}

if (!$args.Count) { Stop-Stage2 $usage 2 }
$mode = $args[0]
$values = @($args | Select-Object -Skip 1)
$precompiled = $false
if ($mode -in 'run', 'verify', 'fixed-point') {
    $option = if ($mode -eq 'run') { '--precompiled' } else { '--precompile' }
    for ($index = 0; $index -lt $values.Count; $index++) {
        $arg = $values[$index]
        if ($arg -eq $option) { $precompiled = $true }
        elseif ($mode -eq 'run' -and $arg -eq '--') { $index++; break }
        elseif ($arg.StartsWith('-')) { Stop-Stage2 "unknown option: $arg" 2 }
        else { break }
    }
    $values = @($values | Select-Object -Skip $index)
}
if (($mode -eq 'which' -and $values.Count -eq 0) -or
    ($mode -eq 'precompile' -and $values.Count -in 1, 2) -or
    ($mode -eq 'run' -and $values.Count -in 3, 4) -or
    ($mode -in 'verify', 'fixed-point' -and $values.Count -in 1, 2)) {
    $binary = Get-Wasmtime
}
else { Stop-Stage2 $usage 2 }

switch ($mode) {
    'which' {
        [void](Get-WasmtimeVersion $binary)
        $binary
    }
    'precompile' {
        [void](Get-WasmtimeVersion $binary)
        $output = if ($values.Count -eq 2) { $values[1] } else { ($values[0] -creplace '\.wasm$', '') + '.cwasm' }
        Compile-Stage2 $binary $values[0] $output
        $output
    }
    'run' {
        $entry = if ($values.Count -eq 4 -and $values[3]) { $values[3] } else { 'main' }
        Invoke-Stage2 $binary $values[0] $values[1] $values[2] $entry $precompiled $env:VESPER_STAGE2_SYMS
    }
    default {
        $original = $values[0]
        $work = if ($values.Count -eq 2) { [IO.Path]::GetFullPath($values[1], $cwd) } else { Join-Path $root 'target/bootstrap-stage2' }
        $reported = Get-WasmtimeVersion $binary
        "== Using $reported ($binary)"
        $source = $original
        if ($precompiled) {
            $source = Join-Path $work 'stage2.cwasm'
            Compile-Stage2 $binary $original $source
        }
        $stage3 = Join-Path $work 'stage3.wasm'
        $symbols = if ($env:VESPER_STAGE2_SYMS) { $env:VESPER_STAGE2_SYMS } else { '1' }
        Invoke-Stage2 $binary $source 'packages/vesper-stage2' $stage3 'main' $precompiled $symbols
        $expected = [IO.File]::ReadAllBytes([IO.Path]::GetFullPath($original, $cwd))
        $actual = [IO.File]::ReadAllBytes($stage3)
        if (![Linq.Enumerable]::SequenceEqual[byte]($expected, $actual)) {
            Stop-Stage2 "stage2 fixed-point mismatch: $original differs from $stage3"
        }
        "== Stage 2 fixed-point verified under wasmtime: stage2 == stage3 ($($actual.Length) bytes)"
    }
}
