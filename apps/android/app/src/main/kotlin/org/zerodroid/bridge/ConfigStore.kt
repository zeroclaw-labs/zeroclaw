package org.zerodroid.bridge

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import java.io.File

/** One model provider offered in the picker. The full list comes from the bundled binary
 *  (`zeroclaw providers`, 61+); this is the metadata the UI layers on top. */
data class ProviderInfo(val id: String, val label: String, val local: Boolean = false)

object ProviderCatalog {
    /** Sensible default model per popular provider (prefill; the user can change it). */
    val DEFAULT_MODEL = mapOf(
        // Refreshed 2026-08-16 against the models.dev catalog (the same source
        // zeroclaw-providers/models_dev.rs uses), picking a current, tool-capable chat model per
        // provider rather than whatever was flagship when this map was written. Stale prefills are
        // not cosmetic: a withdrawn model 404s and reads as an auth failure.
        "deepseek" to "deepseek-chat", "gemini" to "gemini-3-flash-preview",
        "groq" to "openai/gpt-oss-120b", "xai" to "grok-4.6",
        "together" to "deepseek-ai/DeepSeek-V4-Flash-0731",
        "openai" to "gpt-5.6", "anthropic" to "claude-sonnet-5",
        "openrouter" to "google/gemini-3.7-flash",
        "mistral" to "mistral-medium-latest", "cerebras" to "gpt-oss-120b", "ollama" to "llama3.2",
        "llamacpp" to "local-model", "lmstudio" to "local-model", "vllm" to "local-model",
        "minimax" to "MiniMax-M3", "moonshot" to "kimi-k3",
        "fireworks" to "accounts/fireworks/models/deepseek-v4-flash-0731", "nvidia" to "z-ai/glm-5.2",
        "perplexity" to "sonar", "cohere" to "command-a-plus-05-2026", "qwen" to "qwen3.8-max",
        "siliconflow" to "deepseek-ai/DeepSeek-V4-Flash", "novita" to "deepseek/deepseek-v4-flash"
    )

    /** Providers that talk to a local endpoint — no API key, but a base URL (uri) is expected. */
    val LOCAL = setOf("ollama", "gemini_cli", "kilocli", "lmstudio", "llamacpp", "sglang", "vllm",
        "osaurus", "atomic_chat")

    /** Built-in fallback when the binary can't be queried. The live list supersedes this. */
    val FALLBACK: List<ProviderInfo> = listOf(
        "deepseek" to "DeepSeek", "gemini" to "Google Gemini", "openai" to "OpenAI", "anthropic" to "Anthropic",
        "openrouter" to "OpenRouter", "groq" to "Groq", "xai" to "xAI (Grok)", "together" to "Together AI",
        "mistral" to "Mistral", "ollama" to "Ollama", "llamacpp" to "llama.cpp", "lmstudio" to "LM Studio"
    ).map { ProviderInfo(it.first, it.second, it.first in LOCAL) }

    fun defaultModel(id: String) = DEFAULT_MODEL[id] ?: ""

    /**
     * Models a provider has withdrawn, which now answer 404 for everyone. A stored value from this
     * set is worse than no value: it silently breaks a correct API key and reads as an auth
     * problem. Add to this list when a provider retires something we once prefilled.
     */
    val RETIRED_MODELS = setOf(
        // Only add an id after seeing the provider actually reject it. Overriding a model the user
        // deliberately chose, on a guess, is worse than leaving a stale default in place.
        "gemini-2.5-flash"   // observed: 404 "no longer available to new users"
    )
    fun isLocal(id: String) = id in LOCAL
}

/** Pure config policy used by [ConfigStore] and pinned by JVM unit tests. */
internal object GeneratedConfigPolicy {
    fun redactSecrets(config: String): String = config
        .replace(Regex("(?m)^api_key = .*$"), "api_key = \"••••••••\"")
        .replace(Regex("(?m)^secret = .*$"), "secret = \"••••••••\"")
        .replace(
            Regex("(?m)^loopback_admin_secret = .*$"),
            "loopback_admin_secret = \"••••••••\"",
        )

