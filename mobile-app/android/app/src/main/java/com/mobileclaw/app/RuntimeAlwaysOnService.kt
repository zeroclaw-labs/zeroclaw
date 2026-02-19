package com.mobileclaw.app

import android.app.Notification
import android.app.Service
import android.content.Intent
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import androidx.core.app.NotificationCompat

class RuntimeAlwaysOnService : Service() {
    private val handler = Handler(Looper.getMainLooper())
    private val ticker = object : Runnable {
        override fun run() {
            RuntimeBridge.scheduleImmediate(applicationContext)
            handler.postDelayed(this, 20_000L)
        }
    }

    override fun onCreate() {
        super.onCreate()
        RuntimeBridge.ensureNotificationChannel(applicationContext)
        startForeground(42103, buildNotification())
        handler.post(ticker)
    }

    override fun onDestroy() {
        handler.removeCallbacks(ticker)
        super.onDestroy()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        RuntimeBridge.scheduleImmediate(applicationContext)
        return START_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun buildNotification(): Notification {
        return NotificationCompat.Builder(this, RuntimeBridge.CHANNEL_ID)
            .setContentTitle("MobileClaw runtime active")
            .setContentText("Keeping hooks and Telegram relay active")
            .setSmallIcon(R.mipmap.ic_launcher)
            .setOngoing(true)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .build()
    }
}
