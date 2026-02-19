package com.mobileclaw.app

import android.app.NotificationChannel
import android.app.NotificationManager
import android.accessibilityservice.AccessibilityService
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothDevice
import android.location.Location
import android.location.LocationManager
import android.os.Environment
import android.os.BatteryManager
import android.telephony.SmsManager
import android.provider.MediaStore
import android.hardware.Sensor
import android.hardware.SensorManager
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.net.Uri
import android.os.Build
import android.os.VibrationEffect
import android.os.Vibrator
import android.provider.CalendarContract
import android.provider.CallLog
import android.provider.ContactsContract
import android.provider.Settings
import android.provider.Telephony
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import android.content.ComponentName
import android.content.ClipData
import com.facebook.react.bridge.Arguments
import com.facebook.react.bridge.Promise
import com.facebook.react.bridge.ReactApplicationContext
import com.facebook.react.bridge.ReactContextBaseJavaModule
import com.facebook.react.bridge.ReactMethod
import com.facebook.react.bridge.ReadableMap
import com.facebook.react.modules.core.DeviceEventManagerModule
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.lang.ref.WeakReference

class AndroidAgentToolsModule(private val appContext: ReactApplicationContext) : ReactContextBaseJavaModule(appContext) {

    private val prefs: SharedPreferences by lazy {
        appContext.getSharedPreferences("mobileclaw-agent-tools", Context.MODE_PRIVATE)
    }

    init {
        Companion.reactContext = WeakReference(appContext)
    }

    override fun getName(): String = "AndroidAgentTools"

    companion object {
        private var reactContext: WeakReference<ReactApplicationContext>? = null
        private val pendingEvents = mutableListOf<JSONObject>()

        fun emitIncomingCall(context: Context, state: String, phone: String?) {
            val payload = JSONObject()
                .put("event", "incoming_call")
                .put("state", state)
                .put("phone", phone ?: "")
                .put("ts", System.currentTimeMillis())

            RuntimeBridge.enqueueHookEvent(context, "incoming_call", "$state from ${phone ?: "unknown"}")

            val react = reactContext?.get()
            if (react?.hasActiveReactInstance() == true) {
                react
                    .getJSModule(DeviceEventManagerModule.RCTDeviceEventEmitter::class.java)
                    .emit("mobileclaw_incoming_call", payload.toString())
                return
            }

            synchronized(pendingEvents) {
                pendingEvents.add(payload)
            }
        }

        fun emitIncomingSms(context: Context, address: String?, body: String?) {
            val payload = JSONObject()
                .put("event", "incoming_sms")
                .put("address", address ?: "")
                .put("body", body ?: "")
                .put("ts", System.currentTimeMillis())

            val condensedBody = (body ?: "").replace("\n", " ").take(180)
            RuntimeBridge.enqueueHookEvent(context, "incoming_sms", "from ${address ?: "unknown"} | $condensedBody")

            val react = reactContext?.get()
            if (react?.hasActiveReactInstance() == true) {
                react
                    .getJSModule(DeviceEventManagerModule.RCTDeviceEventEmitter::class.java)
                    .emit("mobileclaw_incoming_sms", payload.toString())
                return
            }

            synchronized(pendingEvents) {
                pendingEvents.add(payload)
            }
        }

        fun emitPhotoCaptured(uri: String?) {
            val payload = JSONObject()
                .put("event", "photo_captured")
                .put("uri", uri ?: "")
                .put("ts", System.currentTimeMillis())

            val context = reactContext?.get()
            if (context?.hasActiveReactInstance() == true) {
                context
                    .getJSModule(DeviceEventManagerModule.RCTDeviceEventEmitter::class.java)
                    .emit("mobileclaw_photo_captured", payload.toString())
                return
            }

            synchronized(pendingEvents) {
                pendingEvents.add(payload)
            }
        }
    }

    @ReactMethod
    fun configureRuntimeBridge(config: ReadableMap, promise: Promise) {
        try {
            val raw = JSONObject(config.toHashMap())
            RuntimeBridge.configure(appContext, raw)
            promise.resolve(JSONObject().put("ok", true).toString())
        } catch (error: Throwable) {
            promise.reject("RUNTIME_BRIDGE_CONFIG_ERROR", error.message, error)
        }
    }

    @ReactMethod
    fun getRuntimeBridgeStatus(promise: Promise) {
        try {
            promise.resolve(RuntimeBridge.bridgeStatus(appContext).toString())
        } catch (error: Throwable) {
            promise.reject("RUNTIME_BRIDGE_STATUS_ERROR", error.message, error)
        }
    }

