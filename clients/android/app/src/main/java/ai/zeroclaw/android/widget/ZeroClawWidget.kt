package ai.zeroclaw.android.widget

import android.app.PendingIntent
import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.Context
import android.content.Intent
import android.widget.RemoteViews
import ai.zeroclaw.android.MainActivity
import ai.zeroclaw.android.R
import ai.zeroclaw.android.service.ZeroClawService

/**
 * Home screen widget for ZeroClaw.
 *
 * Features:
 * - Shows agent status (running/stopped)
 * - Quick action button to toggle or send message
 * - Tap to open app
 *
 * Widget sizes:
 * - Small (2x1): Status + toggle button
 * - Medium (4x1): Status + quick message
 * - Large (4x2): Status + recent message + input
 */
class ZeroClawWidget : AppWidgetProvider() {

    override fun onUpdate(
        context: Context,
        appWidgetManager: AppWidgetManager,
        appWidgetIds: IntArray
    ) {
        for (appWidgetId in appWidgetIds) {
            updateAppWidget(context, appWidgetManager, appWidgetId)
        }
    }

    override fun onEnabled(context: Context) {
        // First widget placed
    }

    override fun onDisabled(context: Context) {
        // Last widget removed
    }

    override fun onReceive(context: Context, intent: Intent) {
        super.onReceive(context, intent)

        when (intent.action) {
            ACTION_TOGGLE -> {
                toggleAgent(context)
            }
            ACTION_QUICK_MESSAGE -> {
                openAppWithMessage(context, intent.getStringExtra(EXTRA_MESSAGE))
            }
        }
    }

    private fun toggleAgent(context: Context) {
        // TODO: Check actual status and toggle
        val serviceIntent = Intent(context, ZeroClawService::class.java).apply {
            action = ZeroClawService.ACTION_START
        }
        context.startForegroundService(serviceIntent)
    }

    private fun openAppWithMessage(context: Context, message: String?) {
        val intent = Intent(context, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
            message?.let { putExtra(EXTRA_MESSAGE, it) }
        }
        context.startActivity(intent)
    }

    companion object {
        const val ACTION_TOGGLE = "ai.zeroclaw.widget.TOGGLE"
        const val ACTION_QUICK_MESSAGE = "ai.zeroclaw.widget.QUICK_MESSAGE"
        const val EXTRA_MESSAGE = "message"

        internal fun updateAppWidget(
            context: Context,
            appWidgetManager: AppWidgetManager,
            appWidgetId: Int
        ) {
            // Create RemoteViews
            val views = RemoteViews(context.packageName, R.layout.widget_zeroclaw)

            // Set status text
            // TODO: Get actual status from bridge
            val isRunning = false
            views.setTextViewText(
                R.id.widget_status,
                if (isRunning) "🟢 Running" else "⚪ Stopped"
            )

            // Open app on tap
            val openIntent = Intent(context, MainActivity::class.java)
            val openPendingIntent = PendingIntent.getActivity(
                context, 0, openIntent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
            )
            views.setOnClickPendingIntent(R.id.widget_container, openPendingIntent)

            // Toggle button
            val toggleIntent = Intent(context, ZeroClawWidget::class.java).apply {
                action = ACTION_TOGGLE
            }
            val togglePendingIntent = PendingIntent.getBroadcast(
                context, 1, toggleIntent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
            )
            views.setOnClickPendingIntent(R.id.widget_toggle_button, togglePendingIntent)

            // Update widget
            appWidgetManager.updateAppWidget(appWidgetId, views)
        }

        /**
         * Request widget update from anywhere in the app
         */
        fun requestUpdate(context: Context) {
            val intent = Intent(context, ZeroClawWidget::class.java).apply {
                action = AppWidgetManager.ACTION_APPWIDGET_UPDATE
            }
            context.sendBroadcast(intent)
        }
    }
}
