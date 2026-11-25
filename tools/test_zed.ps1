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
$zedProfile = Join-Path $testRoot 'profile'
$profileConfig = Join-Path $zedProfile 'config'
$installedExtension = Join-Path $zedProfile 'extensions\installed\rs-gopls-lsp'
$extensionWork = Join-Path $zedProfile 'extensions\work\rs-gopls-lsp'
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
# Use a versioned path because Windows locks the binary while Zed runs it.
$serverVersion = (Get-FileHash -LiteralPath $server).Hash.Substring(0, 16).ToLowerInvariant()
$stagedServer = Join-Path $extensionWork "rs-gopls-$serverVersion.exe"
[void](Copy-BuiltFile $server $stagedServer)

function Get-SettingsObject($parent, $name) {
    if ($null -eq $parent.$name) {
        $parent | Add-Member -NotePropertyName $name -NotePropertyValue ([pscustomobject]@{}) -Force
    }
    return $parent.$name
}

$profileSettings = Join-Path $profileConfig 'settings.json'
if (Test-Path -LiteralPath $profileSettings) {
    $configuration = Get-Content -LiteralPath $profileSettings -Raw | ConvertFrom-Json
} else {
    $configuration = [pscustomobject]@{
        languages = @{
            Go = @{ language_servers = @('rs-gopls', '!gopls') }
            'Go Mod' = @{ enable_language_server = $false }
            'Go Work' = @{ enable_language_server = $false }
        }
    }
}
$servers = Get-SettingsObject $configuration 'lsp'
$serverOptions = Get-SettingsObject $servers 'rs-gopls'
$binaryOptions = Get-SettingsObject $serverOptions 'binary'
$configuredPath = [string]$binaryOptions.path
$managedPath = $configuredPath -eq '' -or (
    [IO.Path]::GetDirectoryName($configuredPath) -eq $extensionWork -and
    [IO.Path]::GetFileName($configuredPath) -match '^rs-gopls(?:-[0-9a-f]{16})?\.exe$'
)
if ($managedPath -and $configuredPath -ne $stagedServer) {
    $binaryOptions | Add-Member -NotePropertyName 'path' -NotePropertyValue $stagedServer -Force
    $profileSettingsJson = $configuration | ConvertTo-Json -Depth 32
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

Write-Host "Zed profile: $zedProfile"
Write-Host "Go demo: $demo"
if ($PrepareOnly) {
    return
}

$zed = Get-Command zed.exe -CommandType Application | Select-Object -First 1
Write-Host 'Open main.go from the demo project, then run Go to Definition on Hello.'
& $zed.Source --user-data-dir $zedProfile -n $demo
if ($LASTEXITCODE -ne 0) {
    throw "Zed exited with code $LASTEXITCODE."
}
