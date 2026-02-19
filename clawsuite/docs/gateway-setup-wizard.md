# Gateway Setup Wizard

## Overview

The Gateway Setup Wizard is a first-run onboarding flow that helps users configure their OpenClaw Gateway connection when ClawSuite is launched for the first time.

## Features

### 1. **First-Run Detection**
- Automatically checks if the gateway is configured and reachable on app load
- Shows the setup wizard if gateway is not reachable or not configured
- Stores completion state in `localStorage` (key: `clawsuite-gateway-configured`)

### 2. **Auto-Detection**
- Attempts to detect a local gateway running at `localhost:18789`
- Pre-fills the Gateway URL field if detected
- Shows a friendly "Local gateway detected!" message

### 3. **Gateway Configuration Step**
- Input fields for:
  - **Gateway URL** (e.g., `http://localhost:18789`)
  - **Gateway Token** (optional, for authenticated gateways)
- **Test Connection** button to validate before saving
- Clear success/error feedback
- Only allows proceeding after a successful connection test

### 4. **Provider Setup Guidance**
- After gateway connection is established, guides users to set up at least one AI provider
- Provides clear CLI instructions:
  - `openclaw providers list`
  - `openclaw providers add`
- Users can skip this step if they want to configure providers later

### 5. **Reconnection Banner**
- If the gateway becomes unreachable after initial setup, a dismissible banner appears at the top of the app
- Banner shows only when:
  - Gateway was previously configured
  - Current health check fails
  - User hasn't dismissed it in the current session
- Users can:
  - Dismiss the banner (persists for the session)
  - Click "Reconfigure" to re-open the setup wizard

## Files

### New Files
- `src/hooks/use-gateway-setup.ts` - Zustand store for wizard state management
- `src/components/gateway-setup-wizard.tsx` - Main wizard component
- `src/components/gateway-reconnect-banner.tsx` - Reconnection banner component

### Modified Files
- `src/routes/__root.tsx` - Added wizard and banner to root layout

## Implementation Details

### State Management
The wizard uses Zustand for state management with the following key features:
- Auto-initialization on app mount
- Gateway health checking via `/api/ping`
- Local gateway detection via direct health endpoint fetch
- localStorage persistence for completion state

### Health Checking
Gateway health is checked via:
1. **App-level check**: `GET /api/ping` (proxied through ClawSuite server)
2. **Direct check**: `GET http://localhost:18789/health` (for auto-detection)

Both use AbortSignal.timeout for fail-fast behavior (3-5 seconds).

### User Flow
1. User opens ClawSuite for the first time
2. App checks if gateway is configured (`localStorage` key exists)
3. If not configured:
   - Check if gateway is reachable via `/api/ping`
   - If not reachable, check for local gateway at `localhost:18789`
   - Show wizard with appropriate messaging
4. User enters gateway URL and token (optional)
5. User clicks "Test Connection"
6. On success, proceed to provider setup step
7. User can skip or complete provider setup
8. Wizard closes and sets completion flag in localStorage

### Reconnection Flow
1. Periodic health checks run every 30 seconds (only when user hasn't dismissed)
2. If gateway becomes unreachable, show banner
3. User can dismiss (session-only persistence) or reconfigure
4. Reconfigure re-opens the setup wizard

## Future Enhancements

- [ ] Actually save gateway config to server (currently just proceeds)
- [ ] Add inline provider configuration within the wizard
- [ ] Add support for custom gateway health endpoints
- [ ] Add retry logic for transient connection failures
- [ ] Add more detailed connection diagnostics (SSL errors, timeouts, etc.)
- [ ] Add ability to test connection before entering token
- [ ] Integrate with existing settings screen for gateway config

## Testing

To test the wizard:

1. **First-run test**:
   ```bash
   # Clear completion flag
   localStorage.removeItem('clawsuite-gateway-configured')
   # Reload page
   ```

2. **Local detection test**:
   ```bash
   # Ensure gateway is running
   openclaw gateway status
   # Clear flag and reload
   ```

3. **Reconnection banner test**:
   ```bash
   # Complete wizard first
   # Stop gateway: openclaw gateway stop
   # Wait 30 seconds for health check
   # Banner should appear
   ```

## Notes

- The wizard has z-index `110`, higher than the onboarding tour (`100`) to ensure it appears on top
- The reconnection banner has z-index `50`, below modals but above content
- Gateway config is currently only stored client-side; server-side persistence needs to be implemented
- The wizard does not block interaction with the rest of the app (it's a modal overlay)
