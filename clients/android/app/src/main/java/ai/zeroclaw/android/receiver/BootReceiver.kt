package ai.zeroclaw.android.receiver

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Build
import ai.zeroclaw.android.ZeroClawApp
import ai.zeroclaw.android.service.ZeroClawService
import ai.zeroclaw.android.worker.HeartbeatWorker
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

/**
 * Receives boot completed broadcast to auto-start ZeroClaw.
 *
 * Also handles:
 * - Package updates (MY_PACKAGE_REPLACED)
 * - Quick boot on some devices (QUICKBOOT_POWERON)
 *
 * Respects user's auto-start preference from settings.
 */
class BootReceiver : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        when (intent.action) {
            Intent.ACTION_BOOT_COMPLETED,
            "android.intent.action.QUICKBOOT_POWERON",
            Intent.ACTION_MY_PACKAGE_REPLACED -> {
                handleBoot(context)
            }
        }
    }

    private fun handleBoot(context: Context) {
        // Use goAsync() to get more time for async operations
        val pendingResult = goAsync()

        CoroutineScope(Dispatchers.IO).launch {
            try {
                val app = context.applicationContext as? ZeroClawApp
                val settingsRepo = app?.settingsRepository ?: return@launch

                val settings = settingsRepo.settings.first()

                // Only auto-start if enabled and configured
                if (settings.autoStart && settings.isConfigured()) {
                    // Start the foreground service
                    val serviceIntent = Intent(context, ZeroClawService::class.java).apply {
                        action = ZeroClawService.ACTION_START
                    }

                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                        context.startForegroundService(serviceIntent)
                    } else {
                        context.startService(serviceIntent)
                    }

                    // Schedule heartbeat worker
                    HeartbeatWorker.scheduleHeartbeat(
                        context,
                        settings.heartbeatIntervalMinutes.toLong()
                    )

                    android.util.Log.i(TAG, "ZeroClaw auto-started on boot")
                } else {
                    android.util.Log.d(TAG, "Auto-start disabled or not configured, skipping")
                }
            } catch (e: Exception) {
                android.util.Log.e(TAG, "Error during boot handling", e)
            } finally {
                pendingResult.finish()
            }
        }
    }

    companion object {
        private const val TAG = "BootReceiver"
    }
}
