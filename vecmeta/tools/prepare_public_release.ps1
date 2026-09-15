param([Parameter(Mandatory = $true)][string]$Destination)

$ErrorActionPreference = 'Stop'
$sourceRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$releaseRoot = [IO.Path]::GetFullPath($Destination)
if (Test-Path -LiteralPath $releaseRoot) {
    throw 'Destination already exists. Use a fresh directory; no files will be overwritten.'
}
if ($releaseRoot.StartsWith($sourceRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'The public snapshot must be outside the source directory.'
}

# Copy only reviewed source/documentation. Do not copy .git, external fixtures,
# local sample files, credentials, or build outputs from the authoring checkout.
$files = @('Cargo.toml', 'Cargo.lock', 'LICENSE', 'README.md', 'README_EN.md', 'CONTRIBUTING.md', '.gitignore')
$directories = @('crates', 'docs', '.github', 'tools')
foreach ($name in $files + $directories) {
    if (-not (Test-Path -LiteralPath (Join-Path $sourceRoot $name))) {
        throw "Required release input missing: $name"
    }
}
New-Item -ItemType Directory -Path $releaseRoot | Out-Null
foreach ($name in $files) {
    Copy-Item -LiteralPath (Join-Path $sourceRoot $name) -Destination (Join-Path $releaseRoot $name)
}
foreach ($name in $directories) {
    Copy-Item -LiteralPath (Join-Path $sourceRoot $name) -Destination (Join-Path $releaseRoot $name) -Recurse
}
Write-Output "Prepared source snapshot: $releaseRoot"
Write-Output 'Initialize Git, scan the exact staged contents, and test this snapshot before publication.'
