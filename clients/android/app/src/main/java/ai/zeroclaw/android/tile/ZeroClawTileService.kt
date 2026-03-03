package ai.zeroclaw.android.tile

import android.app.PendingIntent
import android.content.Intent
import android.os.Build
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService
import ai.zeroclaw.android.MainActivity
import ai.zeroclaw.android.service.ZeroClawService

/**
 * Quick Settings tile for ZeroClaw.
 *
 * Allows users to:
 * - See agent status at a glance
 * - Toggle agent on/off from notification shade
 * - Quick access to the app
 */
class ZeroClawTileService : TileService() {

    override fun onStartListening() {
        super.onStartListening()
        updateTile()
    }

    override fun onClick() {
        super.onClick()

        val tile = qsTile ?: return

        when (tile.state) {
            Tile.STATE_ACTIVE -> {
                // Stop the agent
                stopAgent()
                tile.state = Tile.STATE_INACTIVE
                tile.subtitle = "Stopped"
            }
            Tile.STATE_INACTIVE -> {
                // Start the agent
                startAgent()
                tile.state = Tile.STATE_ACTIVE
                tile.subtitle = "Running"
            }
            else -> {
                // Open app for configuration
                openApp()
            }
        }

        tile.updateTile()
    }

    override fun onTileAdded() {
        super.onTileAdded()
        updateTile()
    }

    private fun updateTile() {
        val tile = qsTile ?: return

        // TODO: Check actual agent status from bridge
        // val isRunning = ZeroClawBridge.isRunning()
        val isRunning = isServiceRunning()

        tile.state = if (isRunning) Tile.STATE_ACTIVE else Tile.STATE_INACTIVE
        tile.label = "ZeroClaw"
        tile.subtitle = if (isRunning) "Running" else "Stopped"

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            tile.subtitle = if (isRunning) "Running" else "Tap to start"
        }

        tile.updateTile()
    }

    private fun startAgent() {
        val intent = Intent(this, ZeroClawService::class.java).apply {
            action = ZeroClawService.ACTION_START
        }

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            startForegroundService(intent)
        } else {
            startService(intent)
        }
    }

    private fun stopAgent() {
        val intent = Intent(this, ZeroClawService::class.java).apply {
            action = ZeroClawService.ACTION_STOP
        }
        startService(intent)
    }

    private fun openApp() {
        val intent = Intent(this, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_NEW_TASK
        }

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            // API 34+ requires PendingIntent overload
            val pendingIntent = PendingIntent.getActivity(
                this,
                0,
                intent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
            )
            startActivityAndCollapse(pendingIntent)
        } else {
            @Suppress("DEPRECATION")
            startActivityAndCollapse(intent)
        }
    }

    private fun isServiceRunning(): Boolean {
        // Simple check - in production would check actual service state
        // TODO: Implement proper service state checking
        return false
    }
}
