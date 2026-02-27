package ai.zeroclaw.android.receiver

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import ai.zeroclaw.android.service.ZeroClawService

/**
 * Receives boot completed broadcast to auto-start ZeroClaw.
 * 
 * Requires user opt-in via settings.
 */
class BootReceiver : BroadcastReceiver() {
    
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action == Intent.ACTION_BOOT_COMPLETED ||
            intent.action == "android.intent.action.QUICKBOOT_POWERON") {
            
            // TODO: Check if auto-start is enabled in preferences
            // val prefs = context.getSharedPreferences("zeroclaw", Context.MODE_PRIVATE)
            // if (!prefs.getBoolean("auto_start", false)) return
            
            val serviceIntent = Intent(context, ZeroClawService::class.java).apply {
                action = ZeroClawService.ACTION_START
            }
            context.startForegroundService(serviceIntent)
        }
    }
}
