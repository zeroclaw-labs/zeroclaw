package org.zerodroid.bridge

import android.content.Context
import android.content.Intent
import android.net.LocalServerSocket
import android.net.LocalSocket
import android.net.LocalSocketAddress
import android.os.SystemClock
import android.util.Base64
import android.util.Log
import org.json.JSONException
import org.json.JSONObject
import java.io.BufferedReader
import java.io.File
import java.io.InputStreamReader
import java.io.OutputStream
import java.util.concurrent.Executors

/**
 * Unix-domain-socket JSON-RPC server implementing
 * `docs/book/src/tools/android-bridge-protocol.md` (v1).
 *
 * Transport: a FILESYSTEM-namespace `LocalServerSocket` at `<filesDir>/ui.sock`. The path lives in
 * the app's private data dir (mode 0700), so kernel UID isolation is the whole trust boundary —
 * there is no token, matching the contract. Framing is newline-delimited compact JSON.
 *
 * Ops are brokered to [UiAccessibilityService] via [UiBridge] (broadcast + ResultReceiver), except
 * `ping` (answered here) and `launch` (a plain Intent that needs no a11y). Lifecycle mirrors
 * [ApiServer]: [BridgeService] constructs one, calls [start] in doStart and [stop] in doStop.
 */
class UiSocketServer(private val ctx: Context) {

    private val socketFile = File(ctx.filesDir, SOCKET_NAME)
    @Volatile private var running = false
    private var server: LocalServerSocket? = null
    private var boundSocket: LocalSocket? = null
    private var acceptThread: Thread? = null
    private val workers = Executors.newCachedThreadPool { r ->
        Thread(r, "ui-sock-worker").apply { isDaemon = true }
    }

    fun start() {
        if (running) return
        // Keep the containing dir private (defensive — filesDir is already 0700).
        runCatching { android.system.Os.chmod(ctx.filesDir.absolutePath, 448 /* 0700 */) }
        // Unlink any stale socket left by a previous run, or bind() gets EADDRINUSE.
        runCatching { if (socketFile.exists()) socketFile.delete() }

        val bind = LocalSocket()
        bind.bind(LocalSocketAddress(socketFile.absolutePath, LocalSocketAddress.Namespace.FILESYSTEM))
        boundSocket = bind
        server = LocalServerSocket(bind.fileDescriptor)  // listens on the already-bound fd
        runCatching { android.system.Os.chmod(socketFile.absolutePath, 384 /* 0600 */) }
        running = true

        acceptThread = Thread({
            Log.i(TAG, "UI socket listening at ${socketFile.absolutePath}")
            while (running) {
                val client = try {
                    server?.accept() ?: break
                } catch (e: Exception) {
                    if (running) Log.w(TAG, "accept failed: ${e.message}")
                    break
                }
                workers.execute { handleClient(client) }
            }
        }, "ui-sock-accept").apply { isDaemon = true; start() }
    }

    fun stop() {
        running = false
        runCatching { server?.close() }
        runCatching { boundSocket?.close() }
        runCatching { if (socketFile.exists()) socketFile.delete() }
        server = null; boundSocket = null
        acceptThread = null
    }

    private fun handleClient(client: LocalSocket) {
        try {
            client.soTimeout = 20_000
            val reader = BufferedReader(InputStreamReader(client.inputStream, Charsets.UTF_8))
            val out = client.outputStream
            // Handle every line: supports both one-shot and reused connections (contract allows either).
            while (running) {
                val line = reader.readLine() ?: break
                if (line.isBlank()) continue
                if (line.length > MAX_REQUEST_BYTES) {
                    writeLine(out, fail(null, "bad_args", "request exceeds ${MAX_REQUEST_BYTES}B"))
                    continue
                }
                writeLine(out, dispatch(line))
            }
        } catch (e: Exception) {
            Log.w(TAG, "client error: ${e.message}")
        } finally {
            runCatching { client.close() }
        }
    }

    private fun writeLine(out: OutputStream, obj: JSONObject) {
        out.write((obj.toString() + "\n").toByteArray(Charsets.UTF_8))
        out.flush()
    }

