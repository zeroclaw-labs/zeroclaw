package com.mobileclaw.app

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.os.Build
import androidx.core.content.ContextCompat
import androidx.work.Constraints
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.ByteArrayOutputStream
import java.io.BufferedReader
import java.io.BufferedInputStream
import java.io.BufferedWriter
import java.io.InputStreamReader
import java.io.OutputStreamWriter
import java.net.HttpURLConnection
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.net.URL
import java.util.UUID
import java.util.concurrent.TimeUnit
import android.content.pm.PackageManager
import android.provider.CallLog
import android.provider.Telephony

object RuntimeBridge {
    private const val PREFS = "mobileclaw-runtime-bridge"
    private const val QUEUE_KEY = "pending_events"
    private const val TELEGRAM_ENABLED = "telegram_enabled"
    private const val TELEGRAM_BOT_TOKEN = "telegram_bot_token"
    private const val RUNTIME_PROVIDER = "runtime_provider"
    private const val RUNTIME_MODEL = "runtime_model"
    private const val RUNTIME_API_URL = "runtime_api_url"
    private const val RUNTIME_API_KEY = "runtime_api_key"
    private const val RUNTIME_TEMPERATURE = "runtime_temperature"
    private const val ALWAYS_ON_MODE = "always_on_mode"
    private const val INCOMING_CALL_HOOKS = "incoming_call_hooks"
    private const val INCOMING_SMS_HOOKS = "incoming_sms_hooks"
    private const val LAST_UPDATE_ID = "telegram_last_update_id"
    private const val TELEGRAM_SEEN_COUNT = "telegram_seen_count"
    private const val WEBHOOK_SUCCESS_COUNT = "webhook_success_count"
    private const val WEBHOOK_FAIL_COUNT = "webhook_fail_count"
    private const val LAST_EVENT_NOTE = "last_event_note"
    private const val MAX_QUEUE = 500
    private const val LOCAL_GATEWAY = "http://127.0.0.1:8080"
    private const val RUNTIME_DIR = "runtime"
    private const val RUNTIME_BIN_DIR = "bin"
    private const val RUNTIME_WORKSPACE_DIR = "workspace"
    private const val RUNTIME_LOG_DIR = "logs"
    private const val DAEMON_STDOUT = "daemon.out.log"
    private const val DAEMON_STDERR = "daemon.err.log"
    private const val RUNTIME_CONFIG_FILE = "zeroclaw-mobile.toml"
    const val ANDROID_BRIDGE_HOST = "127.0.0.1"
    const val ANDROID_BRIDGE_PORT = 9797
    const val ANDROID_BRIDGE_PATH = "/v1/android/actions"
    private const val PERIODIC_WORK_NAME = "mobileclaw-runtime-bridge-periodic"
    private const val ONETIME_WORK_NAME = "mobileclaw-runtime-bridge-onetime"
    const val CHANNEL_ID = "mobileclaw_runtime"
    @Volatile
    private var actionBridgeServer: AndroidActionBridgeServer? = null