    @ReactMethod
    fun executeToolAction(action: String, payload: ReadableMap, promise: Promise) {
        try {
            val result = when (action.trim()) {
                "launch_app" -> launchApp(payload)
                "list_apps" -> listApps()
                "open_url" -> openUrl(payload)
                "open_settings" -> openSettings()
                "sensor_read" -> sensorRead(payload)
                "get_network" -> getNetwork()
                "get_battery" -> getBattery()
                "vibrate" -> vibrate(payload)
                "post_notification" -> postNotification(payload)
                "read_notifications" -> readNotifications(payload)
                "hook_notifications" -> hookNotifications(payload)
                "get_location" -> getLocation()
                "manage_geofence" -> manageGeofence(payload)
                "place_call" -> placeCall(payload)
                "send_sms" -> sendSms(payload)
                "hook_incoming_call" -> hookIncomingCall(payload)
                "hook_incoming_sms" -> hookIncomingSms(payload)
                "take_photo" -> takePhoto(payload)
                "scan_qr" -> scanQr()
                "record_audio" -> recordAudio()
                "manage_files" -> manageFiles(payload)
                "request_all_files_access" -> requestAllFilesAccess()
                "pick_document" -> pickDocument(payload)
                "read_contacts" -> readContacts(payload)
                "read_calendar" -> readCalendar(payload)
                "set_clipboard" -> setClipboard(payload)
                "read_photos" -> readPhotos(payload)
                "read_call_log" -> readCallLog(payload)
                "read_sms" -> readSms(payload)
                "scan_bluetooth" -> scanBluetooth(payload)
                "connect_bluetooth" -> connectBluetooth(payload)
                "read_nfc" -> readNfc()
                "write_nfc" -> writeNfc(payload)
                "hardware_board_info" -> hardwareBoardInfo()
                "hardware_memory_map" -> hardwareMemoryMap()
                "hardware_memory_read" -> hardwareMemoryRead(payload)
                "ui_automation_status" -> uiAutomationStatus()
                "ui_automation_enable" -> uiAutomationEnable()
                "ui_automation_tap" -> uiAutomationTap(payload)
                "ui_automation_swipe" -> uiAutomationSwipe(payload)
                "ui_automation_click_text" -> uiAutomationClickText(payload)
                "ui_automation_back" -> uiAutomationGlobalAction(AccessibilityService.GLOBAL_ACTION_BACK, "ui_automation_back")
                "ui_automation_home" -> uiAutomationGlobalAction(AccessibilityService.GLOBAL_ACTION_HOME, "ui_automation_home")
                "ui_automation_recents" -> uiAutomationGlobalAction(AccessibilityService.GLOBAL_ACTION_RECENTS, "ui_automation_recents")
                "browser_open_session" -> browserOpenSession(payload)
                "browser_navigate" -> browserNavigate(payload)
                "browser_state" -> browserState()
                "browser_fetch_page" -> browserFetchPage(payload)
                else -> throw IllegalArgumentException("Unsupported action: $action")
            }
            promise.resolve(result.toString())
        } catch (error: Throwable) {
            promise.reject("ANDROID_AGENT_TOOL_ERROR", error.message, error)
        }
    }

    @ReactMethod
    fun consumePendingEvents(promise: Promise) {
        val events = Arguments.createArray()
        synchronized(pendingEvents) {
            for (event in pendingEvents) {
                events.pushString(event.toString())
            }
            pendingEvents.clear()
        }
        promise.resolve(events)
    }

    @ReactMethod
    fun addListener(eventName: String) {
        // Required by NativeEventEmitter in React Native.
    }

    @ReactMethod
    fun removeListeners(count: Int) {
        // Required by NativeEventEmitter in React Native.
    }

    private fun launchApp(payload: ReadableMap): JSONObject {
        val packageName = payload.getString("package")?.trim().orEmpty()
        require(packageName.isNotEmpty()) { "launch_app requires payload.package" }

        val intent = appContext.packageManager.getLaunchIntentForPackage(packageName)
            ?: throw IllegalArgumentException("Package not launchable: $packageName")
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        appContext.startActivity(intent)
        return JSONObject().put("ok", true).put("action", "launch_app").put("package", packageName)
    }

    private fun listApps(): JSONObject {
        val packages = appContext.packageManager
            .getInstalledApplications(PackageManager.GET_META_DATA)
            .asSequence()
            .map { app -> app.packageName }
            .sorted()
            .take(200)
            .toList()

        val array = JSONArray()
        for (name in packages) {
            array.put(name)
        }

        return JSONObject().put("ok", true).put("packages", array)
    }

