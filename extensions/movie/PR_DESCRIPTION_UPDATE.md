## Android Phase 3 - Agent Integration

This PR implements the Android client for ZeroClaw with full agent integration, including foreground service, Quick Settings tile, boot receiver, and background heartbeat support.

### Changes
- `ZeroClawApp.kt` - Application setup with notification channels and WorkManager
- `SettingsRepository.kt` - DataStore + EncryptedSharedPreferences for secure settings
- `SettingsScreen.kt` - Compose UI for configuring the agent
- `BootReceiver.kt` - Auto-start on boot when enabled
- `HeartbeatWorker.kt` - Background periodic tasks via WorkManager
- `ZeroClawTileService.kt` - Quick Settings tile for agent control
- `ShareHandler.kt` - Handle content shared from other apps
- `ci-android.yml` - GitHub Actions workflow for Android builds
- `proguard-rules.pro` - R8 optimization rules

---

## Validation Evidence

- [x] All HIGH and MEDIUM CodeRabbit issues addressed
- [x] DataStore IOException handling added to prevent crashes on corrupted preferences
- [x] BootReceiver double `pendingResult.finish()` call removed
- [x] `text/uri-list` MIME type routed correctly in ShareHandler
- [x] API 34+ PendingIntent overload added to TileService
- [x] Kotlin Intrinsics null checks preserved in ProGuard rules
- [x] HeartbeatWorker enforces 15-minute minimum and uses UPDATE policy
- [x] SettingsScreen refreshes battery optimization state on resume
- [x] ZeroClawApp listens for settings changes to update heartbeat schedule
- [x] Trailing whitespace removed from all Kotlin files
- [ ] Manual testing: Build and install on Android 14 device (pending)

## Security Impact

- **API Keys**: Stored in Android Keystore via EncryptedSharedPreferences (AES-256-GCM)
- **Permissions**: RECEIVE_BOOT_COMPLETED, FOREGROUND_SERVICE, POST_NOTIFICATIONS
- **Data in Transit**: All API calls use HTTPS
- **No New Vulnerabilities**: No raw SQL, no WebView JavaScript, no exported components without protection

## Privacy and Data Hygiene

- **Local Storage Only**: All settings stored on-device, nothing transmitted except to configured AI provider
- **No Analytics**: No third-party analytics or tracking SDKs
- **User Control**: API key can be cleared via settings, auto-start is opt-in
- **Minimal Permissions**: Only requests permissions necessary for core functionality

## Rollback Plan

1. **Feature Flag**: Not yet implemented; can be added if needed
2. **Version Pinning**: Users can stay on previous APK version
3. **Clean Uninstall**: All data stored in app's private directory, removed on uninstall
4. **Server-Side**: No backend changes required; rollback is client-only
