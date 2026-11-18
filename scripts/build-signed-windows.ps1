# The installer certificate and updater key are intentionally separate.
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$desktop = Join-Path $root "apps/desktop"
$target = "x86_64-pc-windows-msvc"
$required = @(
    "FRIDAY_UPDATE_ENDPOINT",
    "FRIDAY_UPDATER_PUBLIC_KEY",
    "FRIDAY_RELEASE_ASSET_BASE",
    "FRIDAY_SIGNING_PFX",
    "FRIDAY_SIGNING_PFX_PASSWORD",
    "TAURI_SIGNING_PRIVATE_KEY"
)
foreach ($name in $required) {
    if ([string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($name))) {
        throw "Missing release input $name"
    }
}
if (([uri]$env:FRIDAY_UPDATE_ENDPOINT).Scheme -ne "https" -or
    ([uri]$env:FRIDAY_RELEASE_ASSET_BASE).Scheme -ne "https") {
    throw "Release URLs must use HTTPS"
}

$stage = Join-Path ([IO.Path]::GetTempPath()) ("friday-release-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $stage | Out-Null
$pfx = Join-Path $stage "certificate.pfx"
$override = Join-Path $stage "release.conf.json"
$certificate = $null
try {
    $existingCertificates = @(Get-ChildItem Cert:\CurrentUser\My | Select-Object -ExpandProperty Thumbprint)
    [IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:FRIDAY_SIGNING_PFX))
    $password = ConvertTo-SecureString $env:FRIDAY_SIGNING_PFX_PASSWORD -AsPlainText -Force
    $certificate = Import-PfxCertificate -FilePath $pfx -CertStoreLocation Cert:\CurrentUser\My -Password $password |
        Where-Object { $_.HasPrivateKey } | Select-Object -First 1
    if (-not $certificate) { throw "The release certificate has no private key" }

    @{
        bundle = @{
            createUpdaterArtifacts = $true
            windows = @{
                certificateThumbprint = $certificate.Thumbprint
                digestAlgorithm = "sha256"
                timestampUrl = "http://timestamp.digicert.com"
                tsp = $true
            }
        }
    } | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8 $override

    Push-Location $desktop
    try {
        npm run tauri -- build --ci --target $target --bundles nsis --config $override -- --locked
        if ($LASTEXITCODE -ne 0) { throw "Tauri release build failed" }
    } finally { Pop-Location }

    $packaged = Join-Path $root "target/$target/release/bundle/nsis"
    $installers = @(Get-ChildItem -File $packaged -Filter "*-setup.exe")
    if ($installers.Count -ne 1) { throw "Expected one NSIS installer" }
    $installer = $installers[0]
    $sevenZip = Get-Command 7z.exe -CommandType Application -ErrorAction SilentlyContinue
    $extractor = if ($sevenZip) { $sevenZip.Source } else { Join-Path $env:ProgramFiles "7-Zip/7z.exe" }
    if (-not (Test-Path -LiteralPath $extractor -PathType Leaf)) {
        throw "7-Zip is required to verify the packaged executable"
    }
    # Tauri restores the unsigned source binary after bundling.
    $payload = Join-Path $stage "payload"
    & $extractor x -y "-o$payload" $installer.FullName "friday-desktop.exe" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Could not inspect the NSIS installer" }
    $app = Join-Path $payload "friday-desktop.exe"
    foreach ($path in @($app, $installer.FullName)) {
        $signature = Get-AuthenticodeSignature -LiteralPath $path
        if ($signature.Status -ne "Valid" -or $signature.SignerCertificate.Thumbprint -ne $certificate.Thumbprint) {
            throw "Invalid Authenticode signature"
        }
    }
    $signatureFile = $installer.FullName + ".sig"
    if (-not (Test-Path $signatureFile)) { throw "Updater signature is missing" }

    $output = Join-Path $root "release-windows"
    New-Item -ItemType Directory -Force $output | Out-Null
    Copy-Item $installer.FullName, $signatureFile $output
    $version = (Get-Content (Join-Path $desktop "src-tauri/tauri.conf.json") -Raw | ConvertFrom-Json).version
    $asset = $env:FRIDAY_RELEASE_ASSET_BASE.TrimEnd("/") + "/" + [uri]::EscapeDataString($installer.Name)
    @{
        version = $version
        pub_date = [DateTimeOffset]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
        platforms = @{
            "windows-x86_64" = @{
                signature = (Get-Content $signatureFile -Raw).Trim()
                url = $asset
            }
        }
    } | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8 (Join-Path $output "latest.json")
    (Get-FileHash $installer.FullName -Algorithm SHA256).Hash.ToLowerInvariant() + "  " + $installer.Name |
        Set-Content -Encoding ascii (Join-Path $output "SHA256SUMS")
} finally {
    if ($certificate -and $certificate.Thumbprint -notin $existingCertificates) {
        Remove-Item -LiteralPath "Cert:\CurrentUser\My\$($certificate.Thumbprint)" -ErrorAction SilentlyContinue
    }
    Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
}
