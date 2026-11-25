$ErrorActionPreference = 'Stop'

$root = Join-Path $PSScriptRoot '../..'
$key = $env:BUILDBUDDY_API_KEY
$lines = @(
    'common --config=ci'
    # Keep paths short and enable the accepted Visual Studio runtime license.
    'startup --output_user_root=C:/_bzl'
    'common --repo_env=BAZEL_MSVC_RUNTIME_VISUAL_STUDIO_EULA=1'
)
if ($key) {
    $lines += @(
        'common --config=buildbuddy'
        "common --remote_header=x-buildbuddy-api-key=$key"
        'common --build_metadata=ROLE=CI'
    )
}
[IO.File]::WriteAllText((Join-Path $root 'user.bazelrc'), ($lines -join "`n") + "`n")
if ($key) {
    Write-Output 'BuildBuddy: enabled (results link is printed by each Bazel command)'
} else {
    Write-Output 'BuildBuddy: no API key, using the GitHub Actions disk cache'
}
