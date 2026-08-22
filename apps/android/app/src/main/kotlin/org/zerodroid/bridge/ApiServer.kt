package org.zerodroid.bridge

import android.annotation.SuppressLint
import android.app.Notification
import android.app.NotificationManager
import android.bluetooth.BluetoothManager
import android.bluetooth.le.ScanCallback
import android.bluetooth.le.ScanResult
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.net.wifi.WifiManager
import android.os.Build
import android.provider.ContactsContract
import android.telephony.SmsManager
import androidx.core.content.ContextCompat
import com.google.android.gms.tasks.Tasks
import com.google.mlkit.nl.entityextraction.EntityExtraction
import com.google.mlkit.nl.entityextraction.EntityExtractionParams
import com.google.mlkit.nl.entityextraction.EntityExtractorOptions
import com.google.mlkit.nl.languageid.LanguageIdentification
import com.google.mlkit.nl.translate.TranslateLanguage
import com.google.mlkit.nl.translate.Translation
import com.google.mlkit.nl.translate.TranslatorOptions
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.common.InputImage
import com.google.mlkit.vision.text.TextRecognition
import com.google.mlkit.vision.text.latin.TextRecognizerOptions
import fi.iki.elonen.NanoHTTPD
import org.json.JSONArray
import org.json.JSONObject
import java.util.concurrent.TimeUnit

internal fun isValidLocalSecret(value: String): Boolean =
    value.length == 32 && value.all { it in '0'..'9' || it in 'a'..'f' }

internal object BridgePathPolicy {
    fun containsCanonicalFile(base: java.io.File, candidate: java.io.File): Boolean =
        candidate.canonicalFile.toPath().startsWith(base.canonicalFile.toPath())
}

/**
 * Localhost HTTP bridge. Each endpoint is a native-Android / GMS / ML-Kit capability the
 * agent (in its own process) reaches by curl-ing 127.0.0.1:8470. Loopback-only by design.
 */
class ApiServer(private val ctx: Context, port: Int) : NanoHTTPD("127.0.0.1", port) {

    // On Android any local app can reach 127.0.0.1, so the server MUST authenticate callers
    // before exposing permission-protected actions. Token is generated once and stored in the
    // app's private dir. The bundled agent shares this UID; bearer material never goes to external
    // storage and is never printed to logs.
    private val token: String = loadOrCreateToken()
    /** Public so the service can mirror it into the agent's HOME for shell-tool auth. */
    val bridgeToken: String get() = token
    private val imageBase: java.io.File =
        (ctx.getExternalFilesDir(null) ?: ctx.filesDir).canonicalFile

    private fun loadOrCreateToken(): String {
        val f = java.io.File(ctx.filesDir, "bridge-token")
        val existing = runCatching { f.takeIf { it.exists() }?.readText()?.trim() }
            .getOrNull()
            ?.takeIf(::isValidLocalSecret)
        val t = existing ?: java.util.UUID.randomUUID().toString().replace("-", "").also { fresh ->
            val tmp = java.io.File(ctx.filesDir, "bridge-token.tmp")
            tmp.writeText(fresh)
            try {
                android.system.Os.chmod(tmp.absolutePath, 384 /* 0600 */)
                android.system.Os.rename(tmp.absolutePath, f.absolutePath)
            } catch (e: Exception) {
                f.writeText(fresh)
                tmp.delete()
            }
        }
        try { android.system.Os.chmod(f.absolutePath, 384 /* 0600 */) } catch (e: Exception) {}
        android.util.Log.i("zerodroid-bridge", "bridge token ready")
        return t
    }

