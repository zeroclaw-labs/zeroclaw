package org.zerodroid.bridge

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import java.io.File

internal object BridgeForegroundPolicy {
    fun serviceType(apiLevel: Int): Int =
        if (apiLevel >= 34) ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE else 0
}

/**
 * Foreground service that runs the whole on-device stack:
 *  - the rust zeroclaw gateway as a supervised child process (the agent + its HTTP/WS gateway), and
 *  - the in-process NanoHTTPD Android-permission bridge on 127.0.0.1:8470.
 *
 * The APK holds the Android permissions; the bridge exposes them to the agent over loopback.
 * foregroundServiceType = specialUse (a persistent local agent server) — NOT dataSync, which the
 * platform time-limits on API 34+. BLE/USB APIs retain their own runtime permissions; the agent
 * supervisor itself is not a connected-device session and must not require one before startup.
 */
class BridgeService : Service() {

    private lateinit var rt: NativeRuntime
    private var cfg: ConfigStore? = null         // built off-main on the lifecycle thread (keystore)
    private var bridge: ApiServer? = null
    private var uiSocket: UiSocketServer? = null   // UDS ui.sock bridge to the a11y service
    private var gateway: GatewayProcess? = null
    private var sshd: SshdServer? = null

    // All lifecycle work (start/stop) is serialized on ONE thread, so doStart and doStop never
    // overlap — no race where a stop mid-startup leaves a gateway running. desiredRunning lets a
    // queued stop short-circuit an in-flight start.
    private val lifecycle = java.util.concurrent.Executors.newSingleThreadExecutor { r ->
        Thread(r, "zc-lifecycle").apply { isDaemon = true }
    }
    @Volatile private var desiredRunning = false
    @Volatile private var infoEpoch = 0          // bumped on every stop; guards stale pair-info writes

