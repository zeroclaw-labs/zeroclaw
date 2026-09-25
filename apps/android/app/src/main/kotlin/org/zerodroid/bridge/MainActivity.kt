package org.zerodroid.bridge

import android.Manifest
import androidx.appcompat.app.AlertDialog
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.graphics.Color
import android.graphics.Typeface
import android.net.Uri
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Bundle
import android.os.PowerManager
import android.provider.Settings
import android.text.InputType
import android.view.Gravity
import android.view.View
import android.view.WindowManager
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.widget.*
import androidx.appcompat.app.AppCompatActivity
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat

/**
 * The config + control surface (programmatic Views). Pick a provider (full 61-provider catalog from
 * the bundled binary), key/model/base-URL, Start. A network card shows the phone's IP + reach
 * commands and can recycle the gateway. "Edit config" opens the full raw config.toml surface.
 */
class MainActivity : AppCompatActivity(), RuntimeState.Listener {

    private lateinit var cfg: ConfigStore
    private lateinit var rt: NativeRuntime
    private lateinit var providerSpinner: Spinner
    private lateinit var keyField: EditText
    private lateinit var modelField: EditText
    private lateinit var baseUrlField: EditText
    private lateinit var tempField: EditText
    private lateinit var oauthCheck: CheckBox
    private lateinit var lanSwitch: Switch
    private lateinit var bootSwitch: Switch
    private lateinit var manualSwitch: Switch
    private lateinit var overlaySwitch: Switch
    private lateinit var androidToolsSwitch: Switch
    private lateinit var autonomousControlSwitch: Switch
    private lateinit var sshSwitch: Switch
    private lateinit var sshPortField: EditText
    private lateinit var sshKeyField: EditText
    @Volatile private var uidName: String = "shell"
    private lateinit var statusPill: TextView
    private lateinit var infoView: TextView
    private lateinit var networkView: TextView
    private lateinit var logView: TextView
    private lateinit var startBtn: Button
    private lateinit var stopBtn: Button
    private lateinit var scroll: ScrollView

