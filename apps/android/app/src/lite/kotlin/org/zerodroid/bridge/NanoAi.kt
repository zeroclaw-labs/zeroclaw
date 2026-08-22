package org.zerodroid.bridge

import android.content.Context
import org.json.JSONObject

/**
 * `lite` flavor (minSdk 30, no Google AI Edge SDK): on-device Gemini Nano is omitted. Cloud
 * providers (DeepSeek/Gemini/etc.) are unaffected —
 * the agent still reaches any of the 61 providers. Only the local Nano endpoints are stubbed.
 */
object NanoAi {
    fun status(ctx: Context): JSONObject =
        JSONObject().put("aicore_installed", false).put("model", "gemini-nano")
            .put("note", "on-device Gemini Nano is not in the lite build; use a cloud provider")

    fun generate(ctx: Context, prompt: String?): JSONObject =
        JSONObject().put("error", "unavailable")
            .put("hint", "on-device Gemini Nano is not in the lite build (no AI Edge SDK)")
}
