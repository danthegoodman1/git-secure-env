# Installs gse on Windows from GitHub releases.
#
#   irm https://github.com/danthegoodman1/git-secure-env/releases/latest/download/install.ps1 | iex
#
# GSE_VERSION picks a release tag (default: the latest). GSE_INSTALL_DIR picks
# the directory (default: %LOCALAPPDATA%\gse\bin), which joins your user PATH.
$ErrorActionPreference = 'Stop'

$repo = 'danthegoodman1/git-secure-env'
$version = $env:GSE_VERSION
if (-not $version) {
    $version = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest").tag_name
}

$name = "gse-$version-x86_64-pc-windows-msvc"
$url = "https://github.com/$repo/releases/download/$version/$name.zip"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $tmp | Out-Null

try {
    Invoke-WebRequest -Uri $url -OutFile "$tmp\$name.zip"
    Invoke-WebRequest -Uri "$url.sha256" -OutFile "$tmp\$name.zip.sha256"
    $expected = ((Get-Content "$tmp\$name.zip.sha256" -Raw) -split '\s+')[0].ToLower()
    $actual = (Get-FileHash "$tmp\$name.zip" -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $actual) { throw "gse: checksum mismatch for $name.zip" }
    Expand-Archive -Path "$tmp\$name.zip" -DestinationPath $tmp

    $dir = $env:GSE_INSTALL_DIR
    if (-not $dir) { $dir = Join-Path $env:LOCALAPPDATA 'gse\bin' }
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    Copy-Item "$tmp\$name\gse.exe" $dir -Force
    Write-Host "gse: installed $version to $dir\gse.exe"

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not (($userPath -split ';') -contains $dir)) {
        [Environment]::SetEnvironmentVariable('Path', "$userPath;$dir", 'User')
        Write-Host "gse: added $dir to your user PATH; open a new terminal to use it"
    }
    Write-Host "gse: next, run 'gse install' and 'gse keygen'"
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
