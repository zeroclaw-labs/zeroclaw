package org.zerodroid.bridge

import org.json.JSONObject
import java.io.IOException
import java.net.ConnectException
import java.net.HttpURLConnection
import java.net.URL

/**
 * Minimal client for the on-device zeroclaw gateway (loopback `127.0.0.1:<gatewayPort>`), used by
 * the floating bubble to run a one-shot prompt through `POST /webhook`.
 *
 * Auth: the gateway ships with `require_pairing = true` (see [ConfigStore.renderConfig]), so
 * `/webhook` needs `Authorization: Bearer <token>`. The APK's own `bridge-token` is a *different*
 * credential (it authenticates the :8470 permission bridge, not the gateway), so it will not pass
 * the pairing guard. Android shares TCP loopback across app UIDs, so generated installs place
 * every route under an app-private random path and add independent webhook and loopback-admin
 * secrets. Inside that namespace we self-pair over the gateway's localhost-only admin endpoints;
 * Android loopback alone is not trusted, so pair-code minting also proves the app-private secret:
 *
 *   1. `POST /admin/paircode/new` + `X-Loopback-Admin-Secret` → one-time code.
 *   2. `POST /pair` (X-Pairing-Code) → exchanges the code for a bearer token.
 *   3. `POST /webhook` (Bearer)      → `{ "message": "..." }` → `{ "model", "response" }`.
 *
 * The token is cached in-process and reused; a later 401 (gateway restarted with different tokens)
 * triggers one transparent re-pair + retry. Every method is blocking and MUST be called off the
 * main thread (the overlay does so on a worker thread). Nothing here throws — failures surface as a
 * [Reply] with [Reply.gatewayDown] set or an [Reply.error] message.
 */
object GatewayClient {

    data class Reply(
        val response: String?,
        val model: String?,
        val error: String?,
        /** Connection refused / unreachable — the agent process is not running. */
        val gatewayDown: Boolean = false,
        /** Current paired bearer token. Persist it in Keystore-backed prefs across app restarts. */
        val pairingToken: String? = null,
    )

    private const val TAG = "zerodroid-gateway"
    private const val CONNECT_TIMEOUT_MS = 4_000
    // Agent turns can be slow (model round-trip); give the read a generous ceiling.
    private const val READ_TIMEOUT_MS = 120_000

    @Volatile private var cachedToken: String? = null

    private fun base(port: Int, pathPrefix: String) = "http://127.0.0.1:$port$pathPrefix"

    /** Run [message] through the gateway and return its reply (or a graceful failure). */
    fun ask(
        port: Int,
        pathPrefix: String,
        webhookSecret: String?,
        loopbackAdminSecret: String?,
        persistedPairingToken: String?,
        message: String,
    ): Reply {
        if (cachedToken.isNullOrBlank() && !persistedPairingToken.isNullOrBlank()) {
            cachedToken = persistedPairingToken
        }
        return try {
            postWebhook(
                port,
                pathPrefix,
                webhookSecret,
                loopbackAdminSecret,
                message,
                allowRepair = true,
            )
        } catch (e: ConnectException) {
            Reply(null, null, "agent not running", gatewayDown = true, pairingToken = cachedToken)
        } catch (e: IOException) {
            // Refused/host-unreachable often arrives as a bare IOException on Android.
            val down = (e.message ?: "").let {
                it.contains("refused", true) || it.contains("ECONNREFUSED", true) ||
                    it.contains("unreachable", true) || it.contains("Failed to connect", true)
            }
            Reply(
                null,
                null,
                e.message ?: "network error",
                gatewayDown = down,
                pairingToken = cachedToken,
            )
        } catch (e: Exception) {
            Reply(
                null,
                null,
                e.message ?: "unexpected error",
                gatewayDown = false,
                pairingToken = cachedToken,
            )
        }
    }

