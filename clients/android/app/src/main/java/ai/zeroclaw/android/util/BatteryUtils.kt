package ai.zeroclaw.android.util

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings

/**
 * Utilities for handling battery optimization.
 *
 * ZeroClaw needs to run reliably in the background for:
 * - Heartbeat checks
 * - Cron job execution
 * - Notification monitoring
 *
 * This helper manages battery optimization exemption requests.
 */
object BatteryUtils {

    /**
     * Check if app is exempt from battery optimization
     */
    fun isIgnoringBatteryOptimizations(context: Context): Boolean {
        val powerManager = context.getSystemService(Context.POWER_SERVICE) as PowerManager
        return powerManager.isIgnoringBatteryOptimizations(context.packageName)
    }

    /**
     * Request battery optimization exemption.
     *
     * Note: This shows a system dialog - use sparingly and explain to user first.
     * Google Play policy requires justification for this permission.
     */
    fun requestBatteryOptimizationExemption(context: Context) {
        if (isIgnoringBatteryOptimizations(context)) {
            return // Already exempt
        }

        val intent = Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS).apply {
            data = Uri.parse("package:${context.packageName}")
            flags = Intent.FLAG_ACTIVITY_NEW_TASK
        }

        try {
            context.startActivity(intent)
        } catch (e: Exception) {
            // Fallback to battery settings
            openBatterySettings(context)
        }
    }

    /**
     * Open battery optimization settings page
     */
    fun openBatterySettings(context: Context) {
        val intent = Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS).apply {
            flags = Intent.FLAG_ACTIVITY_NEW_TASK
        }

        try {
            context.startActivity(intent)
        } catch (e: Exception) {
            // Fallback to general settings
            openAppSettings(context)
        }
    }

    /**
     * Open app's settings page
     */
    fun openAppSettings(context: Context) {
        val intent = Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS).apply {
            data = Uri.parse("package:${context.packageName}")
            flags = Intent.FLAG_ACTIVITY_NEW_TASK
        }
        context.startActivity(intent)
    }

    /**
     * Check if device has aggressive battery optimization (common on Chinese OEMs)
     */
    fun hasAggressiveBatteryOptimization(): Boolean {
        val manufacturer = Build.MANUFACTURER.lowercase()
        return manufacturer in listOf(
            "xiaomi", "redmi", "poco",
            "huawei", "honor",
            "oppo", "realme", "oneplus",
            "vivo", "iqoo",
            "samsung", // Some Samsung models
            "meizu",
            "asus"
        )
    }

    /**
     * Get manufacturer-specific battery settings intent
     */
    fun getManufacturerBatteryIntent(context: Context): Intent? {
        val manufacturer = Build.MANUFACTURER.lowercase()

        return when {
            manufacturer.contains("xiaomi") || manufacturer.contains("redmi") -> {
                Intent().apply {
                    component = android.content.ComponentName(
                        "com.miui.powerkeeper",
                        "com.miui.powerkeeper.ui.HiddenAppsConfigActivity"
                    )
                    putExtra("package_name", context.packageName)
                    putExtra("package_label", "ZeroClaw")
                }
            }
            manufacturer.contains("huawei") || manufacturer.contains("honor") -> {
                Intent().apply {
                    component = android.content.ComponentName(
                        "com.huawei.systemmanager",
                        "com.huawei.systemmanager.startupmgr.ui.StartupNormalAppListActivity"
                    )
                }
            }
            manufacturer.contains("samsung") -> {
                Intent().apply {
                    component = android.content.ComponentName(
                        "com.samsung.android.lool",
                        "com.samsung.android.sm.battery.ui.BatteryActivity"
                    )
                }
            }
            manufacturer.contains("oppo") || manufacturer.contains("realme") -> {
                Intent().apply {
                    component = android.content.ComponentName(
                        "com.coloros.safecenter",
                        "com.coloros.safecenter.permission.startup.StartupAppListActivity"
                    )
                }
            }
            else -> null
        }
    }
}