    fun configure(context: Context, config: JSONObject) {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val telegramEnabled = config.optBoolean("telegramEnabled", false)
        val telegramBotToken = config.optString("telegramBotToken", "").trim()
        val alwaysOnMode = config.optBoolean("alwaysOnMode", false)
        val incomingCallHooks = config.optBoolean("incomingCallHooks", true)
        val incomingSmsHooks = config.optBoolean("incomingSmsHooks", true)
        val runtimeProvider = config.optString("runtimeProvider", "openrouter").trim().lowercase()
        val runtimeModel = config.optString("runtimeModel", "").trim()
        val runtimeApiUrl = config.optString("runtimeApiUrl", "").trim()
        val runtimeApiKey = config.optString("runtimeApiKey", "").trim()
        val runtimeTemperature = config.optDouble("runtimeTemperature", 0.1)

        prefs.edit()
            .remove("platform_url")
            .putBoolean(TELEGRAM_ENABLED, telegramEnabled)
            .putString(TELEGRAM_BOT_TOKEN, telegramBotToken)
            .putBoolean(ALWAYS_ON_MODE, alwaysOnMode)
            .putBoolean(INCOMING_CALL_HOOKS, incomingCallHooks)
            .putBoolean(INCOMING_SMS_HOOKS, incomingSmsHooks)
            .putString(RUNTIME_PROVIDER, runtimeProvider)
            .putString(RUNTIME_MODEL, runtimeModel)
            .putString(RUNTIME_API_URL, runtimeApiUrl)
            .putString(RUNTIME_API_KEY, runtimeApiKey)
            .putFloat(RUNTIME_TEMPERATURE, runtimeTemperature.toFloat())
            .apply()

        writeRuntimeConfig(context)
        ensureAndroidActionBridge(context)
        ensureDaemonRunning(context)
        resetPendingSchedule(context)
        ensureNotificationChannel(context)
        schedulePeriodic(context)
        if (alwaysOnMode) {
            startAlwaysOn(context)
        } else {
            stopAlwaysOn(context)
        }
        scheduleImmediate(context)
    }

    fun enqueueHookEvent(context: Context, kind: String, detail: String) {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        if (kind == "incoming_call" && !prefs.getBoolean(INCOMING_CALL_HOOKS, true)) return
        if (kind == "incoming_sms" && !prefs.getBoolean(INCOMING_SMS_HOOKS, true)) return

        val payload = JSONObject()
            .put("id", UUID.randomUUID().toString())
            .put("kind", kind)
            .put("message", if (kind == "incoming_sms") "Incoming SMS event: $detail" else "Incoming call event: $detail")
            .put("telegramChatId", "")
            .put("attempts", 0)
            .put("nextAttemptAt", System.currentTimeMillis())
            .put("createdAt", System.currentTimeMillis())
        enqueue(context, payload)
        scheduleImmediate(context)
    }

    fun runBackgroundTick(context: Context): TickResult {
        ensureAndroidActionBridge(context)
        ensureDaemonRunning(context)
        observeTelegramInbound(context)
        return flushQueue(context)
    }

    fun queueSize(context: Context): Int {
        return loadQueue(context).length()
    }

    fun bridgeStatus(context: Context): JSONObject {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val runtimeReady = prefs.getString(RUNTIME_API_KEY, "")?.isNotBlank() == true &&
            prefs.getString(RUNTIME_MODEL, "")?.isNotBlank() == true
        val daemonUp = isLocalGatewayHealthy()
        return JSONObject()
            .put("queue_size", queueSize(context))
            .put("always_on", isAlwaysOnEnabled(context))
            .put("runtime_ready", runtimeReady)
            .put("daemon_up", daemonUp)
            .put("telegram_seen_count", prefs.getLong(TELEGRAM_SEEN_COUNT, 0L))
            .put("webhook_success_count", prefs.getLong(WEBHOOK_SUCCESS_COUNT, 0L))
            .put("webhook_fail_count", prefs.getLong(WEBHOOK_FAIL_COUNT, 0L))
            .put("last_event_note", prefs.getString(LAST_EVENT_NOTE, "") ?: "")
    }

    private fun enqueue(context: Context, item: JSONObject) {
        val queue = loadQueue(context)
        val itemId = item.optString("id", "")
        if (itemId.isNotBlank()) {
            for (idx in 0 until queue.length()) {
                val existing = queue.optJSONObject(idx) ?: continue
                if (existing.optString("id", "") == itemId) {
                    return
                }
            }
        }
        queue.put(item)
        val trimmed = JSONArray()
        val start = maxOf(0, queue.length() - MAX_QUEUE)
        for (idx in start until queue.length()) {
            trimmed.put(queue.optJSONObject(idx) ?: continue)
        }
        saveQueue(context, trimmed)
    }