    private fun dispatch(line: String): JSONObject {
        val req = try {
            JSONObject(line)
        } catch (e: JSONException) {
            return fail(null, "bad_args", "invalid JSON: ${e.message}")
        }
        val id = if (req.has("id")) req.optString("id") else null
        val op = req.optString("op").ifEmpty { return fail(id, "bad_args", "missing op") }
        val args = req.optJSONObject("args") ?: JSONObject()

        return try {
            when (op) {
                "ping" -> ok(id, JSONObject()
                    .put("version", PROTOCOL_VERSION)
                    .put("service_connected", UiBridge.isServiceConnected(ctx)))

                "launch" -> launch(id, args)

                "screenshot" -> screenshot(id, args)

                "read" -> brokered(id, "read") { it.putExtra("max_depth", args.optInt("max_depth", 15)) }

                "foreground" -> brokered(id, "foreground") {}

                // Read-only device facts. Answered here rather than brokered to the accessibility
                // service: they need no screen access, so routing them through it would add a
                // Binder round-trip and make them fail whenever the service is off.
                "device" -> {
                    val what = args.optString("what")
                    when (what) {
                        "sensors" -> ok(id, DeviceApi.sensors(ctx))
                        "location" -> ok(id, DeviceApi.location(ctx))
                        "telephony" -> ok(id, DeviceApi.telephony(ctx))
                        else -> fail(id, "bad_args", "device needs what=sensors|location|telephony")
                    }
                }

                "tap" -> {
                    when {
                        args.has("text") -> brokered(id, "tap", 15_000) { it.putExtra("text", args.getString("text")) }
                        args.has("x") && args.has("y") ->
                            brokered(id, "tap") { it.putExtra("x", args.getInt("x")).putExtra("y", args.getInt("y")) }
                        else -> fail(id, "bad_args", "tap needs {x,y} or {text}")
                    }
                }

                "swipe" -> {
                    if (!args.has("x1") || !args.has("y1") || !args.has("x2") || !args.has("y2"))
                        return fail(id, "bad_args", "swipe needs x1,y1,x2,y2")
                    brokered(id, "swipe") {
                        it.putExtra("x1", args.getInt("x1")).putExtra("y1", args.getInt("y1"))
                            .putExtra("x2", args.getInt("x2")).putExtra("y2", args.getInt("y2"))
                            .putExtra("duration_ms", args.optInt("duration_ms", 300))
                    }
                }

                "scroll" -> {
                    val dir = args.optString("direction").ifEmpty {
                        return fail(id, "bad_args", "scroll needs direction")
                    }
                    brokered(id, "scroll") {
                        it.putExtra("direction", dir)
                        if (args.has("x")) it.putExtra("x", args.getInt("x"))
                        if (args.has("y")) it.putExtra("y", args.getInt("y"))
                    }
                }

                "text" -> {
                    if (!args.has("text")) return fail(id, "bad_args", "text needs 'text'")
                    brokered(id, "text") { it.putExtra("text", args.getString("text")) }
                }

                "key" -> {
                    val key = args.optString("key").ifEmpty { return fail(id, "bad_args", "key needs 'key'") }
                    brokered(id, "key") { it.putExtra("key", key) }
                }

                "dialog" -> {
                    val button = args.optString("button").ifEmpty { return fail(id, "bad_args", "dialog needs 'button'") }
                    brokered(id, "dialog") { it.putExtra("button", button) }
                }

                else -> fail(id, "unsupported_op", "unknown op: $op")
            }
        } catch (e: Exception) {
            fail(id, "internal", e.message ?: e.javaClass.simpleName)
        }
    }

    /** Run an a11y-brokered op and wrap its payload (or error_code) in the response envelope. */
    private fun brokered(id: String?, action: String, timeoutMs: Long = 10_000, configure: (Intent) -> Unit): JSONObject {
        val res = UiBridge.request(ctx, action, timeoutMs, configure)
        return envelope(id, res)
    }