    private fun openUrl(payload: ReadableMap): JSONObject {
        val url = payload.getString("url")?.trim().orEmpty()
        require(url.startsWith("https://")) { "open_url requires https:// URL" }
        val intent = Intent(Intent.ACTION_VIEW).apply {
            data = Uri.parse(url)
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        appContext.startActivity(intent)
        return JSONObject().put("ok", true).put("url", url)
    }

    private fun openSettings(): JSONObject {
        val intent = Intent(Settings.ACTION_SETTINGS).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        appContext.startActivity(intent)
        return JSONObject().put("ok", true).put("action", "open_settings")
    }

    private fun sensorRead(payload: ReadableMap): JSONObject {
        val target = payload.getString("sensor")?.trim().orEmpty()
        val manager = appContext.getSystemService(Context.SENSOR_SERVICE) as SensorManager
        val sensors = manager.getSensorList(Sensor.TYPE_ALL)
        val matched = sensors.firstOrNull { sensor ->
            val name = sensor.name.lowercase()
            val key = target.lowercase()
            name.contains(key)
        }

        return if (matched == null) {
            JSONObject().put("ok", false).put("error", "sensor_not_found").put("sensor", target)
        } else {
            JSONObject()
                .put("ok", true)
                .put("action", "sensor_read")
                .put("sensor", matched.name)
                .put("vendor", matched.vendor)
                .put("type", matched.stringType)
        }
    }

    private fun getNetwork(): JSONObject {
        val manager = appContext.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
        val network = manager.activeNetwork
        val capabilities = manager.getNetworkCapabilities(network)
        return JSONObject()
            .put("ok", true)
            .put("wifi", capabilities?.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) == true)
            .put("cellular", capabilities?.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) == true)
            .put("validated", capabilities?.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED) == true)
    }

    private fun getLocation(): JSONObject {
        val fineGranted = ContextCompat.checkSelfPermission(appContext, android.Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED
        val coarseGranted = ContextCompat.checkSelfPermission(appContext, android.Manifest.permission.ACCESS_COARSE_LOCATION) == PackageManager.PERMISSION_GRANTED
        if (!fineGranted && !coarseGranted) {
            throw IllegalStateException("location_permission_required")
        }

        val manager = appContext.getSystemService(Context.LOCATION_SERVICE) as LocationManager
        val providers = manager.getProviders(true)
        var best: Location? = null
        for (provider in providers) {
            val loc = try {
                manager.getLastKnownLocation(provider)
            } catch (_: SecurityException) {
                null
            }
            if (loc != null && (best == null || loc.time > best!!.time)) {
                best = loc
            }
        }

        if (best == null) {
            return JSONObject()
                .put("ok", false)
                .put("action", "get_location")
                .put("error", "no_location_fix")
        }

        return JSONObject()
            .put("ok", true)
            .put("action", "get_location")
            .put("latitude", best.latitude)
            .put("longitude", best.longitude)
            .put("accuracy_m", best.accuracy.toDouble())
            .put("provider", best.provider ?: "")
            .put("ts", best.time)
    }

    private fun getBattery(): JSONObject {
        val filter = IntentFilter(Intent.ACTION_BATTERY_CHANGED)
        val battery = appContext.registerReceiver(null, filter)
        val level = battery?.getIntExtra(BatteryManager.EXTRA_LEVEL, -1) ?: -1
        val scale = battery?.getIntExtra(BatteryManager.EXTRA_SCALE, 100) ?: 100
        val status = battery?.getIntExtra(BatteryManager.EXTRA_STATUS, BatteryManager.BATTERY_STATUS_UNKNOWN)
            ?: BatteryManager.BATTERY_STATUS_UNKNOWN
        val charging = status == BatteryManager.BATTERY_STATUS_CHARGING || status == BatteryManager.BATTERY_STATUS_FULL

        val percent = if (level >= 0 && scale > 0) (level * 100.0 / scale) else -1.0

        return JSONObject()
            .put("ok", true)
            .put("level_percent", percent)
            .put("charging", charging)
    }

    private fun vibrate(payload: ReadableMap): JSONObject {
        val durationMs = if (payload.hasKey("duration_ms")) payload.getInt("duration_ms") else 400
        val clamped = durationMs.coerceIn(50, 10_000)
        val vibrator = appContext.getSystemService(Context.VIBRATOR_SERVICE) as Vibrator
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            vibrator.vibrate(VibrationEffect.createOneShot(clamped.toLong(), VibrationEffect.DEFAULT_AMPLITUDE))
        } else {
            @Suppress("DEPRECATION")
            vibrator.vibrate(clamped.toLong())
        }
        return JSONObject().put("ok", true).put("duration_ms", clamped)
    }

    private fun postNotification(payload: ReadableMap): JSONObject {
        val title = payload.getString("title")?.ifBlank { "MobileClaw" } ?: "MobileClaw"
        val text = payload.getString("text")?.trim().orEmpty()
        require(text.isNotEmpty()) { "post_notification requires payload.text" }

        val manager = appContext.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        val channelId = "mobileclaw-agent"
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(channelId, "MobileClaw Agent", NotificationManager.IMPORTANCE_DEFAULT)
            manager.createNotificationChannel(channel)
        }

        val notification = NotificationCompat.Builder(appContext, channelId)
            .setContentTitle(title)
            .setContentText(text)
            .setSmallIcon(R.mipmap.ic_launcher)
            .setAutoCancel(true)
            .build()

        val id = (System.currentTimeMillis() % Int.MAX_VALUE).toInt()
        manager.notify(id, notification)
        return JSONObject().put("ok", true).put("notification_id", id)
    }

    private fun readNotifications(payload: ReadableMap): JSONObject {
        val manager = appContext.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        val max = (if (payload.hasKey("limit")) payload.getInt("limit") else 30).coerceIn(1, 200)
        val entries = JSONArray()

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
            val active = manager.activeNotifications
            for (status in active.take(max)) {
                val n = status.notification
                val extras = n.extras
                val title = extras?.getCharSequence(NotificationCompat.EXTRA_TITLE)?.toString().orEmpty()
                val text = extras?.getCharSequence(NotificationCompat.EXTRA_TEXT)?.toString().orEmpty()
                entries.put(
                    JSONObject()
                        .put("package", status.packageName)
                        .put("id", status.id)
                        .put("title", title)
                        .put("text", text)
                        .put("post_time", status.postTime),
                )
            }
        }

        return JSONObject()
            .put("ok", true)
            .put("action", "read_notifications")
            .put("entry_count", entries.length())
            .put("entries", entries)
    }

    private fun hookNotifications(payload: ReadableMap): JSONObject {
        val enabled = payload.hasKey("enabled") && payload.getBoolean("enabled")
        if (enabled) {
            val intent = Intent(Settings.ACTION_NOTIFICATION_LISTENER_SETTINGS).apply {
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            appContext.startActivity(intent)
        }
        return JSONObject()
            .put("ok", true)
            .put("action", "hook_notifications")
            .put("enabled", enabled)
            .put("note", "Notification listener settings opened; explicit user grant is required")
    }

    private fun manageGeofence(payload: ReadableMap): JSONObject {
        val enabled = payload.hasKey("enabled") && payload.getBoolean("enabled")
        val radius = (if (payload.hasKey("radius_m")) payload.getInt("radius_m") else 200).coerceIn(50, 5000)
        val latitude = if (payload.hasKey("latitude")) payload.getDouble("latitude") else 0.0
        val longitude = if (payload.hasKey("longitude")) payload.getDouble("longitude") else 0.0
        prefs.edit()
            .putBoolean("geofence_enabled", enabled)
            .putInt("geofence_radius", radius)
            .putFloat("geofence_lat", latitude.toFloat())
            .putFloat("geofence_lng", longitude.toFloat())
            .apply()

        return JSONObject()
            .put("ok", true)
            .put("action", "manage_geofence")
            .put("enabled", enabled)
            .put("latitude", latitude)
            .put("longitude", longitude)
            .put("radius_m", radius)
            .put("note", "Geofence config stored locally for runtime checks")
    }

    private fun hookIncomingSms(payload: ReadableMap): JSONObject {
        val enabled = payload.hasKey("enabled") && payload.getBoolean("enabled")
        prefs.edit().putBoolean("sms_hook_enabled", enabled).apply()
        return JSONObject()
            .put("ok", true)
            .put("action", "hook_incoming_sms")
            .put("enabled", enabled)
            .put("note", "SMS receiver is manifest-registered; toggle is enforced in app policy")
    }

    private fun takePhoto(payload: ReadableMap): JSONObject {
        val direct = !payload.hasKey("direct") || payload.getBoolean("direct")
        val lens = payload.getString("lens")?.trim().orEmpty().ifEmpty { "rear" }

        if (!direct) {
            val intent = Intent(MediaStore.ACTION_IMAGE_CAPTURE).apply {
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            appContext.startActivity(intent)
            return JSONObject()
                .put("ok", true)
                .put("action", "take_photo")
                .put("mode", "intent")
                .put("note", "Camera app launched")
        }

        val permission = ContextCompat.checkSelfPermission(appContext, android.Manifest.permission.CAMERA)
        if (permission != PackageManager.PERMISSION_GRANTED) {
            throw IllegalStateException("camera_permission_required")
        }

        val intent = Intent(appContext, DirectPhotoCaptureActivity::class.java).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            putExtra("lens", lens)
        }
        appContext.startActivity(intent)
        return JSONObject()
            .put("ok", true)
            .put("action", "take_photo")
            .put("mode", "direct")
            .put("lens", lens)
            .put("note", "Direct capture started")
    }

    private fun scanQr(): JSONObject {
        val intent = Intent(Intent.ACTION_VIEW, Uri.parse("zxing://scan/")).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        return try {
            appContext.startActivity(intent)
            JSONObject()
                .put("ok", true)
                .put("action", "scan_qr")
                .put("note", "QR scanner intent launched")
        } catch (_: Throwable) {
            val fallback = Intent(MediaStore.ACTION_IMAGE_CAPTURE).apply {
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            appContext.startActivity(fallback)
            JSONObject()
                .put("ok", true)
                .put("action", "scan_qr")
                .put("note", "Scanner app unavailable; camera launched as fallback")
        }
    }

    private fun recordAudio(): JSONObject {
        val intent = Intent(MediaStore.Audio.Media.RECORD_SOUND_ACTION).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        appContext.startActivity(intent)
        return JSONObject().put("ok", true).put("action", "record_audio").put("note", "Sound recorder launched")
    }

    private fun pickDocument(payload: ReadableMap): JSONObject {
        val mime = payload.getString("mime")?.trim().orEmpty().ifEmpty { "*/*" }
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = mime
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        appContext.startActivity(intent)
        return JSONObject()
            .put("ok", true)
            .put("action", "pick_document")
            .put("mime", mime)
            .put("note", "Document picker launched")
    }

    private fun placeCall(payload: ReadableMap): JSONObject {
        val number = payload.getString("to")?.trim().orEmpty()
        require(number.isNotEmpty()) { "place_call requires payload.to" }
        val direct = !payload.hasKey("direct") || payload.getBoolean("direct")

        val callIntent = Intent(if (direct) Intent.ACTION_CALL else Intent.ACTION_DIAL).apply {
            data = Uri.parse("tel:$number")
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }

        if (direct) {
            val permission = ContextCompat.checkSelfPermission(appContext, android.Manifest.permission.CALL_PHONE)
            if (permission != PackageManager.PERMISSION_GRANTED) {
                throw IllegalStateException("call_permission_required")
            }
        }

        appContext.startActivity(callIntent)
        return JSONObject()
            .put("ok", true)
            .put("action", "place_call")
            .put("mode", if (direct) "direct" else "intent")
    }

    private fun sendSms(payload: ReadableMap): JSONObject {
        val to = payload.getString("to")?.trim().orEmpty()
        val body = payload.getString("body")?.trim().orEmpty()
        require(to.isNotEmpty()) { "send_sms requires payload.to" }
        require(body.isNotEmpty()) { "send_sms requires payload.body" }

        val direct = !payload.hasKey("direct") || payload.getBoolean("direct")
        if (direct) {
            val permission = ContextCompat.checkSelfPermission(appContext, android.Manifest.permission.SEND_SMS)
            if (permission != PackageManager.PERMISSION_GRANTED) {
                throw IllegalStateException("sms_permission_required")
            }

            val smsManager = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                appContext.getSystemService(SmsManager::class.java)
            } else {
                @Suppress("DEPRECATION")
                SmsManager.getDefault()
            }

            val parts = smsManager.divideMessage(body)
            if (parts.size > 1) {
                smsManager.sendMultipartTextMessage(to, null, parts, null, null)
            } else {
                smsManager.sendTextMessage(to, null, body, null, null)
            }

            return JSONObject()
                .put("ok", true)
                .put("action", "send_sms")
                .put("mode", "direct")
                .put("parts", parts.size)
        }

        val intent = Intent(Intent.ACTION_SENDTO).apply {
            data = Uri.parse("smsto:$to")
            putExtra("sms_body", body)
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        appContext.startActivity(intent)
        return JSONObject().put("ok", true).put("action", "send_sms").put("mode", "intent")
    }

    private fun hookIncomingCall(payload: ReadableMap): JSONObject {
        val enabled = payload.hasKey("enabled") && payload.getBoolean("enabled")
        return JSONObject()
            .put("ok", true)
            .put("action", "hook_incoming_call")
            .put("enabled", enabled)
            .put("note", "Receiver is manifest-registered; toggle is enforced in JS security policy")
    }

    private fun setClipboard(payload: ReadableMap): JSONObject {
        val text = payload.getString("text")?.trim().orEmpty()
        require(text.isNotEmpty()) { "set_clipboard requires payload.text" }

        val clipboard = appContext.getSystemService(Context.CLIPBOARD_SERVICE) as android.content.ClipboardManager
        val clip = ClipData.newPlainText("mobileclaw", text)
        clipboard.setPrimaryClip(clip)
        return JSONObject().put("ok", true).put("action", "set_clipboard")
    }

    private fun readCallLog(payload: ReadableMap): JSONObject {
        val limit = (if (payload.hasKey("limit")) payload.getInt("limit") else 30).coerceIn(1, 200)
        val cursor = appContext.contentResolver.query(
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
                val type = if (typeIdx >= 0) it.getInt(typeIdx) else 0
                val direction = when (type) {
                    CallLog.Calls.INCOMING_TYPE -> "incoming"
                    CallLog.Calls.OUTGOING_TYPE -> "outgoing"
                    CallLog.Calls.MISSED_TYPE -> "missed"
                    CallLog.Calls.REJECTED_TYPE -> "rejected"
                    CallLog.Calls.BLOCKED_TYPE -> "blocked"
                    CallLog.Calls.VOICEMAIL_TYPE -> "voicemail"
                    else -> "unknown"
                }

                val entry = JSONObject()
                    .put("number", if (numberIdx >= 0) it.getString(numberIdx) ?: "" else "")
                    .put("name", if (nameIdx >= 0) it.getString(nameIdx) ?: "" else "")
                    .put("direction", direction)
                    .put("ts", if (dateIdx >= 0) it.getLong(dateIdx) else 0L)
                    .put("duration_seconds", if (durationIdx >= 0) it.getLong(durationIdx) else 0L)
                entries.put(entry)
                count += 1
            }
        }

        return JSONObject()
            .put("ok", true)
            .put("action", "read_call_log")
            .put("entry_count", entries.length())
            .put("entries", entries)
    }

    private fun readSms(payload: ReadableMap): JSONObject {
        val limit = (if (payload.hasKey("limit")) payload.getInt("limit") else 30).coerceIn(1, 200)
        val cursor = appContext.contentResolver.query(
            Telephony.Sms.Inbox.CONTENT_URI,
            arrayOf(
                Telephony.Sms.ADDRESS,
                Telephony.Sms.BODY,
                Telephony.Sms.DATE,
                Telephony.Sms.TYPE,
            ),
            null,
            null,
            "${Telephony.Sms.DATE} DESC",
        )

        val entries = JSONArray()
        cursor?.use {
            val addressIdx = it.getColumnIndex(Telephony.Sms.ADDRESS)
            val bodyIdx = it.getColumnIndex(Telephony.Sms.BODY)
            val dateIdx = it.getColumnIndex(Telephony.Sms.DATE)
            val typeIdx = it.getColumnIndex(Telephony.Sms.TYPE)
            var count = 0
            while (it.moveToNext() && count < limit) {
                val smsType = if (typeIdx >= 0) it.getInt(typeIdx) else 0
                val direction = when (smsType) {
                    Telephony.Sms.MESSAGE_TYPE_INBOX -> "inbox"
                    Telephony.Sms.MESSAGE_TYPE_SENT -> "sent"
                    Telephony.Sms.MESSAGE_TYPE_DRAFT -> "draft"
                    else -> "other"
                }

                val entry = JSONObject()
                    .put("address", if (addressIdx >= 0) it.getString(addressIdx) ?: "" else "")
                    .put("body", if (bodyIdx >= 0) it.getString(bodyIdx) ?: "" else "")
                    .put("ts", if (dateIdx >= 0) it.getLong(dateIdx) else 0L)
                    .put("direction", direction)
                entries.put(entry)
                count += 1
            }
        }

        return JSONObject()
            .put("ok", true)
            .put("action", "read_sms")
            .put("entry_count", entries.length())
            .put("entries", entries)
    }

    private fun readPhotos(payload: ReadableMap): JSONObject {
        val limit = (if (payload.hasKey("limit")) payload.getInt("limit") else 50).coerceIn(1, 300)
        val projection = arrayOf(
            MediaStore.Images.Media._ID,
            MediaStore.Images.Media.DISPLAY_NAME,
            MediaStore.Images.Media.DATE_MODIFIED,
            MediaStore.Images.Media.SIZE,
        )
        val cursor = appContext.contentResolver.query(
            MediaStore.Images.Media.EXTERNAL_CONTENT_URI,
            projection,
            null,
            null,
            "${MediaStore.Images.Media.DATE_MODIFIED} DESC",
        )

        val entries = JSONArray()
        cursor?.use {
            val idIdx = it.getColumnIndex(MediaStore.Images.Media._ID)
            val nameIdx = it.getColumnIndex(MediaStore.Images.Media.DISPLAY_NAME)
            val dateIdx = it.getColumnIndex(MediaStore.Images.Media.DATE_MODIFIED)
            val sizeIdx = it.getColumnIndex(MediaStore.Images.Media.SIZE)
            var count = 0
            while (it.moveToNext() && count < limit) {
                val id = if (idIdx >= 0) it.getLong(idIdx) else 0L
                val uri = Uri.withAppendedPath(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, id.toString())
                entries.put(
                    JSONObject()
                        .put("id", id)
                        .put("name", if (nameIdx >= 0) it.getString(nameIdx) ?: "" else "")
                        .put("uri", uri.toString())
                        .put("modified_s", if (dateIdx >= 0) it.getLong(dateIdx) else 0L)
                        .put("size_bytes", if (sizeIdx >= 0) it.getLong(sizeIdx) else 0L),
                )
                count += 1
            }
        }

        return JSONObject()
            .put("ok", true)
            .put("action", "read_photos")
            .put("entry_count", entries.length())
            .put("entries", entries)
    }

    private fun scanBluetooth(payload: ReadableMap): JSONObject {
        val adapter = BluetoothAdapter.getDefaultAdapter()
            ?: return JSONObject().put("ok", false).put("action", "scan_bluetooth").put("error", "bluetooth_not_available")

        if (!adapter.isEnabled) {
            return JSONObject().put("ok", false).put("action", "scan_bluetooth").put("error", "bluetooth_disabled")
        }

        val bonded = JSONArray()
        for (device in adapter.bondedDevices) {
            bonded.put(
                JSONObject()
                    .put("name", device.name ?: "")
                    .put("address", device.address ?: "")
                    .put("type", device.type),
            )
        }

        val startDiscovery = payload.hasKey("start_discovery") && payload.getBoolean("start_discovery")
        if (startDiscovery) {
            adapter.startDiscovery()
        }

        return JSONObject()
            .put("ok", true)
            .put("action", "scan_bluetooth")
            .put("discovering", adapter.isDiscovering)
            .put("bonded_count", bonded.length())
            .put("bonded", bonded)
    }

    private fun connectBluetooth(payload: ReadableMap): JSONObject {
        val address = payload.getString("address")?.trim().orEmpty()
        val adapter = BluetoothAdapter.getDefaultAdapter()
            ?: return JSONObject().put("ok", false).put("action", "connect_bluetooth").put("error", "bluetooth_not_available")
        if (!adapter.isEnabled) {
            return JSONObject().put("ok", false).put("action", "connect_bluetooth").put("error", "bluetooth_disabled")
        }

        if (address.isBlank()) {
            val intent = Intent(Settings.ACTION_BLUETOOTH_SETTINGS).apply { addFlags(Intent.FLAG_ACTIVITY_NEW_TASK) }
            appContext.startActivity(intent)
            return JSONObject().put("ok", true).put("action", "connect_bluetooth").put("note", "Bluetooth settings opened")
        }

        val device: BluetoothDevice? = try {
            adapter.getRemoteDevice(address)
        } catch (_: Throwable) {
            null
        }

        return if (device == null) {
            JSONObject().put("ok", false).put("action", "connect_bluetooth").put("error", "invalid_device_address")
        } else {
            val intent = Intent(Settings.ACTION_BLUETOOTH_SETTINGS).apply { addFlags(Intent.FLAG_ACTIVITY_NEW_TASK) }
            appContext.startActivity(intent)
            JSONObject()
                .put("ok", true)
                .put("action", "connect_bluetooth")
                .put("address", address)
                .put("name", device.name ?: "")
                .put("note", "Bluetooth settings opened for manual connect")
        }
    }

    private fun readNfc(): JSONObject {
        val intent = Intent(Settings.ACTION_NFC_SETTINGS).apply { addFlags(Intent.FLAG_ACTIVITY_NEW_TASK) }
        appContext.startActivity(intent)
        return JSONObject().put("ok", true).put("action", "read_nfc").put("note", "NFC settings opened")
    }

    private fun writeNfc(payload: ReadableMap): JSONObject {
        val text = payload.getString("text")?.trim().orEmpty()
        val intent = Intent(Settings.ACTION_NFC_SETTINGS).apply { addFlags(Intent.FLAG_ACTIVITY_NEW_TASK) }
        appContext.startActivity(intent)
        return JSONObject()
            .put("ok", true)
            .put("action", "write_nfc")
            .put("text", text)
            .put("note", "NFC settings opened; tap-based write requires dedicated foreground flow")
    }

    private fun hardwareMemoryRead(payload: ReadableMap): JSONObject {
        val key = payload.getString("key")?.trim().orEmpty().ifEmpty { "MemAvailable" }
        val memInfo = File("/proc/meminfo")
        if (!memInfo.exists()) {
            return JSONObject().put("ok", false).put("action", "hardware_memory_read").put("error", "meminfo_unavailable")
        }

        val line = memInfo.useLines { lines ->
            lines.firstOrNull { entry -> entry.startsWith("$key:") }
        }

        return JSONObject()
            .put("ok", line != null)
            .put("action", "hardware_memory_read")
            .put("key", key)
            .put("value", line ?: "")
            .put("source", "/proc/meminfo")
    }

    private fun uiAutomationStatus(): JSONObject {
        val enabled = isAccessibilityServiceEnabled()
        val connected = AgentAccessibilityService.instance != null
        return JSONObject()
            .put("ok", true)
            .put("action", "ui_automation_status")
            .put("enabled", enabled)
            .put("connected", connected)
    }

    private fun uiAutomationEnable(): JSONObject {
        val intent = Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        appContext.startActivity(intent)
        return JSONObject()
            .put("ok", true)
            .put("action", "ui_automation_enable")
            .put("note", "Enable MobileClaw accessibility service in system settings")
    }

    private fun uiAutomationTap(payload: ReadableMap): JSONObject {
        val x = if (payload.hasKey("x")) payload.getDouble("x").toFloat() else 0f
        val y = if (payload.hasKey("y")) payload.getDouble("y").toFloat() else 0f
        val duration = if (payload.hasKey("duration_ms")) payload.getInt("duration_ms").toLong() else 80L
        val service = AgentAccessibilityService.instance
            ?: return JSONObject().put("ok", false).put("action", "ui_automation_tap").put("error", "accessibility_service_not_connected")
        val result = service.tap(x, y, duration)
        return JSONObject().put("ok", result).put("action", "ui_automation_tap").put("x", x).put("y", y)
    }

    private fun uiAutomationSwipe(payload: ReadableMap): JSONObject {
        val x1 = if (payload.hasKey("x1")) payload.getDouble("x1").toFloat() else 0f
        val y1 = if (payload.hasKey("y1")) payload.getDouble("y1").toFloat() else 0f
        val x2 = if (payload.hasKey("x2")) payload.getDouble("x2").toFloat() else 0f
        val y2 = if (payload.hasKey("y2")) payload.getDouble("y2").toFloat() else 0f
        val duration = if (payload.hasKey("duration_ms")) payload.getInt("duration_ms").toLong() else 300L
        val service = AgentAccessibilityService.instance
            ?: return JSONObject().put("ok", false).put("action", "ui_automation_swipe").put("error", "accessibility_service_not_connected")
        val result = service.swipe(x1, y1, x2, y2, duration)
        return JSONObject().put("ok", result).put("action", "ui_automation_swipe")
    }

    private fun uiAutomationClickText(payload: ReadableMap): JSONObject {
        val text = payload.getString("text")?.trim().orEmpty()
        if (text.isBlank()) {
            return JSONObject().put("ok", false).put("action", "ui_automation_click_text").put("error", "text_required")
        }
        val service = AgentAccessibilityService.instance
            ?: return JSONObject().put("ok", false).put("action", "ui_automation_click_text").put("error", "accessibility_service_not_connected")
        val result = service.clickByText(text)
        return JSONObject().put("ok", result).put("action", "ui_automation_click_text").put("text", text)
    }

    private fun uiAutomationGlobalAction(action: Int, actionName: String): JSONObject {
        val service = AgentAccessibilityService.instance
            ?: return JSONObject().put("ok", false).put("action", actionName).put("error", "accessibility_service_not_connected")
        val result = service.performGlobalAction(action)
        return JSONObject().put("ok", result).put("action", actionName)
    }

    private fun browserOpenSession(payload: ReadableMap): JSONObject {
        val url = payload.getString("url")?.trim().orEmpty().ifBlank { "https://www.google.com" }
        val intent = Intent(appContext, AgentBrowserActivity::class.java).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            putExtra("url", url)
        }
        appContext.startActivity(intent)
        return JSONObject().put("ok", true).put("action", "browser_open_session").put("url", url)
    }

    private fun browserNavigate(payload: ReadableMap): JSONObject {
        val url = payload.getString("url")?.trim().orEmpty()
        if (url.isBlank()) {
            return JSONObject().put("ok", false).put("action", "browser_navigate").put("error", "url_required")
        }
        val navigated = AgentBrowserActivity.navigate(url)
        return JSONObject()
            .put("ok", navigated)
            .put("action", "browser_navigate")
            .put("url", url)
            .put("note", if (navigated) "session_updated" else "browser_session_not_running")
    }

    private fun browserState(): JSONObject {
        val state = AgentBrowserActivity.state()
        return JSONObject()
            .put("ok", AgentBrowserActivity.hasSession())
            .put("action", "browser_state")
            .put("url", state["url"] ?: "")
            .put("title", state["title"] ?: "")
    }

    private fun browserFetchPage(payload: ReadableMap): JSONObject {
        val url = payload.getString("url")?.trim().orEmpty()
        if (url.isBlank()) {
            return JSONObject().put("ok", false).put("action", "browser_fetch_page").put("error", "url_required")
        }
        val max = (if (payload.hasKey("max_chars")) payload.getInt("max_chars") else 10000).coerceIn(200, 50000)
        val fetched = AgentBrowserActivity.fetchPage(url, max)
        return JSONObject()
            .put("ok", true)
            .put("action", "browser_fetch_page")
            .put("url", url)
            .put("status", fetched["status"] ?: "")
            .put("content_type", fetched["content_type"] ?: "")
            .put("body", fetched["body"] ?: "")
    }

    private fun isAccessibilityServiceEnabled(): Boolean {
        val enabledServices = Settings.Secure.getString(appContext.contentResolver, Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES)
            ?: return false
        val target = ComponentName(appContext, AgentAccessibilityService::class.java).flattenToString()
        return enabledServices.split(':').any { service -> service.equals(target, ignoreCase = true) }
    }

    private fun readContacts(payload: ReadableMap): JSONObject {
        val granted = ContextCompat.checkSelfPermission(appContext, android.Manifest.permission.READ_CONTACTS) == PackageManager.PERMISSION_GRANTED
        if (!granted) {
            throw IllegalStateException("contacts_permission_required")
        }

        val limit = (if (payload.hasKey("limit")) payload.getInt("limit") else 100).coerceIn(1, 500)
        val cursor = appContext.contentResolver.query(
            ContactsContract.CommonDataKinds.Phone.CONTENT_URI,
            arrayOf(
                ContactsContract.CommonDataKinds.Phone.DISPLAY_NAME,
                ContactsContract.CommonDataKinds.Phone.NUMBER,
                ContactsContract.CommonDataKinds.Phone.TYPE,
            ),
            null,
            null,
            "${ContactsContract.CommonDataKinds.Phone.DISPLAY_NAME} ASC",
        )

        val entries = JSONArray()
        cursor?.use {
            val nameIdx = it.getColumnIndex(ContactsContract.CommonDataKinds.Phone.DISPLAY_NAME)
            val numberIdx = it.getColumnIndex(ContactsContract.CommonDataKinds.Phone.NUMBER)
            val typeIdx = it.getColumnIndex(ContactsContract.CommonDataKinds.Phone.TYPE)
            var count = 0
            while (it.moveToNext() && count < limit) {
                val entry = JSONObject()
                    .put("name", if (nameIdx >= 0) it.getString(nameIdx) ?: "" else "")
                    .put("number", if (numberIdx >= 0) it.getString(numberIdx) ?: "" else "")
                    .put("phone_type", if (typeIdx >= 0) it.getInt(typeIdx) else 0)
                entries.put(entry)
                count += 1
            }
        }

        return JSONObject()
            .put("ok", true)
            .put("action", "read_contacts")
            .put("entry_count", entries.length())
            .put("entries", entries)
    }

    private fun readCalendar(payload: ReadableMap): JSONObject {
        val granted = ContextCompat.checkSelfPermission(appContext, android.Manifest.permission.READ_CALENDAR) == PackageManager.PERMISSION_GRANTED
        if (!granted) {
            throw IllegalStateException("calendar_permission_required")
        }

        val limit = (if (payload.hasKey("limit")) payload.getInt("limit") else 50).coerceIn(1, 200)
        val cursor = appContext.contentResolver.query(
            CalendarContract.Events.CONTENT_URI,
            arrayOf(
                CalendarContract.Events.TITLE,
                CalendarContract.Events.DESCRIPTION,
                CalendarContract.Events.DTSTART,
                CalendarContract.Events.DTEND,
                CalendarContract.Events.EVENT_LOCATION,
            ),
            null,
            null,
            "${CalendarContract.Events.DTSTART} DESC",
        )

        val entries = JSONArray()
        cursor?.use {
            val titleIdx = it.getColumnIndex(CalendarContract.Events.TITLE)
            val descIdx = it.getColumnIndex(CalendarContract.Events.DESCRIPTION)
            val startIdx = it.getColumnIndex(CalendarContract.Events.DTSTART)
            val endIdx = it.getColumnIndex(CalendarContract.Events.DTEND)
            val locIdx = it.getColumnIndex(CalendarContract.Events.EVENT_LOCATION)
            var count = 0
            while (it.moveToNext() && count < limit) {
                val entry = JSONObject()
                    .put("title", if (titleIdx >= 0) it.getString(titleIdx) ?: "" else "")
                    .put("description", if (descIdx >= 0) it.getString(descIdx) ?: "" else "")
                    .put("start_ts", if (startIdx >= 0) it.getLong(startIdx) else 0L)
                    .put("end_ts", if (endIdx >= 0) it.getLong(endIdx) else 0L)
                    .put("location", if (locIdx >= 0) it.getString(locIdx) ?: "" else "")
                entries.put(entry)
                count += 1
            }
        }

        return JSONObject()
            .put("ok", true)
            .put("action", "read_calendar")
            .put("entry_count", entries.length())
            .put("entries", entries)
    }

    private fun hardwareBoardInfo(): JSONObject {
        return JSONObject()
            .put("ok", true)
            .put("action", "hardware_board_info")
            .put("manufacturer", Build.MANUFACTURER)
            .put("brand", Build.BRAND)
            .put("model", Build.MODEL)
            .put("device", Build.DEVICE)
            .put("board", Build.BOARD)
            .put("hardware", Build.HARDWARE)
            .put("product", Build.PRODUCT)
            .put("sdk_int", Build.VERSION.SDK_INT)
            .put("release", Build.VERSION.RELEASE)
    }

    private fun hardwareMemoryMap(): JSONObject {
        val runtime = Runtime.getRuntime()
        val activityManager = appContext.getSystemService(Context.ACTIVITY_SERVICE) as android.app.ActivityManager
        val memInfo = android.app.ActivityManager.MemoryInfo()
        activityManager.getMemoryInfo(memInfo)

        return JSONObject()
            .put("ok", true)
            .put("action", "hardware_memory_map")
            .put("runtime_max_bytes", runtime.maxMemory())
            .put("runtime_total_bytes", runtime.totalMemory())
            .put("runtime_free_bytes", runtime.freeMemory())
            .put("system_total_bytes", memInfo.totalMem)
            .put("system_available_bytes", memInfo.availMem)
            .put("low_memory", memInfo.lowMemory)
            .put("threshold_bytes", memInfo.threshold)
    }

    private fun manageFiles(payload: ReadableMap): JSONObject {
        val scope = payload.getString("scope")?.trim()?.ifEmpty { "app_external" } ?: "app_external"
        val relativePath = payload.getString("path")?.trim().orEmpty()
        val maxEntries = (if (payload.hasKey("limit")) payload.getInt("limit") else 200).coerceIn(1, 1000)

        val root = resolveFileScope(scope)
        val target = if (relativePath.isBlank()) {
            root
        } else {
            File(root, relativePath)
        }

        val rootCanonical = root.canonicalFile
        val targetCanonical = target.canonicalFile
        if (!targetCanonical.path.startsWith(rootCanonical.path)) {
            throw IllegalArgumentException("Path escapes allowed scope")
        }

        if (!targetCanonical.exists()) {
            return JSONObject()
                .put("ok", false)
                .put("action", "manage_files")
                .put("scope", scope)
                .put("path", targetCanonical.absolutePath)
                .put("error", "path_not_found")
        }

        if (!targetCanonical.isDirectory) {
            return JSONObject()
                .put("ok", true)
                .put("action", "manage_files")
                .put("scope", scope)
                .put("path", targetCanonical.absolutePath)
                .put("kind", "file")
                .put("size_bytes", targetCanonical.length())
                .put("last_modified_ms", targetCanonical.lastModified())
        }

        val entries = JSONArray()
        val files = targetCanonical.listFiles().orEmpty()
            .sortedBy { file -> file.name.lowercase() }
            .take(maxEntries)

        for (file in files) {
            val entry = JSONObject()
                .put("name", file.name)
                .put("path", file.absolutePath)
                .put("is_dir", file.isDirectory)
                .put("size_bytes", if (file.isFile) file.length() else -1)
                .put("last_modified_ms", file.lastModified())
            entries.put(entry)
        }

        return JSONObject()
            .put("ok", true)
            .put("action", "manage_files")
            .put("scope", scope)
            .put("path", targetCanonical.absolutePath)
            .put("all_files_access", hasAllFilesAccess())
            .put("entry_count", entries.length())
            .put("entries", entries)
    }

    private fun requestAllFilesAccess(): JSONObject {
        val granted = hasAllFilesAccess()
        if (!granted && Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            val intent = Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION).apply {
                data = Uri.parse("package:${appContext.packageName}")
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            appContext.startActivity(intent)
            return JSONObject()
                .put("ok", true)
                .put("action", "request_all_files_access")
                .put("granted", false)
                .put("prompted", true)
        }

        return JSONObject()
            .put("ok", true)
            .put("action", "request_all_files_access")
            .put("granted", granted)
            .put("prompted", false)
    }

    private fun hasAllFilesAccess(): Boolean {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            Environment.isExternalStorageManager()
        } else {
            true
        }
    }

    private fun resolveFileScope(scope: String): File {
        return when (scope.lowercase()) {
            "app_files" -> appContext.filesDir
            "app_cache" -> appContext.cacheDir
            "app_external" -> appContext.getExternalFilesDir(null) ?: appContext.filesDir
            "downloads" -> Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS)
            "documents" -> Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOCUMENTS)
            "user" -> Environment.getExternalStorageDirectory()
            else -> throw IllegalArgumentException("Unsupported file scope: $scope")
        }
    }

    private fun unsupported(action: String): JSONObject {
        return JSONObject()
            .put("ok", false)
            .put("action", action)
            .put("error", "not_implemented_in_android_native_bridge")
    }
}