    private fun loadQueue(context: Context): JSONArray {
        val raw = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(QUEUE_KEY, "[]") ?: "[]"
        return try {
            JSONArray(raw)
        } catch (_: Throwable) {
            JSONArray()
        }
    }

    private fun saveQueue(context: Context, queue: JSONArray) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit()
            .putString(QUEUE_KEY, queue.toString())
            .apply()
    }

    private fun resetPendingSchedule(context: Context) {
        val queue = loadQueue(context)
        val now = System.currentTimeMillis()
        for (idx in 0 until queue.length()) {
            val item = queue.optJSONObject(idx) ?: continue
            item.put("nextAttemptAt", now)
        }
        saveQueue(context, queue)
    }

    private fun runtimeRoot(context: Context): File = File(context.filesDir, RUNTIME_DIR)

    private fun runtimeBinaryFile(context: Context): File {
        return File(context.applicationInfo.nativeLibraryDir, "libzeroclaw.so")
    }

    private fun runtimeConfigFile(context: Context): File {
        return File(File(runtimeRoot(context), RUNTIME_WORKSPACE_DIR), "config.toml")
    }

    private fun writeRuntimeConfig(context: Context) {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val provider = prefs.getString(RUNTIME_PROVIDER, "openrouter")?.trim().orEmpty()
        val model = prefs.getString(RUNTIME_MODEL, "")?.trim().orEmpty()
        val apiUrl = prefs.getString(RUNTIME_API_URL, "")?.trim().orEmpty()
        val apiKey = prefs.getString(RUNTIME_API_KEY, "")?.trim().orEmpty()
        val temperature = prefs.getFloat(RUNTIME_TEMPERATURE, 0.1f)
        val telegramEnabled = prefs.getBoolean(TELEGRAM_ENABLED, false)
        val telegramBotToken = prefs.getString(TELEGRAM_BOT_TOKEN, "")?.trim().orEmpty()

        val cfgFile = runtimeConfigFile(context)
        cfgFile.parentFile?.mkdirs()

        val content = buildString {
            if (apiKey.isNotBlank()) {
                appendLine("api_key = \"${escapeToml(apiKey)}\"")
            }
            if (apiUrl.isNotBlank()) {
                appendLine("api_url = \"${escapeToml(apiUrl)}\"")
            }
            appendLine("default_provider = \"${escapeToml(provider)}\"")
            appendLine("default_model = \"${escapeToml(model)}\"")
            appendLine("default_temperature = ${temperature}")
            appendLine()
            appendLine("[gateway]")
            appendLine("require_pairing = false")
            appendLine("allow_public_bind = false")
            appendLine()
            appendLine("[channels_config]")
            appendLine("cli = false")
            if (telegramEnabled && telegramBotToken.isNotBlank()) {
                appendLine()
                appendLine("[channels_config.telegram]")
                appendLine("bot_token = \"${escapeToml(telegramBotToken)}\"")
                appendLine("allowed_users = [\"*\"]")
            }

            appendLine()
            appendLine("[android]")
            appendLine("enabled = true")
            appendLine("distribution = \"full\"")
            appendLine()
            appendLine("[android.capabilities]")
            appendLine("sms = true")
            appendLine("calls = true")
            appendLine("app_launch = true")
            appendLine("sensors = true")
            appendLine("network = true")
            appendLine("battery = true")
            appendLine()
            appendLine("[android.bridge]")
            appendLine("mode = \"http\"")
            appendLine("endpoint = \"http://$ANDROID_BRIDGE_HOST:$ANDROID_BRIDGE_PORT$ANDROID_BRIDGE_PATH\"")
            appendLine("allow_remote_endpoint = false")
            appendLine("timeout_ms = 5000")
            appendLine()
            appendLine("[android.policy]")
            appendLine("require_explicit_approval = false")
        }

        BufferedWriter(OutputStreamWriter(FileOutputStream(cfgFile, false), Charsets.UTF_8)).use { out ->
            out.write(content)
        }
    }

    private fun ensureBinaryExtracted(context: Context): File {
        val packaged = runtimeBinaryFile(context)
        if (packaged.exists() && packaged.length() > 0) {
            return packaged
        }

        val abi = Build.SUPPORTED_ABIS.firstOrNull() ?: "x86_64"
        val target = File(File(runtimeRoot(context), RUNTIME_BIN_DIR), "zeroclaw-$abi")
        target.parentFile?.mkdirs()

        val assetPath = "zeroclaw/$abi/zeroclaw"
        context.assets.open(assetPath).use { input ->
            FileOutputStream(target, false).use { output ->
                input.copyTo(output)
            }
        }
        target.setExecutable(true)
        return target
    }

    private fun ensureDaemonRunning(context: Context) {
        writeRuntimeConfig(context)
        if (isLocalGatewayHealthy()) return

        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val ready = prefs.getString(RUNTIME_API_KEY, "")?.isNotBlank() == true &&
            prefs.getString(RUNTIME_MODEL, "")?.isNotBlank() == true
        if (!ready) {
            prefs.edit().putString(LAST_EVENT_NOTE, "Runtime credentials missing; daemon not started").apply()
            return
        }

        try {
            val binary = ensureBinaryExtracted(context)
            val cfgFile = runtimeConfigFile(context)
            val workspaceDir = cfgFile.parentFile ?: throw IOException("Missing workspace directory")
            workspaceDir.mkdirs()
            val logsDir = File(runtimeRoot(context), RUNTIME_LOG_DIR)
            logsDir.mkdirs()
            val stdoutFile = File(logsDir, DAEMON_STDOUT)
            val stderrFile = File(logsDir, DAEMON_STDERR)

            val launcher = ProcessBuilder(
                binary.absolutePath,
                "daemon",
                "--host",
                "127.0.0.1",
                "--port",
                "8080",
            )
            launcher.directory(runtimeRoot(context))
            launcher.environment()["ZEROCLAW_WORKSPACE"] = workspaceDir.absolutePath
            launcher.environment()["HOME"] = runtimeRoot(context).absolutePath
            launcher.environment()["RUST_LOG"] = "info"

            val process = launcher
                .redirectOutput(stdoutFile)
                .redirectError(stderrFile)
                .start()

            // Detach and give daemon time to boot.
            process.outputStream.close()
            process.inputStream.close()
            process.errorStream.close()

            Thread.sleep(350)
            if (!process.isAlive) {
                val exitCode = process.exitValue()
                prefs.edit().putString(LAST_EVENT_NOTE, "Local daemon exited early (code $exitCode)").apply()
                return
            }

            prefs.edit().putString(LAST_EVENT_NOTE, "Local ZeroClaw daemon starting").apply()
        } catch (error: Throwable) {
            val detail = (error.message ?: error::class.java.simpleName).take(120)
            prefs.edit().putString(LAST_EVENT_NOTE, "Failed to start local daemon: $detail").apply()
        }
    }

    private fun ensureAndroidActionBridge(context: Context) {
        val running = actionBridgeServer
        if (running?.isRunning() == true) return
        synchronized(this) {
            val current = actionBridgeServer
            if (current?.isRunning() == true) return
            val server = AndroidActionBridgeServer(context.applicationContext)
            server.start()
            actionBridgeServer = server
        }
    }

    private fun isLocalGatewayHealthy(): Boolean {
        val connection = (URL("$LOCAL_GATEWAY/health").openConnection() as HttpURLConnection)
        return try {
            connection.requestMethod = "GET"
            connection.connectTimeout = 1200
            connection.readTimeout = 1200
            connection.doInput = true
            connection.responseCode in 200..299
        } catch (_: Throwable) {
            false
        } finally {
            connection.disconnect()
        }
    }

    private fun escapeToml(raw: String): String {
        return raw.replace("\\", "\\\\").replace("\"", "\\\"")
    }

    private fun schedulePeriodic(context: Context) {
        val request = PeriodicWorkRequestBuilder<RuntimeBridgeWorker>(15, TimeUnit.MINUTES)
            .setConstraints(
                Constraints.Builder()
                    .setRequiredNetworkType(NetworkType.CONNECTED)
                    .build()
            )
            .build()
        WorkManager.getInstance(context).enqueueUniquePeriodicWork(
            PERIODIC_WORK_NAME,
            ExistingPeriodicWorkPolicy.UPDATE,
            request,
        )
    }

    fun scheduleImmediate(context: Context) {
        val request = OneTimeWorkRequestBuilder<RuntimeBridgeWorker>()
            .setConstraints(
                Constraints.Builder()
                    .setRequiredNetworkType(NetworkType.CONNECTED)
                    .build()
            )
            .build()
        WorkManager.getInstance(context).enqueueUniqueWork(
            ONETIME_WORK_NAME,
            ExistingWorkPolicy.REPLACE,
            request,
        )
    }

    private fun startAlwaysOn(context: Context) {
        val intent = Intent(context, RuntimeAlwaysOnService::class.java)
        ContextCompat.startForegroundService(context, intent)
    }

    private fun stopAlwaysOn(context: Context) {
        val intent = Intent(context, RuntimeAlwaysOnService::class.java)
        context.stopService(intent)
    }

    fun isAlwaysOnEnabled(context: Context): Boolean {
        return context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(ALWAYS_ON_MODE, false)
    }

    fun ensureNotificationChannel(context: Context) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val manager = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        val channel = NotificationChannel(
            CHANNEL_ID,
            "MobileClaw Runtime",
            NotificationManager.IMPORTANCE_LOW,
        )
        channel.description = "Keeps runtime hooks and Telegram relay active"
        manager.createNotificationChannel(channel)
    }


    private fun flushQueue(context: Context): TickResult {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        if (!isLocalGatewayHealthy()) {
            return TickResult(0, 0, "local gateway is down")
        }

        val queue = loadQueue(context)
        val now = System.currentTimeMillis()
        val kept = JSONArray()
        var delivered = 0
        var failed = 0

        for (idx in 0 until queue.length()) {
            val item = queue.optJSONObject(idx) ?: continue
            val nextAttemptAt = item.optLong("nextAttemptAt", 0L)
            if (nextAttemptAt > now) {
                kept.put(item)
                continue
            }

            val message = item.optString("message", "").trim()
            if (message.isBlank()) {
                delivered += 1
                continue
            }

            val webhookRes = postWebhook("$LOCAL_GATEWAY/webhook", message)
            if (!webhookRes.ok) {
                failed += 1
                prefs.edit()
                    .putLong(WEBHOOK_FAIL_COUNT, prefs.getLong(WEBHOOK_FAIL_COUNT, 0L) + 1L)
                    .putString(LAST_EVENT_NOTE, "Local webhook forward failed")
                    .apply()
                val attempts = item.optInt("attempts", 0) + 1
                val backoffMs = minOf(30 * 60 * 1000L, (15_000L * (1L shl minOf(6, attempts))))
                item.put("attempts", attempts)
                item.put("nextAttemptAt", now + backoffMs)
                kept.put(item)
                continue
            }

            val kind = item.optString("kind", "")
            if (kind == "incoming_call" || kind == "incoming_sms") {
                val note = if (kind == "incoming_call") {
                    "Incoming call hook received on-device"
                } else {
                    "Incoming SMS hook received on-device"
                }
                prefs.edit().putString(LAST_EVENT_NOTE, note).apply()
            }

            delivered += 1
            prefs.edit()
                .putLong(WEBHOOK_SUCCESS_COUNT, prefs.getLong(WEBHOOK_SUCCESS_COUNT, 0L) + 1L)
                .putString(LAST_EVENT_NOTE, "Event delivered to local ZeroClaw webhook")
                .apply()
        }

        saveQueue(context, kept)
        return TickResult(delivered, failed, null)
    }

    private fun observeTelegramInbound(context: Context) {
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val enabled = prefs.getBoolean(TELEGRAM_ENABLED, false)
        val botToken = prefs.getString(TELEGRAM_BOT_TOKEN, "")?.trim().orEmpty()
        if (!enabled || botToken.isBlank()) return

        val response = httpGet("https://api.telegram.org/bot$botToken/getUpdates?timeout=1&limit=1") ?: return
        val parsed = try {
            JSONObject(response)
        } catch (_: Throwable) {
            return
        }
        if (!parsed.optBoolean("ok", false)) return
        val updates = parsed.optJSONArray("result") ?: return
        if (updates.length() == 0) return
        val item = updates.optJSONObject(updates.length() - 1) ?: return
        val updateId = item.optLong("update_id", 0L)
        val lastObserved = prefs.getLong(LAST_UPDATE_ID, 0L)
        if (updateId <= lastObserved) return

        prefs.edit()
            .putLong(LAST_UPDATE_ID, updateId)
            .putLong(TELEGRAM_SEEN_COUNT, prefs.getLong(TELEGRAM_SEEN_COUNT, 0L) + 1L)
            .putString(LAST_EVENT_NOTE, "Telegram inbound observed by on-device runtime")
            .apply()
    }

    private fun postWebhook(endpoint: String, message: String): WebhookResult {
        return try {
            val response = httpPostJson(endpoint, JSONObject().put("message", message).toString())
            if (response == null) WebhookResult(false, "") else WebhookResult(true, response)
        } catch (_: Throwable) {
            WebhookResult(false, "")
        }
    }


    private fun httpGet(endpoint: String): String? {
        val connection = (URL(endpoint).openConnection() as HttpURLConnection)
        return try {
            connection.requestMethod = "GET"
            connection.connectTimeout = 10_000
            connection.readTimeout = 25_000
            connection.doInput = true
            val status = connection.responseCode
            if (status !in 200..299) return null
            readAll(connection)
        } finally {
            connection.disconnect()
        }
    }

    private fun httpPostJson(endpoint: String, body: String): String? {
        val connection = (URL(endpoint).openConnection() as HttpURLConnection)
        return try {
            connection.requestMethod = "POST"
            connection.connectTimeout = 12_000
            connection.readTimeout = 25_000
            connection.doOutput = true
            connection.setRequestProperty("Content-Type", "application/json")
            connection.outputStream.use { out ->
                out.write(body.toByteArray(Charsets.UTF_8))
            }
            val status = connection.responseCode
            if (status !in 200..299) return null
            readAll(connection)
        } finally {
            connection.disconnect()
        }
    }

    private fun readAll(connection: HttpURLConnection): String {
        val stream = connection.inputStream
        BufferedReader(InputStreamReader(stream)).use { reader ->
            return buildString {
                while (true) {
                    val line = reader.readLine() ?: break
                    append(line)
                }
            }
        }
    }
}

