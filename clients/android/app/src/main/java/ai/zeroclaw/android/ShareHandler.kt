package ai.zeroclaw.android

import android.content.Intent
import android.net.Uri

/**
 * Handles content shared TO ZeroClaw from other apps.
 *
 * Supports:
 * - Plain text
 * - URLs
 * - Images (future)
 * - Files (future)
 */
object ShareHandler {

    sealed class SharedContent {
        data class Text(val text: String) : SharedContent()
        data class Url(val url: String, val title: String? = null) : SharedContent()
        data class Image(val uri: Uri) : SharedContent()
        data class File(val uri: Uri, val mimeType: String) : SharedContent()
        object None : SharedContent()
    }

    /**
     * Parse incoming share intent
     */
    fun parseIntent(intent: Intent): SharedContent {
        if (intent.action != Intent.ACTION_SEND) {
            return SharedContent.None
        }

        val type = intent.type ?: return SharedContent.None

        return when {
            type == "text/plain" -> parseTextIntent(intent)
            type == "text/uri-list" -> parseUriListIntent(intent)
            type.startsWith("image/") -> parseImageIntent(intent)
            else -> parseFileIntent(intent, type)
        }
    }

    private fun parseTextIntent(intent: Intent): SharedContent {
        val text = intent.getStringExtra(Intent.EXTRA_TEXT) ?: return SharedContent.None

        // Check if it's a URL
        if (text.startsWith("http://") || text.startsWith("https://")) {
            val title = intent.getStringExtra(Intent.EXTRA_SUBJECT)
            return SharedContent.Url(text, title)
        }

        return SharedContent.Text(text)
    }

    private fun parseUriListIntent(intent: Intent): SharedContent {
        val text = intent.getStringExtra(Intent.EXTRA_TEXT) ?: return SharedContent.None
        // text/uri-list contains URLs separated by newlines
        val firstUrl = text.lines().firstOrNull { it.startsWith("http://") || it.startsWith("https://") }
        return if (firstUrl != null) {
            val title = intent.getStringExtra(Intent.EXTRA_SUBJECT)
            SharedContent.Url(firstUrl, title)
        } else {
            SharedContent.Text(text)
        }
    }

    private fun parseImageIntent(intent: Intent): SharedContent {
        val uri = if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
        } else {
            @Suppress("DEPRECATION")
            intent.getParcelableExtra(Intent.EXTRA_STREAM)
        }

        return uri?.let { SharedContent.Image(it) } ?: SharedContent.None
    }

    private fun parseFileIntent(intent: Intent, mimeType: String): SharedContent {
        val uri = if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
        } else {
            @Suppress("DEPRECATION")
            intent.getParcelableExtra(Intent.EXTRA_STREAM)
        }

        return uri?.let { SharedContent.File(it, mimeType) } ?: SharedContent.None
    }

    /**
     * Generate a prompt from shared content
     */
    fun generatePrompt(content: SharedContent): String {
        return when (content) {
            is SharedContent.Text -> "I'm sharing this text with you:\n\n${content.text}"
            is SharedContent.Url -> {
                val title = content.title?.let { "\"$it\"\n" } ?: ""
                "${title}I'm sharing this URL: ${content.url}\n\nPlease summarize or help me with this."
            }
            is SharedContent.Image -> "I'm sharing an image with you. [Image attached]"
            is SharedContent.File -> "I'm sharing a file with you. [File: ${content.mimeType}]"
            SharedContent.None -> ""
        }
    }
}
