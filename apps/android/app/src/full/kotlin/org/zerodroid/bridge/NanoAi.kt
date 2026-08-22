package org.zerodroid.bridge

import android.content.Context
import com.google.ai.edge.aicore.GenerativeModel
import com.google.ai.edge.aicore.generationConfig
import kotlinx.coroutines.runBlocking
import org.json.JSONObject

/**
 * `full` flavor: on-device Gemini Nano via the Google AI Edge SDK (AICore). Requires minSdk 31 +
 * AICore-capable hardware (Pixel 8 Pro/9, Galaxy S24+). The `lite` flavor swaps in a stub.
 */
object NanoAi {
    fun status(ctx: Context): JSONObject {
        val hasAicore = try { ctx.packageManager.getPackageInfo("com.google.android.aicore", 0); true }
            catch (e: Exception) { false }
        return JSONObject().put("aicore_installed", hasAicore).put("model", "gemini-nano")
            .put("note", if (hasAicore) "AICore present — try /ai/generate?prompt="
                else "Gemini Nano needs AICore (Pixel 8 Pro / 9, Galaxy S24+). Not on this device.")
    }

    fun generate(ctx: Context, prompt: String?): JSONObject {
        if (prompt.isNullOrBlank()) return JSONObject().put("error", "need prompt=")
        return try {
            val model = GenerativeModel(generationConfig {
                context = ctx.applicationContext
                temperature = 0.2f
                topK = 16
                maxOutputTokens = 256
            })
            val resp = runBlocking { model.generateContent(prompt) }
            try { model.close() } catch (e: Throwable) {}
            JSONObject().put("model", "gemini-nano").put("on_device", true).put("text", resp.text)
        } catch (t: Throwable) {
            JSONObject().put("error", t.javaClass.simpleName)
                .put("message", t.message ?: "")
                .put("hint", "on-device Gemini Nano unavailable — requires AICore (Pixel 8 Pro/9, S24+)")
        }
    }
}
