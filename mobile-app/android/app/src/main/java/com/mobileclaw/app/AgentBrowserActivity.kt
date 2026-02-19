package com.mobileclaw.app

import android.annotation.SuppressLint
import android.os.Bundle
import android.webkit.WebChromeClient
import android.webkit.WebSettings
import android.webkit.WebView
import androidx.appcompat.app.AppCompatActivity
import java.lang.ref.WeakReference
import java.net.HttpURLConnection
import java.net.URL

class AgentBrowserActivity : AppCompatActivity() {

    companion object {
        private var webViewRef: WeakReference<WebView>? = null

        fun hasSession(): Boolean = webViewRef?.get() != null

        fun navigate(url: String): Boolean {
            val webView = webViewRef?.get() ?: return false
            webView.post { webView.loadUrl(url) }
            return true
        }

        fun state(): Map<String, String> {
            val webView = webViewRef?.get()
            return mapOf(
                "url" to (webView?.url ?: ""),
                "title" to (webView?.title ?: ""),
            )
        }

        fun fetchPage(url: String, maxChars: Int): Map<String, String> {
            val connection = (URL(url).openConnection() as HttpURLConnection).apply {
                requestMethod = "GET"
                connectTimeout = 10_000
                readTimeout = 15_000
                instanceFollowRedirects = true
                setRequestProperty("User-Agent", "MobileClawAgentBrowser/1.0")
            }

            return try {
                val code = connection.responseCode
                val input = if (code in 200..399) connection.inputStream else connection.errorStream
                val text = input?.bufferedReader()?.use { reader ->
                    val full = reader.readText()
                    if (full.length > maxChars) full.take(maxChars) else full
                } ?: ""
                mapOf(
                    "status" to code.toString(),
                    "content_type" to (connection.contentType ?: ""),
                    "body" to text,
                )
            } finally {
                connection.disconnect()
            }
        }
    }

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val webView = WebView(this)
        setContentView(webView)

        webView.settings.javaScriptEnabled = true
        webView.settings.domStorageEnabled = true
        webView.settings.cacheMode = WebSettings.LOAD_DEFAULT
        webView.webChromeClient = WebChromeClient()

        webViewRef = WeakReference(webView)

        val url = intent.getStringExtra("url")?.trim().orEmpty().ifBlank { "https://www.google.com" }
        webView.loadUrl(url)
    }

    override fun onDestroy() {
        val active = webViewRef?.get()
        active?.destroy()
        webViewRef = null
        super.onDestroy()
    }
}