    fun gatewayIsolation(
        pathPrefix: String,
        webhookSecret: String,
        loopbackAdminSecret: String,
    ): List<String> = listOf(
        "path_prefix = ${ConfigStore.tomlStr(pathPrefix)}",
        "loopback_admin_secret = ${ConfigStore.tomlStr(loopbackAdminSecret)}",
        "",
        "[channels.webhook.zerodroid_internal]",
        "enabled = false",
        "secret = ${ConfigStore.tomlStr(webhookSecret)}",
        "",
    )

    fun androidSection(
        enabled: Boolean,
        autonomousControl: Boolean,
        socketPath: String,
    ): List<String> {
        val autonomous = enabled && autonomousControl
        return listOf(
            "[android]",
            "enabled = $enabled",
            "socket_path = ${ConfigStore.tomlStr(socketPath)}",
            "require_approval_for_actions = ${!autonomous}",
            "screenshot_max_width = 540",
            "",
        )
    }

    fun riskProfile(autonomous: Boolean): List<String> = buildList {
        add("[risk_profiles.standard]")
        add(
            if (autonomous) {
                "auto_approve = [\"file_read\", \"web_search_tool\", \"web_fetch\", \"glob_search\", \"content_search\", \"screenshot\", \"android_screenshot\", \"android_ui_read\", \"android_device\", \"android_action\", \"android_launch\"]"
            } else {
                "auto_approve = [\"file_read\", \"web_search_tool\", \"web_fetch\", \"glob_search\", \"content_search\", \"screenshot\", \"android_screenshot\", \"android_ui_read\", \"android_device\"]"
            }
        )
        // Autonomous phone control is deliberately scoped, not `level = "full"`: UI actions and
        // app launch are pre-approved, while shell/file mutation still enter the approval flow.
        add("workspace_only = true")
        add("level = ${ConfigStore.tomlStr(if (autonomous) "supervised" else "readonly")}")
        add("block_high_risk_commands = true")
        add("require_approval_for_medium_risk = true")
        add(
            // The generated provider key lives under the app's data directory. Phone control
            // needs only the per-agent workspace; granting the package root would let file_read
            // expose config.toml and its credential to the model.
            "allowed_roots = []"
        )
        add(
            if (autonomous) {
                "allowed_commands = [\"ls\",\"cat\",\"grep\",\"echo\",\"pwd\",\"uname\",\"date\",\"ps\",\"ip\",\"curl\",\"wget\",\"sh\",\"bash\",\"head\",\"tail\",\"wc\",\"sed\",\"awk\",\"find\",\"telnet\",\"nc\",\"busybox\",\"am\",\"cmd\",\"content\",\"pm\",\"settings\",\"svc\",\"dumpsys\",\"getprop\"]"
            } else {
                "allowed_commands = [\"ls\",\"cat\",\"grep\",\"pwd\",\"uname\",\"date\",\"ps\",\"head\",\"tail\",\"wc\",\"find\"]"
            }
        )
        add("shell_env_passthrough = [\"PATH\",\"HOME\",\"USER\",\"LANG\",\"LD_LIBRARY_PATH\",\"TMPDIR\",\"ZEROCLAW_BUSYBOX\"]")
        add("allowed_tools = []")
        add(if (autonomous) "always_ask = [\"shell\", \"bash\", \"file_write\"]" else "always_ask = []")
        add(
            if (autonomous) {
                "excluded_tools = []"
            } else {
                "excluded_tools = [\"android_action\", \"android_dialog\", \"android_launch\", \"shell\", \"bash\", \"file_write\"]"
            }
        )
        add("")
    }
}

class ConfigStore(private val ctx: Context) {

    // EncryptedSharedPreferences (Tink/Keystore) can throw on corrupted keysets or OEM keystore
    // quirks. Fail closed without deleting encrypted state: a transient Keystore failure must not
    // become credential loss, and plaintext fallback would overstate at-rest protection. Lazy so
    // keystore work happens off the main/FGS thread.
    private val prefs: SharedPreferences by lazy { openPrefs(ctx) }

    private fun openPrefs(ctx: Context): SharedPreferences {
        fun encrypted(): SharedPreferences {
            val key = MasterKey.Builder(ctx).setKeyScheme(MasterKey.KeyScheme.AES256_GCM).build()
            return EncryptedSharedPreferences.create(
                ctx, SECURE_PREFS, key,
                EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
                EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM
            )
        }
        return try {
            encrypted()
        } catch (e: Exception) {
            android.util.Log.e(TAG, "encrypted prefs unavailable; refusing credential storage")
            throw IllegalStateException("Android Keystore is unavailable", e)
        }
    }