    override fun serve(session: IHTTPSession): Response {
        val q = session.parameters.mapValues { it.value.firstOrNull() ?: "" }
        val uri = session.uri.trimEnd('/')
        if (uri != "/health") {
            val provided = session.headers["authorization"]?.removePrefix("Bearer ")?.trim()
            if (provided == null || provided != token) {
                return newFixedLengthResponse(Response.Status.UNAUTHORIZED, "application/json",
                    JSONObject().put("error", "unauthorized")
                        .put("hint", "pass Authorization: Bearer <bridge-token>").toString())
            }
        }
        return try {
            val json = when (uri) {
                "/health" -> JSONObject().put("ok", true)
                "", "/caps" -> caps()
                "/device" -> device()
                "/sensors" -> sensors()
                "/location" -> location()
                "/ble/scan" -> bleScan(4000)
                "/wifi/scan" -> wifiScan()
                "/telephony" -> telephony()
                "/contacts" -> contacts(q["limit"]?.toIntOrNull() ?: 50)
                "/sms/list" -> smsList(q["limit"]?.toIntOrNull() ?: 20)
                "/sms/send" -> smsSend(q["to"], q["body"])
                "/notify" -> notify(q["title"] ?: "zerodroid", q["text"] ?: "")
                "/intent" -> fireIntent(q["action"], q["data"], q["package"], q["type"])
                "/ml/ocr" -> mlOcr(q["path"])
                "/ml/langid" -> mlLangId(q["text"])
                "/ml/translate" -> mlTranslate(q["text"], q["to"] ?: "en")
                "/ml/entities" -> mlEntities(q["text"])
                "/ml/barcode" -> mlBarcode(q["path"])
                "/ai/status" -> NanoAi.status(ctx)
                "/ai/generate" -> NanoAi.generate(ctx, q["prompt"])
                "/auth/signin" -> authSignin()
                "/auth/status" -> authStatus()
                "/drive/list" -> driveList()
                "/gmail/list" -> gmailList()
                "/calendar/list" -> calendarList()
                "/photos/pick" -> photosPick()
                "/photos/items" -> photosItems()
                else -> JSONObject().put("error", "unknown endpoint ${session.uri}")
            }
            newFixedLengthResponse(Response.Status.OK, "application/json", json.toString(2))
        } catch (t: Throwable) {
            newFixedLengthResponse(
                Response.Status.INTERNAL_ERROR, "application/json",
                JSONObject().put("error", t.javaClass.simpleName).put("message", t.message).toString()
            )
        }
    }

    private fun granted(p: String) =
        ContextCompat.checkSelfPermission(ctx, p) == PackageManager.PERMISSION_GRANTED

    private fun caps(): JSONObject {
        val perms = listOf(
            "ACCESS_FINE_LOCATION", "READ_SMS", "SEND_SMS", "READ_PHONE_STATE",
            "READ_CONTACTS",
            "BLUETOOTH_SCAN", "BLUETOOTH_CONNECT", "NEARBY_WIFI_DEVICES"
        )
        val map = JSONObject()
        for (p in perms) map.put(p, granted("android.permission.$p"))
        val pm = ctx.packageManager
        val features = JSONObject()
            .put("bluetooth_le", pm.hasSystemFeature(PackageManager.FEATURE_BLUETOOTH_LE))
            .put("camera", pm.hasSystemFeature(PackageManager.FEATURE_CAMERA_ANY))
            .put("nfc", pm.hasSystemFeature(PackageManager.FEATURE_NFC))
            .put("telephony", pm.hasSystemFeature(PackageManager.FEATURE_TELEPHONY))
            .put("gms", try { pm.getPackageInfo("com.google.android.gms", 0); true } catch (e: Exception) { false })
            .put("gemini_app", try { pm.getPackageInfo("com.google.android.apps.bard", 0); true } catch (e: Exception) { false })
        return JSONObject().put("permissions", map).put("features", features)
            .put("image_dir", imageBase.path)
            .put("endpoints", JSONArray(listOf(
                "/device", "/sensors", "/location", "/ble/scan", "/wifi/scan", "/telephony",
                "/contacts", "/sms/list", "/sms/send?to=&body=", "/notify?title=&text=",
                "/intent?action=&data=&package=", "/ml/ocr?path=", "/ml/langid?text=",
                "/ml/translate?text=&to=", "/ml/entities?text=", "/ml/barcode?path=",
                "/ai/status", "/ai/generate?prompt=", "/health",
                "/auth/signin", "/auth/status")))
    }

    private fun device() = JSONObject()
        .put("model", Build.MODEL).put("manufacturer", Build.MANUFACTURER)
        .put("sdk_int", Build.VERSION.SDK_INT).put("release", Build.VERSION.RELEASE)
        .put("device", Build.DEVICE).put("hardware", Build.HARDWARE)

    private fun sensors(): JSONObject = DeviceApi.sensors(ctx)

    private fun location(): JSONObject = DeviceApi.location(ctx)