    private fun envelope(id: String?, res: JSONObject): JSONObject {
        val code = res.optString("error_code")
        return if (code.isNotEmpty()) fail(id, code, res.optString("error", code))
        else ok(id, res)
    }

    private fun launch(id: String?, args: JSONObject): JSONObject {
        val pkg = args.optString("package").ifEmpty { return fail(id, "bad_args", "launch needs 'package'") }
        val activity = if (args.has("activity")) args.optString("activity") else null
        val intent = if (activity != null) {
            Intent(Intent.ACTION_MAIN).apply {
                setClassName(pkg, activity); addCategory(Intent.CATEGORY_LAUNCHER)
            }
        } else {
            ctx.packageManager.getLaunchIntentForPackage(pkg)
                ?: return fail(id, "not_found", "no launch intent for $pkg")
        }
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        return try {
            ctx.startActivity(intent)
            ok(id, JSONObject().put("launched", true))
        } catch (e: Exception) {
            fail(id, "not_found", "cannot launch $pkg: ${e.message}")
        }
    }

    private fun screenshot(id: String?, args: JSONObject): JSONObject {
        var maxWidth = args.optInt("max_width", 540).coerceIn(ScreenshotSizing.MIN_WIDTH, 4096)
        val deadline = SystemClock.elapsedRealtime() + SCREENSHOT_BUDGET_MS
        repeat(MAX_SCREENSHOT_ATTEMPTS) {
            val remainingMs = deadline - SystemClock.elapsedRealtime()
            if (remainingMs <= 0) {
                return fail(id, "timeout", "screenshot downscaling exceeded the server budget")
            }
            val res = UiBridge.request(ctx, "screenshot", remainingMs) {
                it.putExtra("max_width", maxWidth)
            }
            val code = res.optString("error_code")
            if (code.isNotEmpty()) return fail(id, code, res.optString("error", code))

            val path = res.optString("file_path")
            if (path.isEmpty()) return fail(id, "screenshot_failed", "no file path from a11y")
            val file = File(path)
            try {
                val bytes = file.readBytes()
                val b64 = Base64.encodeToString(bytes, Base64.NO_WRAP)
                if (b64.length <= MAX_BASE64_BYTES) {
                    return ok(id, JSONObject().put("png_base64", b64)
                        .put("width", res.optInt("width")).put("height", res.optInt("height")))
                }

                maxWidth = ScreenshotSizing.nextWidth(
                    requestedWidth = maxWidth,
                    actualWidth = res.optInt("width", maxWidth),
                    encodedBytes = b64.length,
                    maxEncodedBytes = MAX_BASE64_BYTES,
                ) ?: return fail(
                    id,
                    "screenshot_failed",
                    "png remains too large at the minimum capture width (${b64.length}B)",
                )
            } catch (e: Exception) {
                return fail(id, "screenshot_failed", "read failed: ${e.message}")
            } finally {
                runCatching { file.delete() }
            }
        }
        return fail(id, "screenshot_failed", "png did not fit after repeated downscaling")
    }

    private fun ok(id: String?, data: JSONObject): JSONObject {
        val r = JSONObject().put("ok", true)
        if (id != null) r.put("id", id)
        return r.put("data", data)
    }

    private fun fail(id: String?, code: String, message: String): JSONObject {
        val r = JSONObject().put("ok", false)
        if (id != null) r.put("id", id)
        return r.put("error", JSONObject().put("code", code).put("message", message))
    }

    companion object {
        private const val TAG = "zerodroid-ui-sock"
        const val SOCKET_NAME = "ui.sock"
        private const val PROTOCOL_VERSION = "1"
        private const val MAX_REQUEST_BYTES = 64 * 1024
        // Mirrors zeroclaw-tools MAX_BASE64_BYTES (2 MiB) from the contract.
        private const val MAX_BASE64_BYTES = 2 * 1024 * 1024
        private const val MAX_SCREENSHOT_ATTEMPTS = 8
        private const val SCREENSHOT_BUDGET_MS = 10_000L
    }
}