    // ---- selected provider (id string; full catalog is dynamic) ----
    var providerId: String
        get() = prefs.getString("provider_id", "deepseek") ?: "deepseek"
        set(v) = prefs.edit().putString("provider_id", v).apply()

    // ---- per-provider fields (keyed by id so switching remembers each) ----
    fun apiKey(id: String) = prefs.getString("key_$id", "") ?: ""
    fun setApiKey(id: String, v: String) = prefs.edit().putString("key_$id", v.trim()).apply()

    /**
     * Correcting the prefill in [ProviderCatalog.DEFAULT_MODEL] only helps installs that never
     * stored one. An install that already ran wrote the old prefill into prefs, so it keeps
     * calling a model the provider has since withdrawn and every request 404s. Treat a stored
     * model that is known-retired as if it were unset, so those installs heal themselves.
     */
    fun model(id: String): String {
        val stored = prefs.getString("model_$id", "").orEmpty().trim()
        if (stored.isEmpty() || stored in ProviderCatalog.RETIRED_MODELS) {
            return ProviderCatalog.defaultModel(id)
        }
        return stored
    }
    fun setModel(id: String, v: String) = prefs.edit().putString("model_$id", v.trim()).apply()

    /** Base URL / endpoint override (config `uri`) — for local + OpenAI-compatible providers. */
    fun baseUrl(id: String) = prefs.getString("uri_$id", "") ?: ""
    fun setBaseUrl(id: String, v: String) = prefs.edit().putString("uri_$id", v.trim()).apply()

    /** OAuth instead of an API key (ChatGPT subscription / Qwen OAuth). */
    fun oauth(id: String) = prefs.getBoolean("oauth_$id", false)
    fun setOauth(id: String, v: Boolean) = prefs.edit().putBoolean("oauth_$id", v).apply()

    /** Optional sampling temperature (blank = provider default). */
    fun temperature(id: String) = prefs.getString("temp_$id", "") ?: ""
    fun setTemperature(id: String, v: String) = prefs.edit().putString("temp_$id", v.trim()).apply()

    // ---- gateway / runtime toggles ----
    /** Enable encrypted remote access through the app's pubkey-only SSH server. The gateway itself
     *  remains loopback-only; remote clients use an SSH local-forward tunnel. */
    var lanAccess: Boolean
        get() = prefs.getBoolean("lan_access", false)
        set(v) = prefs.edit().putBoolean("lan_access", v).apply()

    var startOnBoot: Boolean
        get() = prefs.getBoolean("start_on_boot", false)
        set(v) = prefs.edit().putBoolean("start_on_boot", v).apply()

    /** When true the user owns config.toml (edited raw); the app will NOT regenerate it on start. */
    var manualConfig: Boolean
        get() = prefs.getBoolean("manual_config", false)
        set(v) = prefs.edit().putBoolean("manual_config", v).apply()

    /** Floating-bubble overlay on/off (drives [OverlayService]). Reflects the user's toggle; the
     *  live authority is [OverlayService.running]. */
    var overlayEnabled: Boolean
        get() = prefs.getBoolean("overlay_enabled", false)
        set(v) = prefs.edit().putBoolean("overlay_enabled", v).apply()

    /** Register the read-only Android tool family. Off by default so installing the APK and
     * enabling Accessibility never silently grants the model a phone-control surface. */
    var androidToolsEnabled: Boolean
        get() = prefs.getBoolean("android_tools_enabled", false)
        set(v) {
            val edit = prefs.edit().putBoolean("android_tools_enabled", v)
            if (!v) edit.putBoolean("autonomous_control_enabled", false)
            edit.apply()
        }

    /** Explicit high-risk opt-in for tap/type/launch; shell and file mutation stay approval-gated. */
    var autonomousControlEnabled: Boolean
        get() = prefs.getBoolean("autonomous_control_enabled", false)
        set(v) = prefs.edit()
            .putBoolean("autonomous_control_enabled", v)
            .putBoolean("android_tools_enabled", androidToolsEnabled || v)
            .apply()

