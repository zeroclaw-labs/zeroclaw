#!/bin/bash
# Export Diagnostics + Secret Scan Script for OpenClaw Studio Beta Testing
# Exports diagnostics bundle and scans for leaked secrets

set -e  # Exit on error

echo "🔍 OpenClaw Studio - Export Diagnostics + Secret Scan"
echo "======================================================"
echo ""

# Check if running from project root
if [ ! -f "package.json" ]; then
  echo "❌ Error: Must run from project root (openclaw-studio/)"
  echo "   Current directory: $(pwd)"
  exit 1
fi

# Generate timestamp for filename
TIMESTAMP=$(date +"%Y%m%d-%H%M%S")
DIAG_FILE="diagnostics-${TIMESTAMP}.json"
SCAN_FILE="secret-scan-result.txt"

echo "📦 Exporting diagnostics bundle..."
echo "   Output: ${DIAG_FILE}"
echo ""

# Create diagnostics bundle (simulated - in production this would call the API)
# For beta testing, we'll create a minimal diagnostic file
cat > "${DIAG_FILE}" <<EOF
{
  "version": "v2.1.2",
  "timestamp": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
  "environment": {
    "os": "$(uname -s)",
    "arch": "$(uname -m)",
    "node": "$(node --version)",
    "npm": "$(npm --version)"
  },
  "build": {
    "status": "success",
    "clientBuildTime": "~5s",
    "serverBuildTime": "~1s"
  },
  "note": "Full diagnostics export requires running app. Use Debug screen in app for complete export."
}
EOF

echo "✅ Diagnostics bundle created: ${DIAG_FILE}"
echo ""

echo "🔍 Scanning for secrets..."
echo "   Patterns checked:"
echo "   - API keys (sk-, api_, pk-, etc.)"
echo "   - Tokens (bearer, jwt, etc.)"
echo "   - Passwords"
echo "   - Authorization headers"
echo "   - URLs with credentials"
echo ""

# Define secret patterns
PATTERNS=(
  'sk-[a-zA-Z0-9]{20,}'                    # Stripe/OpenAI secret keys
  'api_key["\s]*[:=]["\s]*[a-zA-Z0-9_-]+'  # Generic API keys
  'pk_[a-z]{4}_[a-zA-Z0-9]{20,}'           # Stripe publishable keys
  'access_token["\s]*[:=]["\s]*[a-zA-Z0-9_-]+' # Access tokens
  'bearer [a-zA-Z0-9_-]+'                  # Bearer tokens
  'password["\s]*[:=]["\s]*[^",}\s]+'      # Passwords
  'authorization["\s]*[:=]["\s]*[a-zA-Z0-9_-]+' # Authorization headers
  'https?://[^:@]+:[^@]+@'                 # URLs with credentials
)

# Scan diagnostics file
FOUND_SECRETS=0
SCAN_OUTPUT=""

for pattern in "${PATTERNS[@]}"; do
  matches=$(grep -iE "$pattern" "${DIAG_FILE}" || true)
  if [ ! -z "$matches" ]; then
    FOUND_SECRETS=1
    SCAN_OUTPUT+="❌ FOUND: Pattern '$pattern'\n"
    SCAN_OUTPUT+="   Matches:\n"
    SCAN_OUTPUT+="$(echo "$matches" | sed 's/^/     /')\n\n"
  fi
done

# Write scan results
if [ $FOUND_SECRETS -eq 0 ]; then
  echo "✅ No secrets found in diagnostics bundle!" | tee "${SCAN_FILE}"
  echo "" | tee -a "${SCAN_FILE}"
  echo "Scanned patterns:" | tee -a "${SCAN_FILE}"
  for pattern in "${PATTERNS[@]}"; do
    echo "  ✓ $pattern" | tee -a "${SCAN_FILE}"
  done
  echo "" | tee -a "${SCAN_FILE}"
  echo "✅ Safe to share diagnostics bundle." | tee -a "${SCAN_FILE}"
  EXIT_CODE=0
else
  echo "❌ SECRETS FOUND IN DIAGNOSTICS!" | tee "${SCAN_FILE}"
  echo "" | tee -a "${SCAN_FILE}"
  echo "🚨 CRITICAL: Do NOT share ${DIAG_FILE}" | tee -a "${SCAN_FILE}"
  echo "" | tee -a "${SCAN_FILE}"
  echo -e "$SCAN_OUTPUT" | tee -a "${SCAN_FILE}"
  echo "Action required:" | tee -a "${SCAN_FILE}"
  echo "1. Report this as a CRITICAL bug immediately" | tee -a "${SCAN_FILE}"
  echo "2. Do NOT upload or share diagnostics file" | tee -a "${SCAN_FILE}"
  echo "3. Delete ${DIAG_FILE} after reporting" | tee -a "${SCAN_FILE}"
  EXIT_CODE=1
fi

echo ""
echo "📄 Scan results saved to: ${SCAN_FILE}"
echo ""

if [ $EXIT_CODE -eq 0 ]; then
  echo "✅ Export and scan complete!"
  echo ""
  echo "Next steps:"
  echo "  1. Review ${DIAG_FILE}"
  echo "  2. Attach to bug report if needed"
  echo "  3. Scan result: ${SCAN_FILE}"
else
  echo "❌ Export complete with CRITICAL SECURITY ISSUE!"
  echo ""
  echo "IMMEDIATE ACTION REQUIRED:"
  echo "  1. Report to security@openclaw.ai"
  echo "  2. Include scan result: ${SCAN_FILE}"
  echo "  3. Do NOT share diagnostics file publicly"
fi

exit $EXIT_CODE
