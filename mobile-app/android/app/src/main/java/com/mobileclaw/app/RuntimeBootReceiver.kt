package com.mobileclaw.app

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.core.content.ContextCompat

class RuntimeBootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED && intent.action != Intent.ACTION_MY_PACKAGE_REPLACED) {
            return
        }

        RuntimeBridge.scheduleImmediate(context)
        if (RuntimeBridge.isAlwaysOnEnabled(context)) {
            val serviceIntent = Intent(context, RuntimeAlwaysOnService::class.java)
            ContextCompat.startForegroundService(context, serviceIntent)
        }
    }
}