    /** On-device in-process SSH shell (pubkey-only). Off by default. */
    var sshEnabled: Boolean
        get() = prefs.getBoolean("ssh_enabled", false)
        set(v) = prefs.edit().putBoolean("ssh_enabled", v).apply()
    var sshPort: Int
        get() = prefs.getInt("ssh_port", 2222)
        set(v) = prefs.edit().putInt("ssh_port", v.coerceIn(1024, 65535)).apply()
    /** The user's PUBLIC key (authorized_keys). No private material. */
    var sshAuthorizedKey: String
        get() = prefs.getString("ssh_authkey", "") ?: ""
        set(v) = prefs.edit().putString("ssh_authkey", v.trim()).apply()

    val agentAlias = "phone"
    val gatewayPort = 42617
    val providerAlias = "main"

    private fun localSecret(key: String): String {
        return synchronized(LOCAL_SECRET_LOCK) {
            val stored = prefs.getString(key, "").orEmpty().trim()
            if (isValidLocalSecret(stored)) return@synchronized stored
            java.util.UUID.randomUUID().toString().replace("-", "").also { fresh ->
                prefs.edit().putString(key, fresh).apply()
            }
        }
    }

    /** Android shares TCP loopback across app UIDs. Generated installs put the whole gateway under
     * an unguessable namespace, then retain normal pairing inside it. Manual config remains owned
     * by the operator and therefore uses whatever path it declares. */
    val activeGatewayPathPrefix: String
        get() = if (manualConfig) "" else "/zd-${localSecret("gateway_path_secret")}"

    /** Independent defense for the overlay's POST /webhook if a gateway URL is disclosed. */
    val activeGatewayWebhookSecret: String?
        get() = if (manualConfig) null else localSecret("gateway_webhook_secret")

    /** App-private second factor for localhost admin endpoints. Unlike the path prefix, this value
     *  is never shown or copied into a dashboard URL. */
    val activeGatewayAdminSecret: String?
        get() = if (manualConfig) null else localSecret("gateway_admin_secret")

    /** Persist the overlay's paired bearer token across process restarts so self-pairing does not
     *  create an orphaned valid token on every service recreation. */
    var overlayPairingToken: String?
        get() = prefs.getString("overlay_pairing_token", null)?.takeIf { it.isNotBlank() }
        set(v) = prefs.edit().apply {
            if (v.isNullOrBlank()) remove("overlay_pairing_token")
            else putString("overlay_pairing_token", v)
        }.apply()

    fun gatewayUrl(host: String): String =
        "http://$host:$gatewayPort${activeGatewayPathPrefix}/"

    fun providerRef(id: String = providerId) = "$id.$providerAlias"

    fun isConfigured(): Boolean {
        if (manualConfig) return true                 // user owns the config
        val id = providerId
        return oauth(id) || apiKey(id).isNotEmpty() ||
            (ProviderCatalog.isLocal(id) && baseUrl(id).isNotEmpty())
    }

    /**
     * Render config.toml from current settings. The gateway is always loopback-only; remote access
     * is preserved through the separately authenticated SSH tunnel rather than exposing bearer
     * tokens and user traffic over cleartext Wi-Fi.
     */
    fun renderConfig(
        webDistDir: String? = null,
        encryptedApiKey: String? = null,
        encryptedWebhookSecret: String? = null,
        encryptedAdminSecret: String? = null,
    ): String {
        val id = providerId
        val host = "127.0.0.1"
        val androidEnabled = androidToolsEnabled
        val autonomous = androidEnabled && autonomousControlEnabled
        val gatewayPathPrefix = "/zd-${localSecret("gateway_path_secret")}"
        val gatewayWebhookSecret = localSecret("gateway_webhook_secret")
        val gatewayAdminSecret = localSecret("gateway_admin_secret")
        val L = ArrayList<String>()
        L += "# Generated by zerodroid. (Switch on \"Edit config manually\" to own this file.)"
        L += "schema_version = 3"
        L += ""
        L += "[providers.models.$id.$providerAlias]"
        if (oauth(id)) {
            L += "requires_openai_auth = true"
            L += "wire_api = \"responses\""
        } else if (apiKey(id).isNotEmpty()) {
            L += "api_key = ${tomlStr(encryptedApiKey ?: apiKey(id))}"
        }
        if (model(id).isNotEmpty()) L += "model = ${tomlStr(model(id))}"
        if (baseUrl(id).isNotEmpty()) L += "uri = ${tomlStr(baseUrl(id))}"
        temperature(id).toDoubleOrNull()?.let { L += "temperature = $it" }
        L += ""
        L += "[gateway]"
        L += "host = ${tomlStr(host)}"
        L += "port = $gatewayPort"
        L += "allow_public_bind = false"
        L += "require_pairing = true"
        if (webDistDir != null) L += "web_dist_dir = ${tomlStr(webDistDir)}"
        L += GeneratedConfigPolicy.gatewayIsolation(
            gatewayPathPrefix,
            encryptedWebhookSecret ?: gatewayWebhookSecret,
            encryptedAdminSecret ?: gatewayAdminSecret,
        )
        L += GeneratedConfigPolicy.androidSection(
            androidEnabled,
            autonomousControlEnabled,
            File(ctx.filesDir, UiSocketServer.SOCKET_NAME).absolutePath,
        )
        L += GeneratedConfigPolicy.riskProfile(autonomous)
        L += "[runtime_profiles.coordinator]"
        L += "agentic = true"
        L += "agentic_timeout_secs = 600"
        L += "max_actions_per_hour = 1000"
        L += ""
        L += "[agents.$agentAlias]"
        L += "risk_profile = \"standard\""
        L += "runtime_profile = \"coordinator\""
        L += "model_provider = ${tomlStr(providerRef(id))}"
        L += "enabled = true"
        return L.joinToString("\n") + "\n"
    }