    @SuppressLint("MissingPermission") // Every platform permission is checked immediately below.
    private fun bleScan(durationMs: Long): JSONObject {
        if (Build.VERSION.SDK_INT >= 31 && !granted("android.permission.BLUETOOTH_SCAN"))
            return JSONObject().put("error", "BLUETOOTH_SCAN not granted")
        if (Build.VERSION.SDK_INT < 31 && !granted("android.permission.ACCESS_FINE_LOCATION"))
            return JSONObject().put("error", "ACCESS_FINE_LOCATION not granted for BLE scan")
        val bm = ctx.getSystemService(Context.BLUETOOTH_SERVICE) as BluetoothManager
        val scanner = bm.adapter?.bluetoothLeScanner
            ?: return JSONObject().put("error", "no BLE scanner (bluetooth off?)")
        val seen = LinkedHashMap<String, JSONObject>()
        val cb = object : ScanCallback() {
            override fun onScanResult(type: Int, r: ScanResult) {
                seen[r.device.address] = JSONObject().put("address", r.device.address)
                    .put("name", try { r.device.name } catch (e: SecurityException) { null }).put("rssi", r.rssi)
            }
        }
        scanner.startScan(cb); Thread.sleep(durationMs); scanner.stopScan(cb)
        return JSONObject().put("count", seen.size).put("devices", JSONArray(seen.values.toList()))
    }

    @SuppressLint("MissingPermission") // Both revocable permissions are checked below.
    private fun wifiScan(): JSONObject {
        if (!granted("android.permission.ACCESS_FINE_LOCATION"))
            return JSONObject().put("error", "ACCESS_FINE_LOCATION not granted for wifi scan")
        if (Build.VERSION.SDK_INT >= 33 && !granted("android.permission.NEARBY_WIFI_DEVICES"))
            return JSONObject().put("error", "NEARBY_WIFI_DEVICES not granted")
        val wm = ctx.applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
        val arr = JSONArray()
        @Suppress("DEPRECATION")
        for (r in wm.scanResults) arr.put(JSONObject().put("bssid", r.BSSID).put("ssid", r.SSID)
            .put("rssi", r.level).put("freq", r.frequency).put("caps", r.capabilities))
        return JSONObject().put("count", arr.length()).put("aps", arr)
    }

    private fun telephony(): JSONObject = DeviceApi.telephony(ctx)

    private fun contacts(limit: Int): JSONObject {
        if (!granted("android.permission.READ_CONTACTS")) return JSONObject().put("error", "READ_CONTACTS not granted")
        val arr = JSONArray()
        ctx.contentResolver.query(ContactsContract.CommonDataKinds.Phone.CONTENT_URI,
            arrayOf(ContactsContract.CommonDataKinds.Phone.DISPLAY_NAME,
                ContactsContract.CommonDataKinds.Phone.NUMBER), null, null, null)?.use { c ->
            var n = 0
            while (c.moveToNext() && n++ < limit)
                arr.put(JSONObject().put("name", c.getString(0)).put("number", c.getString(1)))
        }
        return JSONObject().put("count", arr.length()).put("contacts", arr)
    }

    private fun smsList(limit: Int): JSONObject {
        if (!granted("android.permission.READ_SMS")) return JSONObject().put("error", "READ_SMS not granted")
        val arr = JSONArray()
        ctx.contentResolver.query(Uri.parse("content://sms/inbox"),
            arrayOf("address", "body", "date"), null, null, "date DESC")?.use { c ->
            var n = 0
            while (c.moveToNext() && n++ < limit)
                arr.put(JSONObject().put("from", c.getString(0)).put("body", c.getString(1)).put("date", c.getLong(2)))
        }
        return JSONObject().put("count", arr.length()).put("messages", arr)
    }

    private val smsTimestamps = ArrayDeque<Long>()
    private fun smsSend(to: String?, body: String?): JSONObject {
        if (to.isNullOrBlank() || body.isNullOrBlank()) return JSONObject().put("error", "need to= and body=")
        if (!granted("android.permission.SEND_SMS")) return JSONObject().put("error", "SEND_SMS not granted")
        synchronized(smsTimestamps) {
            val now = System.currentTimeMillis()
            while (smsTimestamps.isNotEmpty() && now - smsTimestamps.first() > 60_000) smsTimestamps.removeFirst()
            if (smsTimestamps.size >= 5) return JSONObject().put("error", "rate limit: max 5 SMS/min")
            smsTimestamps.addLast(now)
        }
        val sm = if (Build.VERSION.SDK_INT >= 31) ctx.getSystemService(SmsManager::class.java) else SmsManager.getDefault()
        sm.sendTextMessage(to, null, body, null, null)
        return JSONObject().put("sent", true).put("to", to)
    }

