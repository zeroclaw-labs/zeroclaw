package org.zerodroid.bridge

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.ResultReceiver
import android.provider.Settings
import org.json.JSONObject
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

/**
 * Broker between the UDS socket server and the in-APK [UiAccessibilityService].
 *
 * Ported from CellClaw's AccessibilityBridge; see `apps/android/NOTICE`. Two intentional changes:
 *  - the a11y ComponentName is derived from the context + service class instead of being a
 *    hard-coded package string, so it follows the applicationId automatically;
 *  - the request blocks on a CountDownLatch rather than a coroutine `withTimeout`, so it needs no
 *    kotlinx-coroutines dependency and runs cleanly on a socket worker thread.
 */
object UiBridge {

    /** True when the user has enabled our accessibility service in system settings. */
    fun isServiceConnected(ctx: Context): Boolean {
        val expected = ComponentName(ctx, UiAccessibilityService::class.java).flattenToString()
        val enabled = Settings.Secure.getString(
            ctx.contentResolver, Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES
        ) ?: return false
        return enabled.split(':').any { it.trim() == expected }
    }

    /**
     * Broadcast [action] to the a11y service and block (on the calling thread) for its reply.
     *
     * Returns the service's JSON payload verbatim. On failure the payload carries an `error_code`
     * from the PROTOCOL closed set (`service_unavailable`, `timeout`, ...). Never throws.
     */
    fun request(
        ctx: Context,
        action: String,
        timeoutMs: Long = 10_000,
        configure: (Intent) -> Unit = {}
    ): JSONObject {
        if (!isServiceConnected(ctx)) return err("service_unavailable", "Accessibility service not enabled")

        val latch = CountDownLatch(1)
        val holder = AtomicReference<String?>(null)
        val receiver = object : ResultReceiver(Handler(Looper.getMainLooper())) {
            override fun onReceiveResult(resultCode: Int, resultData: Bundle?) {
                holder.set(resultData?.getString("result"))
                latch.countDown()
            }
        }
        val intent = Intent(UiAccessibilityService.ACTION_COMMAND).apply {
            setPackage(ctx.packageName)
            putExtra("action", action)
            putExtra("result_receiver", receiver)
            configure(this)
        }
        ctx.sendBroadcast(intent)

        val answered = try {
            latch.await(timeoutMs, TimeUnit.MILLISECONDS)
        } catch (e: InterruptedException) {
            Thread.currentThread().interrupt(); false
        }
        if (!answered) return err("timeout", "No reply from accessibility service in ${timeoutMs}ms")
        val json = holder.get() ?: return err("internal", "Empty result from accessibility service")
        return try {
            JSONObject(json)
        } catch (e: Exception) {
            err("internal", "Malformed a11y result: ${e.message}")
        }
    }

    private fun err(code: String, message: String) =
        JSONObject().put("error_code", code).put("error", message)
}