private class AndroidActionBridgeServer(private val context: Context) {
    @Volatile
    private var running = false
    private var serverSocket: ServerSocket? = null

    fun isRunning(): Boolean = running

    fun start() {
        Thread({ runLoop() }, "mobileclaw-android-bridge").apply {
            isDaemon = true
            start()
        }
    }

    private fun runLoop() {
        try {
            val socket = ServerSocket()
            socket.reuseAddress = true
            socket.bind(InetSocketAddress(RuntimeBridge.ANDROID_BRIDGE_HOST, RuntimeBridge.ANDROID_BRIDGE_PORT))
            serverSocket = socket
            running = true
            while (running) {
                val client = socket.accept()
                handleClient(client)
            }
        } catch (_: Throwable) {
            running = false
        }
    }

    private fun handleClient(client: Socket) {
        client.use { socket ->
            val input = BufferedInputStream(socket.getInputStream())
            val request = readRequest(input)
            if (request == null) {
                writeResponse(socket, 400, "{\"ok\":false,\"error\":\"bad_request\"}")
                return
            }

            if (request.method != "POST" || request.path != RuntimeBridge.ANDROID_BRIDGE_PATH) {
                writeResponse(socket, 404, "{\"ok\":false,\"error\":\"not_found\"}")
                return
            }

            val parsed = try {
                JSONObject(request.body)
            } catch (_: Throwable) {
                writeResponse(socket, 400, "{\"ok\":false,\"error\":\"invalid_json\"}")
                return
            }

            val action = parsed.optString("action", "").trim()
            val payload = parsed.optJSONObject("payload") ?: JSONObject()
            val response = dispatch(action, payload)
            writeResponse(socket, 200, response.toString())
        }
    }

