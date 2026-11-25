[CmdletBinding()]
param(
    [switch]$PrepareOnly
)

$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $repo
try {
    $cargoJson = & cargo metadata --format-version 1 --no-deps --locked
    if ($LASTEXITCODE -ne 0) {
        throw 'Could not read the configured Cargo target directory.'
    }
    $target = ($cargoJson | ConvertFrom-Json).target_directory
} finally {
    Pop-Location
}

$server = Join-Path $target 'release\rs-gopls.exe'
$wasm = Join-Path $target 'wasm32-wasip2\release\rs_gopls_zed.wasm'
$manifest = Join-Path $repo 'zed-extension\extension.toml'
foreach ($path in @($server, $wasm, $manifest)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Required file is missing: $path. See the Zed build commands in README.md."
    }
}

$testRoot = Join-Path $target 'zed-rs-gopls'
$profile = Join-Path $testRoot 'profile'
$profileConfig = Join-Path $profile 'config'
$installedExtension = Join-Path $profile 'extensions\installed\rs-gopls-lsp'
$extensionWork = Join-Path $profile 'extensions\work\rs-gopls-lsp'
$demo = Join-Path $testRoot 'demo'
$demoSettings = Join-Path $demo '.zed'
$greeting = Join-Path $demo 'greeting'
New-Item -ItemType Directory -Path $installedExtension, $extensionWork, $profileConfig, $demoSettings, $greeting -Force | Out-Null

function Copy-BuiltFile($source, $destination) {
    if ((Test-Path -LiteralPath $destination -PathType Leaf) -and
        (Get-FileHash -LiteralPath $source).Hash -eq (Get-FileHash -LiteralPath $destination).Hash) {
        return $false
    }
    Copy-Item -LiteralPath $source -Destination $destination -Force
    return $true
}

$manifestChanged = Copy-BuiltFile $manifest (Join-Path $installedExtension 'extension.toml')
$wasmChanged = Copy-BuiltFile $wasm (Join-Path $installedExtension 'extension.wasm')
if ($manifestChanged -or $wasmChanged) {
    # Zed compares its index timestamp with the installed directory's timestamp.
    [IO.Directory]::SetLastWriteTimeUtc((Split-Path -Parent $installedExtension), [DateTime]::UtcNow)
}
[void](Copy-BuiltFile $server (Join-Path $extensionWork 'rs-gopls.exe'))

$profileSettings = Join-Path $profileConfig 'settings.json'
if (-not (Test-Path -LiteralPath $profileSettings)) {
    $profileSettingsJson = @{
        languages = @{
            Go = @{ language_servers = @('rs-gopls', '!gopls') }
            'Go Mod' = @{ enable_language_server = $false }
            'Go Work' = @{ enable_language_server = $false }
        }
    } | ConvertTo-Json -Depth 6
    [IO.File]::WriteAllText($profileSettings, $profileSettingsJson + [Environment]::NewLine)
}

$settings = Join-Path $demoSettings 'settings.json'
if (-not (Test-Path -LiteralPath $settings)) {
    $settingsJson = @{
        lsp = @{
            'rs-gopls' = @{
                binary = @{
                    env = @{ GO111MODULE = 'on'; GOWORK = 'off' }
                }
            }
        }
    } | ConvertTo-Json -Depth 8
    [IO.File]::WriteAllText($settings, $settingsJson + [Environment]::NewLine)
}

$goMod = Join-Path $demo 'go.mod'
if (-not (Test-Path -LiteralPath $goMod)) {
    [IO.File]::WriteAllText($goMod, @'
module example.com/rs-gopls-demo

go 1.22
'@ + [Environment]::NewLine)
}

$caller = Join-Path $demo 'main.go'
if (-not (Test-Path -LiteralPath $caller)) {
    [IO.File]::WriteAllText($caller, @'
package main

import "example.com/rs-gopls-demo/greeting"

func main() {
    greeting.Hello()
}
'@ + [Environment]::NewLine)
}

$definition = Join-Path $greeting 'greeting.go'
if (-not (Test-Path -LiteralPath $definition)) {
    [IO.File]::WriteAllText($definition, @'
package greeting

func Hello() {}
'@ + [Environment]::NewLine)
}

Write-Host "Zed profile: $profile"
Write-Host "Go demo: $demo"
if ($PrepareOnly) {
    return
}

$zed = Get-Command zed.exe -CommandType Application | Select-Object -First 1
Write-Host 'Open main.go from the demo project, then run Go to Definition on Hello.'
& $zed.Source --user-data-dir $profile -n $demo
if ($LASTEXITCODE -ne 0) {
    throw "Zed exited with code $LASTEXITCODE."
}

