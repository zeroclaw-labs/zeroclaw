package ai.zeroclaw.android

import android.app.Application
import android.app.NotificationChannel
import android.app.NotificationManager
import android.os.Build

class ZeroClawApp : Application() {
    
    companion object {
        const val CHANNEL_ID = "zeroclaw_service"
        const val CHANNEL_NAME = "ZeroClaw Agent"
        const val AGENT_CHANNEL_ID = "zeroclaw_agent"
        const val AGENT_CHANNEL_NAME = "Agent Messages"
    }
    
    override fun onCreate() {
        super.onCreate()
        createNotificationChannels()
        
        // TODO: Initialize native library
        // System.loadLibrary("zeroclaw")
    }
    
    private fun createNotificationChannels() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val manager = getSystemService(NotificationManager::class.java)
            
            // Service channel (foreground service)
            val serviceChannel = NotificationChannel(
                CHANNEL_ID,
                CHANNEL_NAME,
                NotificationManager.IMPORTANCE_LOW
            ).apply {
                description = "ZeroClaw background service"
                setShowBadge(false)
            }
            
            // Agent messages channel
            val agentChannel = NotificationChannel(
                AGENT_CHANNEL_ID,
                AGENT_CHANNEL_NAME,
                NotificationManager.IMPORTANCE_HIGH
            ).apply {
                description = "Messages from your AI agent"
                enableVibration(true)
            }
            
            manager.createNotificationChannel(serviceChannel)
            manager.createNotificationChannel(agentChannel)
        }
    }
}
