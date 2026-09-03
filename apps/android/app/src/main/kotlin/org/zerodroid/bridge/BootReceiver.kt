package org.zerodroid.bridge

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/**
 * Restart the agent after a device reboot — but only if the user opted in (start-on-boot) and a
 * provider is configured. BOOT_COMPLETED is an allowed entry point for starting a foreground
 * service, so the agent comes back "always on your phone". Work is done in goAsync() off the main
 * thread because reading the encrypted prefs touches the keystore.
 */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(ctx: Context, intent: Intent) {
        // Only the protected BOOT_COMPLETED (system-only sender). We intentionally don't accept the
        // spoofable QUICKBOOT_POWERON.
        if (intent.action != Intent.ACTION_BOOT_COMPLETED) return
        val pending = goAsync()
        Thread {
            try {
                val cfg = ConfigStore(ctx.applicationContext)
                if (cfg.startOnBoot && cfg.isConfigured()) BridgeService.start(ctx.applicationContext)
            } catch (e: Exception) {
                android.util.Log.w("zerodroid-boot", "boot start failed: ${e.message}")
            } finally { pending.finish() }
        }.apply { isDaemon = true }.start()
    }
}