    private fun dispatch(action: String, payload: JSONObject): JSONObject {
        return try {
            when (action) {
                "read_sms" -> readSms(payload)
                "read_call_log" -> readCallLog(payload)
                else -> JSONObject()
                    .put("ok", false)
                    .put("error", "unsupported_action")
                    .put("action", action)
            }
        } catch (error: Throwable) {
            JSONObject()
                .put("ok", false)
                .put("error", error.message ?: "bridge_error")
                .put("action", action)
        }
    }

    private fun readSms(payload: JSONObject): JSONObject {
        val permission = ContextCompat.checkSelfPermission(context, android.Manifest.permission.READ_SMS)
        if (permission != PackageManager.PERMISSION_GRANTED) {
            return JSONObject().put("ok", false).put("error", "read_sms_permission_required")
        }

        val limit = payload.optInt("limit", 10).coerceIn(1, 100)
        val cursor = context.contentResolver.query(
            Telephony.Sms.Inbox.CONTENT_URI,
            arrayOf(Telephony.Sms.ADDRESS, Telephony.Sms.BODY, Telephony.Sms.DATE),
            null,
            null,
            "${Telephony.Sms.DATE} DESC",
        )

        val entries = JSONArray()
        cursor?.use {
            val addressIdx = it.getColumnIndex(Telephony.Sms.ADDRESS)
            val bodyIdx = it.getColumnIndex(Telephony.Sms.BODY)
            val dateIdx = it.getColumnIndex(Telephony.Sms.DATE)
            var count = 0
            while (it.moveToNext() && count < limit) {
                entries.put(
                    JSONObject()
                        .put("address", if (addressIdx >= 0) it.getString(addressIdx) ?: "" else "")
                        .put("body", if (bodyIdx >= 0) it.getString(bodyIdx) ?: "" else "")
                        .put("ts", if (dateIdx >= 0) it.getLong(dateIdx) else 0L),
                )
                count += 1
            }
        }

        return JSONObject()
            .put("ok", true)
            .put("action", "read_sms")
            .put("entry_count", entries.length())
            .put("entries", entries)
    }