    @Volatile private var catalog: List<ProviderInfo> = ProviderCatalog.FALLBACK
    @Volatile private var ready = false
    // Suppresses switch listeners while we set their state from config/live state programmatically.
    @Volatile private var bindingUi = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // This activity renders provider credentials, pairing codes and private gateway paths.
        window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        cfg = ConfigStore(applicationContext)
        rt = NativeRuntime(applicationContext)
        setContentView(buildUi())
        ensureNotificationPermission()
        // keystore + provider-catalog (runs the binary) are off-main; then bind the UI.
        Thread {
            cfg.providerId                       // warm lazy EncryptedSharedPreferences
            cfg.activeGatewayPathPrefix          // generate per-install gateway guards off-main
            cfg.activeGatewayWebhookSecret
            val cat = try { rt.listProviders() } catch (e: Exception) { ProviderCatalog.FALLBACK }
            uidName = try { rt.uidName() } catch (e: Exception) { "shell" }
            runOnUiThread { catalog = cat; bindCatalog(); bindFromConfig() }
        }.apply { isDaemon = true }.start()
    }

    override fun onResume() {
        super.onResume()
        RuntimeState.addListener(this)
        syncState()
        refreshLog()
        refreshNetwork()
        refreshOverlaySwitch()
        refreshCapabilitySwitches()
    }
    override fun onPause() { super.onPause(); RuntimeState.removeListener(this) }

    private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()

    private fun buildUi(): View {
        scroll = ScrollView(this)
        val col = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL; setPadding(dp(20), dp(22), dp(20), dp(28))
        }
        scroll.addView(col)

        // ---- header ----
        col.addView(TextView(this).apply { text = "zerodroid"; textSize = 28f; setTypeface(typeface, Typeface.BOLD) })
        col.addView(TextView(this).apply {
            text = "A self-hosted AI agent that lives on your phone."
            textSize = 13f; setTextColor(Color.GRAY); setPadding(0, dp(2), 0, dp(14))
        })

        statusPill = TextView(this).apply {
            textSize = 14f; setTypeface(typeface, Typeface.BOLD); setPadding(dp(12), dp(8), dp(12), dp(8))
        }
        col.addView(statusPill)

        // ---- primary agent card: the only fields most runs need ----
        val card = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Color.parseColor("#141414")); setPadding(dp(14), dp(6), dp(14), dp(14))
        }
        col.addView(card, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT).apply { topMargin = dp(14) })

        providerSpinner = Spinner(this).apply {
            onItemSelectedListener = object : AdapterView.OnItemSelectedListener {
                override fun onItemSelected(p: AdapterView<*>?, v: View?, pos: Int, id: Long) {
                    catalog.getOrNull(pos)?.let { onProviderPicked(it) }
                }
                override fun onNothingSelected(p: AdapterView<*>?) {}
            }
        }
        card.addView(label("Provider")); card.addView(providerSpinner)

        keyField = EditText(this).apply {
            hint = "API key"; inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
        }
        card.addView(label("API key")); card.addView(keyField)

        modelField = EditText(this).apply { hint = "model"; inputType = InputType.TYPE_CLASS_TEXT }
        card.addView(label("Model")); card.addView(modelField)

        val btnRow = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL; setPadding(0, dp(12), 0, dp(2)) }
        startBtn = Button(this).apply { text = "Start agent"; setOnClickListener { onStartAgent() } }
        stopBtn = Button(this).apply { text = "Stop"; setOnClickListener { BridgeService.stop(this@MainActivity) } }
        btnRow.addView(startBtn, lp(1f)); btnRow.addView(stopBtn, lp(1f))
        card.addView(btnRow)

        infoView = TextView(this).apply {
            textSize = 13f; setTextIsSelectable(true); setPadding(0, dp(8), 0, 0)
            setOnClickListener { copyDashboardUrl() }
        }
        card.addView(infoView)

        // ---- first-run setup (expanded) ----
        col.addView(collapsible("Setup — first run", true) { c ->
            c.addView(Button(this).apply { text = "Test provider key"; setOnClickListener { testProvider() } })
            c.addView(Button(this).apply {
                text = getString(R.string.basic_permissions_label)
                setOnClickListener { requestBasicPermissions() }
            })
            c.addView(Button(this).apply {
                text = getString(R.string.sensitive_permissions_label)
                setOnClickListener { confirmSensitivePermissions() }
            })
            c.addView(Button(this).apply {
                text = "Ignore battery optimization (keep agent alive)"; setOnClickListener { requestIgnoreBattery() }
            })
            c.addView(Button(this).apply {
                text = "Enable UI control (Accessibility)"; setOnClickListener { openAccessibilitySettings() }
            })
            c.addView(hint("UI control turns on screen read + tap/swipe/type for the agent (ui.sock). " +
                "Revoke any time in Settings > Accessibility."))

            val capabilityPanel = LinearLayout(this).apply {
                orientation = LinearLayout.VERTICAL
                setBackgroundColor(ContextCompat.getColor(this@MainActivity, R.color.cellclaw_overlay_surface))
                setPadding(dp(12), dp(8), dp(12), dp(8))
            }
            androidToolsSwitch = Switch(this).apply {
                text = getString(R.string.phone_tools_label)
                setTextColor(ContextCompat.getColor(this@MainActivity, R.color.cellclaw_state_idle))
                setOnCheckedChangeListener { _, want -> onAndroidToolsToggle(want) }
            }
            capabilityPanel.addView(androidToolsSwitch)
            capabilityPanel.addView(hint(getString(R.string.phone_tools_hint)))
            autonomousControlSwitch = Switch(this).apply {
                text = getString(R.string.autonomous_control_label)
                setTextColor(ContextCompat.getColor(this@MainActivity, R.color.cellclaw_state_busy))
                setOnCheckedChangeListener { _, want -> onAutonomousControlToggle(want) }
            }
            capabilityPanel.addView(autonomousControlSwitch)
            capabilityPanel.addView(hint(getString(R.string.autonomous_control_hint)))
            c.addView(capabilityPanel, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT).apply {
                topMargin = dp(8)
            })

            overlaySwitch = Switch(this).apply {
                text = "Floating bubble (drive the agent from any app)"; setPadding(0, dp(8), 0, 0)
                setOnCheckedChangeListener { _, want -> onOverlayToggle(want) }
            }
            c.addView(overlaySwitch)
            c.addView(hint("A draggable bubble floats over other apps. Tap it, type a task, and the " +
                "agent runs on your phone and shows the reply. Needs \"Draw over other apps\"."))
        })

        // ---- provider options (advanced, collapsed) ----
        col.addView(collapsible("Provider options", false) { c ->
            baseUrlField = EditText(this).apply {
                hint = "https://… (local / OpenAI-compatible only)"; inputType = InputType.TYPE_TEXT_VARIATION_URI
            }
            c.addView(label("Base URL (optional)")); c.addView(baseUrlField)
            oauthCheck = CheckBox(this).apply {
                text = "Use OAuth subscription instead of a key (OpenAI/Qwen)"; setPadding(0, dp(6), 0, 0)
            }
            c.addView(oauthCheck)
            tempField = EditText(this).apply {
                hint = "blank = provider default"; inputType = InputType.TYPE_CLASS_NUMBER or InputType.TYPE_NUMBER_FLAG_DECIMAL
            }
            c.addView(label("Temperature (optional)")); c.addView(tempField)
        })

        // ---- access & network (collapsed) ----
        col.addView(collapsible("Access & network", false) { c ->
            lanSwitch = Switch(this).apply {
                text = "Encrypted remote access (SSH tunnel over wifi)"
            }
            c.addView(lanSwitch)
            c.addView(hint("The gateway always stays on loopback. On starts the pubkey-only SSH " +
                "listener so a PC can forward the dashboard through an encrypted tunnel."))
            bootSwitch = Switch(this).apply { text = "Start automatically on boot"; setPadding(0, dp(8), 0, 0) }
            c.addView(bootSwitch)
            c.addView(hint("Bring the agent back after a reboot (pair with battery exemption)."))
            networkView = TextView(this).apply {
                typeface = Typeface.MONOSPACE; textSize = 12f; setTextIsSelectable(true)
                setPadding(dp(10), dp(10), dp(10), dp(10)); setBackgroundColor(Color.parseColor("#16201A"))
                setTextColor(Color.parseColor("#BFE8C8"))
            }
            c.addView(networkView, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT).apply { topMargin = dp(8) })
            val netRow = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL; setPadding(0, dp(6), 0, 0) }
            netRow.addView(Button(this).apply { text = "Refresh"; setOnClickListener { refreshNetwork() } }, lp(1f))
            netRow.addView(Button(this).apply { text = "Recycle"; setOnClickListener { recycleNetwork() } }, lp(1f))
            netRow.addView(Button(this).apply { text = "Wi-Fi"; setOnClickListener { openWifiSettings() } }, lp(1f))
            c.addView(netRow)
        })

        // ---- advanced config (collapsed) ----
        col.addView(collapsible("Advanced config", false) { c ->
            manualSwitch = Switch(this).apply {
                text = "Edit config manually (app stops regenerating it)"
                setOnCheckedChangeListener { _, v -> if (ready) cfg.manualConfig = v }
            }
            c.addView(manualSwitch)
            c.addView(Button(this).apply {
                text = "Edit config.toml (advanced)"
                setOnClickListener {
                    if (ready) startActivity(Intent(this@MainActivity, ConfigEditorActivity::class.java))
                    else toast("still loading…")
                }
            })
        })

        // ---- remote shell (SSH) (collapsed) ----
        col.addView(collapsible("Remote shell (SSH)", false) { c ->
            sshSwitch = Switch(this).apply { text = "Enable SSH shell (in-process, pubkey-only)" }
            c.addView(sshSwitch)
            c.addView(hint("Encrypted shell and dashboard tunnel into the phone — no passwords. " +
                "Paste your PUBLIC key below and enable encrypted remote access."))
            sshPortField = EditText(this).apply { hint = "2222"; inputType = InputType.TYPE_CLASS_NUMBER }
            c.addView(label("SSH port")); c.addView(sshPortField)
            sshKeyField = EditText(this).apply {
                hint = "ssh-ed25519 AAAA…  (your ~/.ssh/id_ed25519.pub)"
                inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
                minLines = 2
            }
            c.addView(label("Your public key (authorized_keys)")); c.addView(sshKeyField)
        })

        // ---- gateway log (collapsed) ----
        col.addView(collapsible("Gateway log", false) { c ->
            logView = TextView(this).apply {
                typeface = Typeface.MONOSPACE; textSize = 10f
                setTextColor(Color.parseColor("#33FF66")); setBackgroundColor(Color.parseColor("#101010"))
                setPadding(dp(8), dp(8), dp(8), dp(8))
            }
            c.addView(logView, LinearLayout.LayoutParams(MATCH_PARENT, dp(220)))
        })
        return scroll
    }

    /** A tap-to-expand section: a bold header row that shows/hides its content. Keeps the
     *  primary agent card front-and-center and tucks advanced controls out of the way. */
    private fun collapsible(title: String, startExpanded: Boolean, build: (LinearLayout) -> Unit): View {
        val wrap = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        val content = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            visibility = if (startExpanded) View.VISIBLE else View.GONE
        }
        val header = TextView(this).apply {
            text = (if (startExpanded) "▾  " else "▸  ") + title
            textSize = 13f; setTypeface(typeface, Typeface.BOLD)
            setTextColor(Color.parseColor("#9A9A9A")); setPadding(0, dp(20), 0, dp(6))
            setOnClickListener {
                val show = content.visibility != View.VISIBLE
                content.visibility = if (show) View.VISIBLE else View.GONE
                text = (if (show) "▾  " else "▸  ") + title
            }
        }
        build(content)
        wrap.addView(header); wrap.addView(content)
        return wrap
    }

    private fun label(t: String) = TextView(this).apply {
        text = t; textSize = 12f; setTextColor(Color.parseColor("#888888")); setPadding(0, dp(8), 0, dp(2))
    }
    private fun hint(t: String) = TextView(this).apply {
        text = t; textSize = 12f; setTextColor(Color.GRAY); setPadding(0, dp(2), 0, dp(4))
    }
    private fun lp(weight: Float) = LinearLayout.LayoutParams(0, WRAP_CONTENT, weight).apply { marginEnd = dp(8) }

    // ---------- catalog + config binding ----------
    private fun bindCatalog() {
        providerSpinner.adapter = ArrayAdapter(this, android.R.layout.simple_spinner_dropdown_item,
            catalog.map { it.label + if (it.local) "  [local]" else "" })
    }

    private fun bindFromConfig() {
        ready = true
        val idx = catalog.indexOfFirst { it.id == cfg.providerId }.coerceAtLeast(0)
        providerSpinner.setSelection(idx)
        catalog.getOrNull(idx)?.let { onProviderPicked(it) }
        lanSwitch.isChecked = cfg.lanAccess
        bootSwitch.isChecked = cfg.startOnBoot
        manualSwitch.isChecked = cfg.manualConfig
        sshSwitch.isChecked = cfg.sshEnabled
        sshPortField.setText(cfg.sshPort.toString())
        sshKeyField.setText(cfg.sshAuthorizedKey)
        refreshCapabilitySwitches()
        refreshOverlaySwitch()
        refreshNetwork()
    }

    private fun refreshCapabilitySwitches() {
        if (!::androidToolsSwitch.isInitialized || !ready) return
        bindingUi = true
        androidToolsSwitch.isChecked = cfg.androidToolsEnabled
        autonomousControlSwitch.isChecked = cfg.autonomousControlEnabled
        autonomousControlSwitch.isEnabled = cfg.androidToolsEnabled
        bindingUi = false
    }

    private fun onAndroidToolsToggle(want: Boolean) {
        if (!ready || bindingUi) return
        if (manualSwitch.isChecked) {
            toast("Edit Android capabilities in config.toml while manual config is enabled.")
            refreshCapabilitySwitches()
            return
        }
        cfg.androidToolsEnabled = want
        if (!want) cfg.autonomousControlEnabled = false
        refreshCapabilitySwitches()
        restartForCapabilityChange()
        if (want && !UiBridge.isServiceConnected(applicationContext)) {
            toast("Turn on zerodroid in Accessibility to read the screen.")
            openAccessibilitySettings()
        }
    }

    private fun onAutonomousControlToggle(want: Boolean) {
        if (!ready || bindingUi) return
        if (manualSwitch.isChecked) {
            toast("Edit Android capabilities in config.toml while manual config is enabled.")
            refreshCapabilitySwitches()
            return
        }
        if (!want) {
            cfg.autonomousControlEnabled = false
            refreshCapabilitySwitches()
            restartForCapabilityChange()
            return
        }

        AlertDialog.Builder(this)
            .setTitle(R.string.autonomous_control_title)
            .setMessage(R.string.autonomous_control_message)
            .setPositiveButton(R.string.autonomous_control_allow) { _, _ ->
                cfg.androidToolsEnabled = true
                cfg.autonomousControlEnabled = true
                refreshCapabilitySwitches()
                restartForCapabilityChange()
                if (!UiBridge.isServiceConnected(applicationContext)) openAccessibilitySettings()
            }
            .setNegativeButton(R.string.autonomous_control_cancel) { _, _ ->
                cfg.autonomousControlEnabled = false
                refreshCapabilitySwitches()
            }
            .setOnCancelListener {
                cfg.autonomousControlEnabled = false
                refreshCapabilitySwitches()
            }
            .show()
    }

    /** A risk-profile change must take effect at the process boundary. Live gateway sessions keep
     * their resolved profile, so a preferences-only toggle would make the UI claim control was off
     * while the current process retained it. Restarting also drops those stale sessions. */
    private fun restartForCapabilityChange() {
        when (RuntimeState.gatewayState) {
            GatewayProcess.State.STARTING,
            GatewayProcess.State.RUNNING,
            GatewayProcess.State.RESTARTING -> {
                BridgeService.restart(this)
                toast("Restarting the agent to apply the new phone-access policy…")
            }
            GatewayProcess.State.STOPPED,
            GatewayProcess.State.FAILED -> Unit
        }
    }

    /** Reflect the bubble's ACTUAL running state in the toggle (authority is [OverlayService.running],
     *  not the persisted pref — the user can close the bubble from the bubble itself). */
    private fun refreshOverlaySwitch() {
        if (!::overlaySwitch.isInitialized) return
        bindingUi = true
        overlaySwitch.isChecked = OverlayService.running
        bindingUi = false
    }

    private fun onOverlayToggle(want: Boolean) {
        if (!ready || bindingUi) return
        if (want) {
            if (manualSwitch.isChecked) {
                toast("The bubble requires app-managed config so its private gateway path stays in sync.")
                bindingUi = true; overlaySwitch.isChecked = false; bindingUi = false
                return
            }
            if (!Settings.canDrawOverlays(this)) {
                toast("Grant \"Draw over other apps\" for zerodroid, then flip this on again.")
                try { startActivity(Intent(Settings.ACTION_MANAGE_OVERLAY_PERMISSION, Uri.parse("package:$packageName"))) }
                catch (e: Exception) { runCatching { startActivity(Intent(Settings.ACTION_MANAGE_OVERLAY_PERMISSION)) } }
                bindingUi = true; overlaySwitch.isChecked = false; bindingUi = false
                return
            }
            cfg.overlayEnabled = true
            OverlayService.start(this)
            toast("Bubble on — drag it anywhere; tap to ask.")
        } else {
            cfg.overlayEnabled = false
            OverlayService.stop(this)
        }
    }

    private fun onProviderPicked(p: ProviderInfo) {
        if (!ready) { keyField.setText(""); modelField.setText(ProviderCatalog.defaultModel(p.id)); return }
        keyField.setText(cfg.apiKey(p.id))
        modelField.setText(cfg.model(p.id))
        baseUrlField.setText(cfg.baseUrl(p.id))
        baseUrlField.hint = if (p.local) "http://127.0.0.1:port (local provider)" else "https://… (OpenAI-compatible only)"
        oauthCheck.isChecked = cfg.oauth(p.id)
        tempField.setText(cfg.temperature(p.id))
        keyField.visibility = if (oauthCheck.isChecked) View.GONE else View.VISIBLE
    }

    private fun persistUi() {
        val p = catalog.getOrNull(providerSpinner.selectedItemPosition) ?: return
        cfg.providerId = p.id
        cfg.setApiKey(p.id, keyField.text.toString())
        cfg.setModel(p.id, modelField.text.toString())
        cfg.setBaseUrl(p.id, baseUrlField.text.toString())
        cfg.setOauth(p.id, oauthCheck.isChecked)
        cfg.setTemperature(p.id, tempField.text.toString())
        cfg.lanAccess = lanSwitch.isChecked
        cfg.startOnBoot = bootSwitch.isChecked
        cfg.manualConfig = manualSwitch.isChecked
        cfg.androidToolsEnabled = androidToolsSwitch.isChecked
        cfg.autonomousControlEnabled = androidToolsSwitch.isChecked && autonomousControlSwitch.isChecked
        cfg.sshEnabled = sshSwitch.isChecked
        sshPortField.text.toString().toIntOrNull()?.let { if (it in 1024..65535) cfg.sshPort = it }
        cfg.sshAuthorizedKey = sshKeyField.text.toString()
    }

    // ---------- actions ----------
    private fun onStartAgent() {
        if (!ready) { toast("still loading…"); return }
        persistUi()
        if (!cfg.isConfigured()) { toast("Enter an API key (or base URL for local) for this provider."); return }
        ensureNotificationPermission()
        BridgeService.start(this)
        val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
        if (!pm.isIgnoringBatteryOptimizations(packageName))
            toast("Tip: tap \"Ignore battery optimization\" so the OS keeps the agent alive.")
    }

    private fun testProvider() {
        if (!ready) { toast("still loading…"); return }
        persistUi()
        if (!cfg.isConfigured()) { toast("Configure the provider first."); return }
        val label = catalog.getOrNull(providerSpinner.selectedItemPosition)?.label ?: cfg.providerId
        toast("Testing $label…")
        Thread {
            val test = NativeRuntime(applicationContext, variant = "test")
            var ok = false
            var detail = ""
            try {
                test.prepare(cfg.agentAlias); cfg.writeConfig(test)
                val out = test.execWithTimeout(
                    listOf(test.zeroclawBin.absolutePath, "agent", "-a", cfg.agentAlias,
                        "-m", "Reply with exactly the word READY and nothing else.",
                        "--config-dir", test.configDir.absolutePath), 45_000) { test.applyEnv(it) }
                ok = out != null && out.contains("READY", ignoreCase = true)
                // The binary already says exactly what went wrong (404 retired model, 403
                // unregistered caller, 429 quota). Throwing that away and printing "check
                // key/model/base-URL" sends people hunting a key problem that may not exist.
                if (!ok) detail = when {
                    out == null -> "no response in 45s — check the connection"
                    else -> summarizeFailure(out)
                }
            } catch (e: Exception) { detail = e.message ?: e.javaClass.simpleName }
            runOnUiThread {
                if (ok) toast("✓ $label works — config is valid.")
                else showTestFailure(label, detail)
            }
        }.apply { isDaemon = true }.start()
    }

    /** Pull the useful line out of the binary's output: the provider error, not the banner. */
    private fun summarizeFailure(out: String): String {
        val lines = out.lines().map { it.trim() }.filter { it.isNotEmpty() }
        val err = lines.lastOrNull { it.contains("error=", true) || it.startsWith("Error", true) }
            ?: lines.lastOrNull() ?: "unknown failure"
        return if (err.length > 900) err.take(900) + "…" else err
    }

    /** A dialog, not a toast: these messages are long and the user needs to read and act on them. */
    private fun showTestFailure(label: String, detail: String) {
        val hint = when {
            detail.contains("404") || detail.contains("not found", true) ->
                "\n\nThe model name looks wrong or retired. Set Model to a current one (e.g. gemini-3-flash-preview)."
            detail.contains("403") || detail.contains("unregistered", true) ->
                "\n\nThe request carried no usable credential — re-paste the API key (no spaces or line breaks)."
            detail.contains("401") || detail.contains("API_KEY_INVALID", true) ->
                "\n\nThe key was rejected. Check you pasted the whole key for this provider."
            detail.contains("429") || detail.contains("quota", true) ->
                "\n\nRate-limited or out of quota, which is not a config problem. Try again later, switch model, or enable billing."
            else -> ""
        }
        AlertDialog.Builder(this)
            .setTitle("✗ $label test failed")
            .setMessage(detail + hint)
            .setPositiveButton("Close", null)
            .show()
    }

    // ---------- network ----------
    private fun lanIp(): String? = try {
        val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
        @Suppress("DEPRECATION") val ip = wifi.connectionInfo.ipAddress
        if (ip == 0) null else String.format("%d.%d.%d.%d", ip and 0xff, ip shr 8 and 0xff, ip shr 16 and 0xff, ip shr 24 and 0xff)
    } catch (e: Exception) { null }

    private fun refreshNetwork() {
        val ip = lanIp()                          // WifiManager only — no prefs
        val port = cfg.gatewayPort                // plain val, no keystore
        val remote = ready && cfg.lanAccess       // encrypted SSH listener, never a gateway bind
        val running = RuntimeState.gatewayState == GatewayProcess.State.RUNNING
        val lines = ArrayList<String>()
        lines += "phone IP : ${ip ?: "(wifi down)"}"
        lines += "gateway  : ${if (running) "running :$port" else "stopped"} (loopback only)"
        RuntimeState.pairCode?.let { lines += "pair code: $it" }
        lines += "—"
        if (ready && cfg.sshEnabled && remote && ip != null) {
            lines += "ssh      : ssh -p ${cfg.sshPort} $uidName@$ip"
            lines += "tunnel   : ssh -N -L $port:127.0.0.1:$port -p ${cfg.sshPort} $uidName@$ip"
            lines += "dashboard: ${cfg.gatewayUrl("127.0.0.1")}  (after tunnel)"
        }
        lines += "CLI      : /data/local/tmp/zeroclaw  (adb/ssh shell)"
        networkView.text = lines.joinToString("\n")
    }

    private fun recycleNetwork() {
        if (!ready) { toast("still loading…"); return }
        persistUi()
        // Restart the local gateway and encrypted remote-access listener after network changes.
        BridgeService.restart(this)
        toast("Recycling gateway and remote-access listener…")
        networkView.postDelayed({ refreshNetwork() }, 4000)
    }

    private fun openWifiSettings() {
        try { startActivity(Intent(Settings.ACTION_WIFI_SETTINGS)) } catch (e: Exception) {
            startActivity(Intent(Settings.ACTION_WIRELESS_SETTINGS))
        }
    }

    private fun copyDashboardUrl() {
        val txt = RuntimeState.lanUrl ?: RuntimeState.dashboardUrl ?: return
        (getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager)
            .setPrimaryClip(ClipData.newPlainText("zerodroid dashboard URL", txt))
        toast("Dashboard URL copied — enter the pairing code manually.")
    }

    // ---------- permissions ----------
    private fun ensureNotificationPermission() {
        if (Build.VERSION.SDK_INT >= 33 &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS)
            != android.content.pm.PackageManager.PERMISSION_GRANTED)
            ActivityCompat.requestPermissions(this, arrayOf(Manifest.permission.POST_NOTIFICATIONS), 2)
    }

    private fun requestBasicPermissions() {
        val wanted = buildList {
            add(Manifest.permission.ACCESS_FINE_LOCATION); add(Manifest.permission.ACCESS_COARSE_LOCATION)
            if (Build.VERSION.SDK_INT >= 31) { add(Manifest.permission.BLUETOOTH_SCAN); add(Manifest.permission.BLUETOOTH_CONNECT) }
            if (Build.VERSION.SDK_INT >= 33) { add(Manifest.permission.POST_NOTIFICATIONS); add(Manifest.permission.NEARBY_WIFI_DEVICES) }
        }.filter { ContextCompat.checkSelfPermission(this, it) != android.content.pm.PackageManager.PERMISSION_GRANTED }
        if (wanted.isEmpty()) toast("basic permissions granted")
        else ActivityCompat.requestPermissions(this, wanted.toTypedArray(), 1)
    }

    private fun confirmSensitivePermissions() {
        AlertDialog.Builder(this)
            .setTitle(R.string.sensitive_permissions_title)
            .setMessage(R.string.sensitive_permissions_message)
            .setPositiveButton(R.string.sensitive_permissions_allow) { _, _ ->
                val wanted = buildList {
                    add(Manifest.permission.READ_SMS)
                    add(Manifest.permission.SEND_SMS)
                    add(Manifest.permission.READ_PHONE_STATE)
                    add(Manifest.permission.READ_CONTACTS)
                }.filter {
                    ContextCompat.checkSelfPermission(this, it) !=
                        android.content.pm.PackageManager.PERMISSION_GRANTED
                }
                if (wanted.isEmpty()) toast("sensitive permissions already granted")
                else ActivityCompat.requestPermissions(this, wanted.toTypedArray(), 3)
            }
            .setNegativeButton(R.string.sensitive_permissions_cancel, null)
            .show()
    }

    private fun requestIgnoreBattery() {
        val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
        if (pm.isIgnoringBatteryOptimizations(packageName)) { toast("already exempt"); return }
        try { startActivity(Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:$packageName"))) }
        catch (e: Exception) { startActivity(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS)) }
    }

    /** Deep-link to the system Accessibility list so the user can enable the UI control service.
     *  Android has no API to toggle it programmatically — the grant is always a manual user action. */
    private fun openAccessibilitySettings() {
        if (UiBridge.isServiceConnected(applicationContext)) { toast("UI control already enabled"); return }
        try { startActivity(Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS)) }
        catch (e: Exception) { toast("open Settings > Accessibility to enable zerodroid") }
    }

    // ---------- live state ----------
    override fun onGatewayState(state: GatewayProcess.State) = runOnUiThread { syncState(); refreshNetwork() }
    override fun onInfo() = runOnUiThread { syncState(); refreshNetwork() }
    override fun onLog(line: String) = runOnUiThread { appendLog(line) }

    private fun syncState() {
        val s = RuntimeState.gatewayState
        val (lbl, color) = when (s) {
            GatewayProcess.State.RUNNING -> "RUNNING" to "#1B5E20"
            GatewayProcess.State.STARTING -> "STARTING…" to "#E65100"
            GatewayProcess.State.RESTARTING -> "RESTARTING…" to "#E65100"
            GatewayProcess.State.FAILED -> "FAILED" to "#B71C1C"
            GatewayProcess.State.STOPPED -> "STOPPED" to "#424242"
        }
        statusPill.text = "  ● $lbl" + (RuntimeState.pid?.let { "   pid $it" } ?: "")
        statusPill.setBackgroundColor(Color.parseColor(color)); statusPill.setTextColor(Color.WHITE)
        startBtn.isEnabled = s == GatewayProcess.State.STOPPED || s == GatewayProcess.State.FAILED
        stopBtn.isEnabled = !startBtn.isEnabled
        infoView.text = listOfNotNull(
            RuntimeState.pairCode?.let { "Pairing code: $it  (tap to copy)" },
            RuntimeState.lanUrl?.let { "LAN dashboard: $it" },
            RuntimeState.dashboardUrl?.let { "Local dashboard: $it" }
        ).joinToString("\n")
    }

    private fun refreshLog() { logView.text = RuntimeState.logSnapshot().takeLast(200).joinToString("\n"); scrollLog() }
    private fun appendLog(line: String) {
        logView.append("\n$line")
        if ((logView.text?.length ?: 0) > 40_000) refreshLog() else scrollLog()
    }
    private fun scrollLog() = scroll.post { scroll.fullScroll(View.FOCUS_DOWN) }
    private fun toast(s: String) = Toast.makeText(this, s, Toast.LENGTH_LONG).show()

    override fun onRequestPermissionsResult(rc: Int, perms: Array<out String>, res: IntArray) {
        super.onRequestPermissionsResult(rc, perms, res)
        if (rc == 1) toast("permissions updated")
    }
}
