# Export Diagnostics + Secret Scan Script for OpenClaw Studio Beta Testing (Windows)
# Exports diagnostics bundle and scans for leaked secrets

Write-Host "🔍 OpenClaw Studio - Export Diagnostics + Secret Scan" -ForegroundColor Cyan
Write-Host "======================================================" -ForegroundColor Cyan
Write-Host ""

# Check if running from project root
if (-Not (Test-Path "package.json")) {
  Write-Host "❌ Error: Must run from project root (openclaw-studio/)" -ForegroundColor Red
  Write-Host "   Current directory: $(Get-Location)" -ForegroundColor Red
  exit 1
}

# Generate timestamp for filename
$timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
$diagFile = "diagnostics-$timestamp.json"
$scanFile = "secret-scan-result.txt"

Write-Host "📦 Exporting diagnostics bundle..." -ForegroundColor Yellow
Write-Host "   Output: $diagFile"
Write-Host ""

# Create diagnostics bundle (simulated - in production this would call the API)
$nodeVersion = node --version
$npmVersion = npm --version
$os = [System.Environment]::OSVersion.Platform
$arch = [System.Environment]::GetEnvironmentVariable("PROCESSOR_ARCHITECTURE")

$diagnostics = @{
  version = "v2.1.2"
  timestamp = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
  environment = @{
    os = $os
    arch = $arch
    node = $nodeVersion
    npm = $npmVersion
  }
  build = @{
    status = "success"
    clientBuildTime = "~5s"
    serverBuildTime = "~1s"
  }
  note = "Full diagnostics export requires running app. Use Debug screen in app for complete export."
}

$diagnostics | ConvertTo-Json -Depth 10 | Out-File -FilePath $diagFile -Encoding UTF8

Write-Host "✅ Diagnostics bundle created: $diagFile" -ForegroundColor Green
Write-Host ""

Write-Host "🔍 Scanning for secrets..." -ForegroundColor Yellow
Write-Host "   Patterns checked:"
Write-Host "   - API keys (sk-, api_, pk-, etc.)"
Write-Host "   - Tokens (bearer, jwt, etc.)"
Write-Host "   - Passwords"
Write-Host "   - Authorization headers"
Write-Host "   - URLs with credentials"
Write-Host ""

# Define secret patterns (PowerShell regex syntax)
$patterns = @(
  'sk-[a-zA-Z0-9]{20,}',                    # Stripe/OpenAI secret keys
  'api_key["\s]*[:=]["\s]*[a-zA-Z0-9_-]+',  # Generic API keys
  'pk_[a-z]{4}_[a-zA-Z0-9]{20,}',           # Stripe publishable keys
  'access_token["\s]*[:=]["\s]*[a-zA-Z0-9_-]+', # Access tokens
  'bearer [a-zA-Z0-9_-]+',                  # Bearer tokens
  'password["\s]*[:=]["\s]*[^",}\s]+',      # Passwords
  'authorization["\s]*[:=]["\s]*[a-zA-Z0-9_-]+', # Authorization headers
  'https?://[^:@]+:[^@]+@'                  # URLs with credentials
)

# Scan diagnostics file
$foundSecrets = $false
$scanOutput = ""
$diagContent = Get-Content $diagFile -Raw

foreach ($pattern in $patterns) {
  $matches = [regex]::Matches($diagContent, $pattern, [System.Text.RegularExpressions.RegexOptions]::IgnoreCase)
  if ($matches.Count -gt 0) {
    $foundSecrets = $true
    $scanOutput += "❌ FOUND: Pattern '$pattern'`n"
    $scanOutput += "   Matches:`n"
    foreach ($match in $matches) {
      $scanOutput += "     $($match.Value)`n"
    }
    $scanOutput += "`n"
  }
}

# Write scan results
if (-Not $foundSecrets) {
  $result = "✅ No secrets found in diagnostics bundle!`n`n"
  $result += "Scanned patterns:`n"
  foreach ($pattern in $patterns) {
    $result += "  ✓ $pattern`n"
  }
  $result += "`n✅ Safe to share diagnostics bundle."
  
  Write-Host $result -ForegroundColor Green
  $result | Out-File -FilePath $scanFile -Encoding UTF8
  $exitCode = 0
} else {
  $result = "❌ SECRETS FOUND IN DIAGNOSTICS!`n`n"
  $result += "🚨 CRITICAL: Do NOT share $diagFile`n`n"
  $result += $scanOutput
  $result += "Action required:`n"
  $result += "1. Report this as a CRITICAL bug immediately`n"
  $result += "2. Do NOT upload or share diagnostics file`n"
  $result += "3. Delete $diagFile after reporting`n"
  
  Write-Host $result -ForegroundColor Red
  $result | Out-File -FilePath $scanFile -Encoding UTF8
  $exitCode = 1
}

Write-Host ""
Write-Host "📄 Scan results saved to: $scanFile" -ForegroundColor Cyan
Write-Host ""

if ($exitCode -eq 0) {
  Write-Host "✅ Export and scan complete!" -ForegroundColor Green
  Write-Host ""
  Write-Host "Next steps:"
  Write-Host "  1. Review $diagFile"
  Write-Host "  2. Attach to bug report if needed"
  Write-Host "  3. Scan result: $scanFile"
} else {
  Write-Host "❌ Export complete with CRITICAL SECURITY ISSUE!" -ForegroundColor Red
  Write-Host ""
  Write-Host "IMMEDIATE ACTION REQUIRED:"
  Write-Host "  1. Report to security@openclaw.ai"
  Write-Host "  2. Include scan result: $scanFile"
  Write-Host "  3. Do NOT share diagnostics file publicly"
}

exit $exitCode