    private fun readCallLog(payload: JSONObject): JSONObject {
        val permission = ContextCompat.checkSelfPermission(context, android.Manifest.permission.READ_CALL_LOG)
        if (permission != PackageManager.PERMISSION_GRANTED) {
            return JSONObject().put("ok", false).put("error", "read_call_log_permission_required")
        }

        val limit = payload.optInt("limit", 10).coerceIn(1, 100)
        val cursor = context.contentResolver.query(
            CallLog.Calls.CONTENT_URI,
            arrayOf(
                CallLog.Calls.NUMBER,
                CallLog.Calls.CACHED_NAME,
                CallLog.Calls.TYPE,
                CallLog.Calls.DATE,
                CallLog.Calls.DURATION,
            ),
            null,
            null,
            "${CallLog.Calls.DATE} DESC",
        )

        val entries = JSONArray()
        cursor?.use {
            val numberIdx = it.getColumnIndex(CallLog.Calls.NUMBER)
            val nameIdx = it.getColumnIndex(CallLog.Calls.CACHED_NAME)
            val typeIdx = it.getColumnIndex(CallLog.Calls.TYPE)
            val dateIdx = it.getColumnIndex(CallLog.Calls.DATE)
            val durationIdx = it.getColumnIndex(CallLog.Calls.DURATION)
            var count = 0
            while (it.moveToNext() && count < limit) {
                val direction = when (if (typeIdx >= 0) it.getInt(typeIdx) else 0) {
                    CallLog.Calls.INCOMING_TYPE -> "incoming"
                    CallLog.Calls.OUTGOING_TYPE -> "outgoing"
                    CallLog.Calls.MISSED_TYPE -> "missed"
                    CallLog.Calls.REJECTED_TYPE -> "rejected"
                    CallLog.Calls.BLOCKED_TYPE -> "blocked"
                    CallLog.Calls.VOICEMAIL_TYPE -> "voicemail"
                    else -> "unknown"
                }
                entries.put(
                    JSONObject()
                        .put("number", if (numberIdx >= 0) it.getString(numberIdx) ?: "" else "")
                        .put("name", if (nameIdx >= 0) it.getString(nameIdx) ?: "" else "")
                        .put("direction", direction)
                        .put("ts", if (dateIdx >= 0) it.getLong(dateIdx) else 0L)
                        .put("duration_seconds", if (durationIdx >= 0) it.getLong(durationIdx) else 0L),
                )
                count += 1
            }
        }

        return JSONObject()
            .put("ok", true)
            .put("action", "read_call_log")
            .put("entry_count", entries.length())
            .put("entries", entries)
    }