    @Suppress("DEPRECATION")
    private fun notify(title: String, text: String): JSONObject {
        val mgr = ctx.getSystemService(NotificationManager::class.java)
        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(ctx, "zerodroid-bridge")
        } else {
            Notification.Builder(ctx)
        }
        val n = builder.setContentTitle(title)
            .setContentText(text).setSmallIcon(android.R.drawable.stat_notify_chat).build()
        mgr.notify((System.currentTimeMillis() % 100000).toInt(), n)
        return JSONObject().put("posted", true)
    }

    private val allowedIntentActions = setOf(
        "android.intent.action.VIEW", "android.intent.action.DIAL",
        "android.intent.action.SENDTO", "android.intent.action.SEND",
        "android.intent.action.WEB_SEARCH"
    )

    /** Fire a vetted Intent — launch apps, Maps, dialer, share, Settings. Allowlisted actions only. */
    private fun fireIntent(action: String?, data: String?, pkg: String?, type: String?): JSONObject {
        val act = action ?: Intent.ACTION_VIEW
        if (act !in allowedIntentActions && !act.startsWith("android.settings."))
            return JSONObject().put("error", "action not allowed").put("action", act)
        val i = Intent(act).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        if (!data.isNullOrBlank()) i.data = Uri.parse(data)
        if (!type.isNullOrBlank()) i.type = type
        if (!pkg.isNullOrBlank()) i.setPackage(pkg)
        return try { ctx.startActivity(i); JSONObject().put("launched", true).put("action", i.action) }
        catch (e: Exception) { JSONObject().put("error", e.message) }
    }

    // ---- ML Kit (on-device AI) ----
    private fun img(path: String?): InputImage {
        // Restrict to the app's external files dir; reject path traversal / arbitrary file read.
        val f = java.io.File(path ?: "").canonicalFile
        require(BridgePathPolicy.containsCanonicalFile(imageBase, f)) {
            "path must be under ${imageBase.path} (got ${f.path})"
        }
        require(f.isFile) { "path must name an existing image file" }
        return InputImage.fromFilePath(ctx, Uri.fromFile(f))
    }

    private fun mlOcr(path: String?): JSONObject {
        if (path.isNullOrBlank()) return JSONObject().put("error", "need path= to an image file")
        val t = TextRecognition.getClient(TextRecognizerOptions.DEFAULT_OPTIONS).process(img(path))
        val r = Tasks.await(t, 20, TimeUnit.SECONDS)
        return JSONObject().put("text", r.text).put("blocks", r.textBlocks.size)
    }

    private fun mlLangId(text: String?): JSONObject {
        if (text.isNullOrBlank()) return JSONObject().put("error", "need text=")
        val code = Tasks.await(LanguageIdentification.getClient().identifyLanguage(text), 10, TimeUnit.SECONDS)
        return JSONObject().put("language", code)
    }

    private fun mlTranslate(text: String?, to: String): JSONObject {
        if (text.isNullOrBlank()) return JSONObject().put("error", "need text=")
        val src = Tasks.await(LanguageIdentification.getClient().identifyLanguage(text), 10, TimeUnit.SECONDS)
        if (src == "und") return JSONObject().put("error", "could not detect source language")
        val opts = TranslatorOptions.Builder()
            .setSourceLanguage(TranslateLanguage.fromLanguageTag(src) ?: TranslateLanguage.ENGLISH)
            .setTargetLanguage(TranslateLanguage.fromLanguageTag(to) ?: TranslateLanguage.ENGLISH).build()
        val tr = Translation.getClient(opts)
        Tasks.await(tr.downloadModelIfNeeded(), 120, TimeUnit.SECONDS)
        val out = Tasks.await(tr.translate(text), 20, TimeUnit.SECONDS)
        return JSONObject().put("source", src).put("target", to).put("translation", out)
    }

    private fun mlEntities(text: String?): JSONObject {
        if (text.isNullOrBlank()) return JSONObject().put("error", "need text=")
        val ex = EntityExtraction.getClient(
            EntityExtractorOptions.Builder(EntityExtractorOptions.ENGLISH).build())
        Tasks.await(ex.downloadModelIfNeeded(), 120, TimeUnit.SECONDS)
        val anns = Tasks.await(ex.annotate(EntityExtractionParams.Builder(text).build()), 20, TimeUnit.SECONDS)
        val arr = JSONArray()
        for (a in anns) for (e in a.entities)
            arr.put(JSONObject().put("text", a.annotatedText).put("type", e.type))
        return JSONObject().put("count", arr.length()).put("entities", arr)
    }

    private fun mlBarcode(path: String?): JSONObject {
        if (path.isNullOrBlank()) return JSONObject().put("error", "need path= to an image file")
        val codes = Tasks.await(BarcodeScanning.getClient().process(img(path)), 20, TimeUnit.SECONDS)
        val arr = JSONArray()
        for (b in codes) arr.put(JSONObject().put("value", b.rawValue).put("format", b.format))
        return JSONObject().put("count", arr.length()).put("barcodes", arr)
    }

    // ---- Google Sign-In ----
    private fun authSignin(): JSONObject {
        ctx.startActivity(Intent(ctx, SignInActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        return JSONObject().put("launched", true)
            .put("note", "Pick an account + consent on the phone, then GET /auth/status")
    }

    private fun authStatus(): JSONObject = JSONObject()
        .put("signed_in", Auth.email != null)
        .put("email", Auth.email)
        .put("display_name", Auth.displayName)
        .put("has_access_token", Auth.accessToken != null)
        .put("scopes", JSONArray(Auth.SCOPES))
        .put("error", Auth.error)

    // ---- Google REST APIs (Drive / Gmail / Calendar / Photos) via the access token ----
    private fun gapi(method: String, url: String, body: String? = null): JSONObject {
        val tok = Auth.accessToken ?: return JSONObject().put("error", "no access token — GET /auth/signin first")
        val c = java.net.URL(url).openConnection() as javax.net.ssl.HttpsURLConnection
        c.requestMethod = method
        c.setRequestProperty("Authorization", "Bearer $tok")
        c.connectTimeout = 12000; c.readTimeout = 25000
        if (body != null) {
            c.doOutput = true; c.setRequestProperty("Content-Type", "application/json")
            c.outputStream.use { it.write(body.toByteArray()) }
        }
        val code = c.responseCode
        val txt = (if (code in 200..299) c.inputStream else c.errorStream)?.bufferedReader()?.readText() ?: ""
        return try { JSONObject(txt).put("_status", code) }
        catch (e: Exception) { JSONObject().put("_status", code).put("raw", txt.take(600)) }
    }

    private fun driveList(): JSONObject {
        val r = gapi("GET", "https://www.googleapis.com/drive/v3/files?pageSize=20&fields=files(id,name,mimeType,modifiedTime,size)")
        return JSONObject().put("status", r.optInt("_status"))
            .put("files", r.optJSONArray("files") ?: JSONArray()).put("error", r.opt("error") ?: r.opt("raw"))
    }

    private fun gmailList(): JSONObject {
        val r = gapi("GET", "https://gmail.googleapis.com/gmail/v1/users/me/messages?maxResults=10")
        return JSONObject().put("status", r.optInt("_status"))
            .put("count", r.optJSONArray("messages")?.length() ?: 0)
            .put("messages", r.optJSONArray("messages") ?: JSONArray()).put("error", r.opt("error") ?: r.opt("raw"))
    }

    private fun calendarList(): JSONObject {
        val formatter = java.text.SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ss.SSS'Z'", java.util.Locale.US)
        formatter.timeZone = java.util.TimeZone.getTimeZone("UTC")
        val now = formatter.format(java.util.Date())
        val r = gapi("GET", "https://www.googleapis.com/calendar/v3/calendars/primary/events?maxResults=10&singleEvents=true&orderBy=startTime&timeMin=$now")
        return JSONObject().put("status", r.optInt("_status"))
            .put("events", r.optJSONArray("items") ?: JSONArray()).put("error", r.opt("error") ?: r.opt("raw"))
    }

    private fun photosPick(): JSONObject {
        val r = gapi("POST", "https://photospicker.googleapis.com/v1/sessions", "{}")
        Auth.pickerSessionId = if (r.has("id")) r.getString("id") else null
        return JSONObject().put("status", r.optInt("_status")).put("session_id", Auth.pickerSessionId)
            .put("picker_uri", r.opt("pickerUri")).put("error", r.opt("error") ?: r.opt("raw"))
            .put("note", "open picker_uri on the phone, select photos, then GET /photos/items")
    }

    private fun photosItems(): JSONObject {
        val sid = Auth.pickerSessionId ?: return JSONObject().put("error", "call /photos/pick first")
        val r = gapi("GET", "https://photospicker.googleapis.com/v1/mediaItems?sessionId=$sid")
        return JSONObject().put("status", r.optInt("_status"))
            .put("count", r.optJSONArray("mediaItems")?.length() ?: 0)
            .put("media", r.optJSONArray("mediaItems") ?: JSONArray()).put("error", r.opt("error") ?: r.opt("raw"))
    }

    // Gemini Nano (Google AI Edge SDK) lives in the `full` flavor's NanoAi; the `lite` flavor
    // (minSdk 30, no AI Edge SDK) provides a stub for devices that use cloud providers instead.

    companion object {
        const val PORT = 8470
    }
}