    override fun onCreate() {
        super.onCreate()
        rt = NativeRuntime(applicationContext)
        createChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> { desiredRunning = false; infoEpoch++; lifecycle.submit { doStop() }; return START_NOT_STICKY }
            ACTION_RESTART -> {
                goForeground("restarting…")
                desiredRunning = true; infoEpoch++
                lifecycle.submit { sshd?.stop(); sshd = null; gateway?.stop(); gateway = null; doStart() }   // rebind
            }
            else -> {
                goForeground("starting…")        // FGS must enter foreground promptly (main thread)
                desiredRunning = true
                lifecycle.submit { doStart() }
            }
        }
        return START_STICKY
    }

    private fun doStart() {
        try {
            if (!desiredRunning) return
            val c = cfg ?: ConfigStore(applicationContext).also { cfg = it }   // off-main keystore
            rt.prepare(c.agentAlias)
            // In manual mode the user owns config.toml — don't overwrite their edits.
            if (!c.manualConfig || !rt.configFile.exists()) c.writeConfig(rt)
            if (!desiredRunning) return
            if (bridge == null) {
                val server = ApiServer(applicationContext, ApiServer.PORT)
                server.start()
                bridge = server
                rt.publishBridgeToken(server.bridgeToken)
            }
            if (uiSocket == null) {
                // UDS UI bridge (filesDir/ui.sock). Independent of a11y being enabled yet — it just
                // answers `service_unavailable` until the user turns the accessibility service on.
                uiSocket = try {
                    UiSocketServer(applicationContext).also { it.start() }
                } catch (e: Exception) {
                    RuntimeState.pushLog("[ui-sock] start failed: ${e.message}"); null
                }
            }
            if (gateway == null) {
                gateway = GatewayProcess(
                    rt = rt,
                    argv = { rt.gatewayArgv() },
                    onState = { st ->
                        RuntimeState.setGatewayState(st)
                        RuntimeState.setInfo(p = gateway?.pid)
                        updateNotification(st.name.lowercase())
                        if (st == GatewayProcess.State.RUNNING) refreshGatewayInfo(infoEpoch)
                    },
                    onLogLine = { RuntimeState.pushLog(it) }
                )
            }
            if (!desiredRunning) { doStop(); return }   // stop queued during setup
            gateway?.start()
            maybeStartSsh(c)
        } catch (e: Exception) {
            RuntimeState.pushLog("[service] start failed: ${e.message}")
            RuntimeState.setGatewayState(GatewayProcess.State.FAILED)
        }
    }

    /** Bring up the in-process SSH shell + constrained gateway tunnel when the user opted into
     *  encrypted remote access. The gateway itself remains loopback-only. */
    private fun maybeStartSsh(c: ConfigStore) {
        if (!c.sshEnabled) return
        val hasKey = c.sshAuthorizedKey.isNotBlank() || rt.sshAuthorizedKeys.exists()
        if (!hasKey) { android.util.Log.i("zerodroid-ssh", "no authorized key (pref or file)"); return }
        if (!c.lanAccess) {
            RuntimeState.pushLog("[ssh] not started — enable encrypted remote access first")
            return
        }
        if (Build.VERSION.SDK_INT < 26) { RuntimeState.pushLog("[ssh] needs Android 8+ (API 26)"); return }
        android.util.Log.i("zerodroid-ssh", "maybeStartSsh: starting on ${c.sshPort}")
        if (sshd == null) sshd = SshdServer(rt, c.gatewayPort) {
            RuntimeState.pushLog(it)
            android.util.Log.i("zerodroid-ssh", it)
        }
        try { sshd?.start(c.sshPort, c.sshAuthorizedKey) }
        catch (e: Exception) { RuntimeState.pushLog("[ssh] start failed: ${e.message}"); android.util.Log.e("zerodroid-ssh", "start failed", e) }
    }

    private fun doStop() {
        sshd?.stop(); sshd = null
        gateway?.stop(); gateway = null
        uiSocket?.stop(); uiSocket = null
        bridge?.stop(); bridge = null
        RuntimeState.setGatewayState(GatewayProcess.State.STOPPED)
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    /** Pull the pairing code + compute reachable URLs once the gateway is up. */
    private fun refreshGatewayInfo(epoch: Int) {
        Thread {
            val c = cfg ?: return@Thread
            val code = fetchPairCode(c)
            // Don't write stale info if a stop/restart happened while we waited.
            if (epoch != infoEpoch || gateway?.state != GatewayProcess.State.RUNNING) return@Thread
            RuntimeState.setInfo(
                pair = code,
                dash = c.gatewayUrl("127.0.0.1"),
                lan = null,
                p = gateway?.pid,
            )
        }.apply { isDaemon = true }.start()
    }

    private fun fetchPairCode(c: ConfigStore): String? {
        return try {
            val url = java.net.URL(
                "http://127.0.0.1:${c.gatewayPort}${c.activeGatewayPathPrefix}/admin/paircode"
            )
            val conn = url.openConnection() as java.net.HttpURLConnection
            try {
                conn.connectTimeout = 4_000
                conn.readTimeout = 4_000
                conn.setRequestProperty("X-Loopback-Admin-Secret", c.activeGatewayAdminSecret)
                if (conn.responseCode !in 200..299) return null
                val body = conn.inputStream.bufferedReader().use { it.readText() }
                org.json.JSONObject(body).optString("pairing_code").takeIf {
                    it.isNotBlank() && it != "null"
                }
            } finally {
                conn.disconnect()
            }
        } catch (e: Exception) {
            RuntimeState.pushLog("[pairing] could not read pair code: ${e.message}")
            null
        }
    }

    private fun shutdown() {
        gateway?.stop(); gateway = null
        uiSocket?.stop(); uiSocket = null
        bridge?.stop(); bridge = null
        RuntimeState.setGatewayState(GatewayProcess.State.STOPPED)
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onDestroy() {
        // Tell any in-flight doStart to abort, then tear down on the lifecycle thread so we don't
        // race a concurrent startup. shutdownNow lets the daemon executor go with the process.
        desiredRunning = false; infoEpoch++
        sshd?.stop(); sshd = null
        gateway?.stop(); gateway = null
        uiSocket?.stop(); uiSocket = null
        bridge?.stop(); bridge = null
        lifecycle.shutdownNow()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    // ---- foreground notification ----
    private fun createChannel() {
        if (Build.VERSION.SDK_INT >= 26) {
            getSystemService(NotificationManager::class.java).createNotificationChannel(
                NotificationChannel(CH, "zerodroid agent", NotificationManager.IMPORTANCE_LOW)
            )
        }
    }

    private fun goForeground(status: String) {
        val type = BridgeForegroundPolicy.serviceType(Build.VERSION.SDK_INT)
        if (Build.VERSION.SDK_INT >= 34) startForeground(NOTI_ID, notification(status), type)
        else startForeground(NOTI_ID, notification(status))
    }

    private fun updateNotification(status: String) {
        getSystemService(NotificationManager::class.java).notify(NOTI_ID, notification(status))
    }

    @Suppress("DEPRECATION")
    private fun notification(status: String): Notification {
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        )
        val stop = PendingIntent.getService(
            this, 1, Intent(this, BridgeService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        )
        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, CH)
        } else {
            Notification.Builder(this)
        }
        return builder
            .setContentTitle("zerodroid agent")
            .setContentText("gateway: $status")
            .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
            .setContentIntent(open)
            .addAction(Notification.Action.Builder(null, "Stop", stop).build())
            .setOngoing(true)
            .build()
    }

    companion object {
        const val ACTION_START = "org.zerodroid.bridge.START"
        const val ACTION_STOP = "org.zerodroid.bridge.STOP"
        const val ACTION_RESTART = "org.zerodroid.bridge.RESTART"
        private const val CH = "zerodroid-bridge"
        private const val NOTI_ID = 1

        fun start(ctx: Context) {
            val i = Intent(ctx, BridgeService::class.java).setAction(ACTION_START)
            if (Build.VERSION.SDK_INT >= 26) ctx.startForegroundService(i) else ctx.startService(i)
        }
        fun restart(ctx: Context) {
            val i = Intent(ctx, BridgeService::class.java).setAction(ACTION_RESTART)
            if (Build.VERSION.SDK_INT >= 26) ctx.startForegroundService(i) else ctx.startService(i)
        }
        fun stop(ctx: Context) {
            ctx.startService(Intent(ctx, BridgeService::class.java).setAction(ACTION_STOP))
        }
    }
}