    /** Write config.toml (0600) ATOMICALLY: a reader never sees a half-written file. */
    fun writeConfig(rt: NativeRuntime) {
        rt.configDir.mkdirs()
        val webDist = rt.webDist.takeIf { File(it, "index.html").exists() }?.absolutePath
        val encoder = RuntimeSecretEncoder(rt.configDir)
        val key = apiKey(providerId).takeIf { it.isNotEmpty() }?.let(encoder::encrypt)
        val webhook = encoder.encrypt(localSecret("gateway_webhook_secret"))
        val admin = encoder.encrypt(localSecret("gateway_admin_secret"))
        val tmp = File(rt.configDir, "config.toml.tmp")
        tmp.writeText(renderConfig(webDist, key, webhook, admin))
        try { android.system.Os.chmod(tmp.absolutePath, 384 /* 0600 */) } catch (e: Exception) {}
        if (!tmp.renameTo(rt.configFile)) {
            rt.configFile.writeText(renderConfig(webDist, key, webhook, admin))
            try { android.system.Os.chmod(rt.configFile.absolutePath, 384) } catch (e: Exception) {}
            tmp.delete()
        }
    }

    /** Raw config text on disk (or the generated one if none exists yet) — for the manual editor. */
    fun readRawConfig(rt: NativeRuntime): String =
        if (rt.configFile.exists()) rt.configFile.readText() else renderConfig()

    /** Persist raw, user-edited config.toml atomically (manual mode). */
    fun writeRawConfig(rt: NativeRuntime, text: String) {
        rt.configDir.mkdirs()
        val tmp = File(rt.configDir, "config.toml.tmp")
        tmp.writeText(if (text.endsWith("\n")) text else text + "\n")
        try { android.system.Os.chmod(tmp.absolutePath, 384) } catch (e: Exception) {}
        if (!tmp.renameTo(rt.configFile)) { rt.configFile.writeText(text); tmp.delete() }
    }

    fun configPreviewRedacted(): String =
        GeneratedConfigPolicy.redactSecrets(renderConfig())

    companion object {
        private const val TAG = "zerodroid-config"
        private const val SECURE_PREFS = "zerodroid-secure"
        private val LOCAL_SECRET_LOCK = Any()

        /** Render any user string as a safe TOML basic string (quoted + escaped). */
        fun tomlStr(s: String): String {
            val sb = StringBuilder("\"")
            for (c in s) when (c) {
                '\\' -> sb.append("\\\\")
                '"' -> sb.append("\\\"")
                '\b' -> sb.append("\\b")
                '\t' -> sb.append("\\t")
                '\n' -> sb.append("\\n")
                '\r' -> sb.append("\\r")
                else -> if (c < ' ' || c.code == 0x7F) sb.append("\\u%04X".format(c.code))
                        else sb.append(c)
            }
            return sb.append("\"").toString()
        }
    }
}