    private fun readRequest(input: InputStream): HttpRequest? {
        val headerBytes = ByteArrayOutputStream()
        var state = 0
        while (true) {
            val value = input.read()
            if (value == -1) return null
            headerBytes.write(value)
            state = when {
                state == 0 && value == '\r'.code -> 1
                state == 1 && value == '\n'.code -> 2
                state == 2 && value == '\r'.code -> 3
                state == 3 && value == '\n'.code -> 4
                else -> 0
            }
            if (state == 4) break
            if (headerBytes.size() > 8192) return null
        }

        val headerText = String(headerBytes.toByteArray(), Charsets.UTF_8)
        val headerLines = headerText.split("\r\n").filter { it.isNotBlank() }
        if (headerLines.isEmpty()) return null
        val requestLine = headerLines[0].split(" ")
        if (requestLine.size < 2) return null
        val method = requestLine[0].trim().uppercase()
        val path = requestLine[1].trim()

        var contentLength = 0
        for (i in 1 until headerLines.size) {
            val line = headerLines[i]
            val sep = line.indexOf(':')
            if (sep <= 0) continue
            val key = line.substring(0, sep).trim().lowercase()
            val value = line.substring(sep + 1).trim()
            if (key == "content-length") {
                contentLength = value.toIntOrNull() ?: 0
            }
        }

        val bodyBytes = ByteArray(contentLength)
        var offset = 0
        while (offset < contentLength) {
            val read = input.read(bodyBytes, offset, contentLength - offset)
            if (read <= 0) break
            offset += read
        }
        if (offset != contentLength) return null
        val body = String(bodyBytes, Charsets.UTF_8)
        return HttpRequest(method = method, path = path, body = body)
    }

    private fun writeResponse(socket: Socket, statusCode: Int, body: String) {
        val payload = body.toByteArray(Charsets.UTF_8)
        val statusText = if (statusCode == 200) "OK" else "ERROR"
        val head = "HTTP/1.1 $statusCode $statusText\r\n" +
            "Content-Type: application/json\r\n" +
            "Content-Length: ${payload.size}\r\n" +
            "Connection: close\r\n\r\n"
        val out = socket.getOutputStream()
        out.write(head.toByteArray(Charsets.UTF_8))
        out.write(payload)
        out.flush()
    }
}

private data class HttpRequest(
    val method: String,
    val path: String,
    val body: String,
)

data class TickResult(
    val delivered: Int,
    val failed: Int,
    val error: String?,
)

data class WebhookResult(
    val ok: Boolean,
    val responseText: String,
)