    private fun postWebhook(
        port: Int,
        pathPrefix: String,
        webhookSecret: String?,
        loopbackAdminSecret: String?,
        message: String,
        allowRepair: Boolean,
    ): Reply {
        val token = cachedToken ?: pair(port, pathPrefix, loopbackAdminSecret)
        val body = JSONObject().put("message", message).toString()
        val headers = mutableMapOf<String, String>()
        if (token != null) headers["Authorization"] = "Bearer $token"
        if (!webhookSecret.isNullOrBlank()) headers["X-Webhook-Secret"] = webhookSecret
        val (code, text) = httpPost(
            base(port, pathPrefix) + "/webhook", body, headers
        )
        if (code == 401 || code == 403) {
            // Stale/absent token — re-pair once, then retry.
            cachedToken = null
            if (allowRepair && pair(port, pathPrefix, loopbackAdminSecret) != null) {
                return postWebhook(
                    port,
                    pathPrefix,
                    webhookSecret,
                    loopbackAdminSecret,
                    message,
                    allowRepair = false,
                )
            }
            return Reply(null, null, "not authorized (pairing failed)", pairingToken = null)
        }
        if (code !in 200..299) {
            val msg = runCatching { JSONObject(text).optString("error") }.getOrNull()
            return Reply(
                null,
                null,
                msg?.takeIf { it.isNotBlank() } ?: "gateway error $code",
                pairingToken = cachedToken,
            )
        }
        val json = runCatching { JSONObject(text) }.getOrNull()
            ?: return Reply(
                text,
                null,
                null,
                pairingToken = cachedToken,
            )   // non-JSON body — surface it verbatim
        return Reply(
            response = json.optString("response").ifBlank { json.optString("message").ifBlank { text } },
            model = json.optString("model").takeIf { it.isNotBlank() },
            error = json.optString("error").takeIf { it.isNotBlank() },
            pairingToken = cachedToken,
        )
    }

    /**
     * Obtain a bearer token by minting a fresh pairing code (localhost admin) and exchanging it.
     * Returns null when pairing is disabled (no token needed) or when the handshake fails — callers
     * treat a null token as "send unauthenticated and let the webhook decide".
     */
    private fun pair(port: Int, pathPrefix: String, loopbackAdminSecret: String?): String? {
        val gateway = base(port, pathPrefix)
        val mintHeaders = if (loopbackAdminSecret.isNullOrBlank()) {
            emptyMap()
        } else {
            mapOf("X-Loopback-Admin-Secret" to loopbackAdminSecret)
        }
        val (mintCode, mintBody) = httpPost(
            gateway + "/admin/paircode/new",
            "",
            mintHeaders,
        )
        val code = runCatching { JSONObject(mintBody).optString("pairing_code") }.getOrNull()
        if (mintCode !in 200..299 || code.isNullOrBlank() || code == "null") return null
        val (pairCode, pairBody) = httpPost(gateway + "/pair", "", mapOf("X-Pairing-Code" to code))
        if (pairCode !in 200..299) return null
        val token = runCatching { JSONObject(pairBody).optString("token") }.getOrNull()
        return token?.takeIf { it.isNotBlank() }?.also { cachedToken = it }
    }

    /** POST [body] with [headers]; returns (statusCode, responseText). Connection errors propagate. */
    private fun httpPost(urlStr: String, body: String, headers: Map<String, String>): Pair<Int, String> {
        val conn = URL(urlStr).openConnection() as HttpURLConnection
        return try {
            conn.requestMethod = "POST"
            conn.connectTimeout = CONNECT_TIMEOUT_MS
            conn.readTimeout = READ_TIMEOUT_MS
            conn.doOutput = true
            conn.setRequestProperty("Content-Type", "application/json")
            for ((k, v) in headers) conn.setRequestProperty(k, v)
            conn.outputStream.use { it.write(body.toByteArray(Charsets.UTF_8)) }
            val code = conn.responseCode
            val stream = if (code in 200..299) conn.inputStream else conn.errorStream
            val text = stream?.bufferedReader()?.use { it.readText() } ?: ""
            code to text
        } finally {
            conn.disconnect()
        }
    }
}
