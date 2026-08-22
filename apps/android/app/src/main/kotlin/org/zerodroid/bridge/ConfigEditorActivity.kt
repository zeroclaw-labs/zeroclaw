package org.zerodroid.bridge

import android.graphics.Color
import android.graphics.Typeface
import android.os.Bundle
import android.text.InputType
import android.view.View
import android.view.WindowManager
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.widget.*
import androidx.appcompat.app.AlertDialog
import androidx.appcompat.app.AppCompatActivity

/**
 * Full config surface: edit config.toml raw, validate it against the bundled binary, and browse
 * every setting (`zeroclaw config list`). This is the escape hatch beyond the curated home screen —
 * any of the agent/risk/runtime/gateway/channel/provider keys the binary supports.
 *
 * Saving raw config flips the app into MANUAL mode so it stops regenerating config.toml on Start.
 */
class ConfigEditorActivity : AppCompatActivity() {

    private lateinit var cfg: ConfigStore
    private lateinit var rt: NativeRuntime
    private lateinit var editor: EditText
    private lateinit var status: TextView

    private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Raw config can contain provider and gateway secrets; never expose it to screenshots,
        // recent-app thumbnails, screen sharing, or another capture service.
        window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        cfg = ConfigStore(applicationContext)
        rt = NativeRuntime(applicationContext)
        setContentView(buildUi())
        load()
    }

    private fun buildUi(): View {
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(16), dp(16), dp(16), dp(16))
        }
        root.addView(TextView(this).apply {
            text = "Edit config.toml"; textSize = 20f; setTypeface(typeface, Typeface.BOLD)
        })
        root.addView(TextView(this).apply {
            text = "Full agent config. Save flips on manual mode (the app stops regenerating this " +
                "file). Validate runs the bundled binary against your edit."
            textSize = 12f; setTextColor(Color.GRAY); setPadding(0, dp(2), 0, dp(8))
        })
        status = TextView(this).apply { textSize = 12f; setPadding(0, dp(4), 0, dp(4)) }
        root.addView(status)

        editor = EditText(this).apply {
            typeface = Typeface.MONOSPACE; textSize = 12f
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE or
                InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
            gravity = android.view.Gravity.TOP
            setHorizontallyScrolling(true)
            setBackgroundColor(Color.parseColor("#101010")); setTextColor(Color.parseColor("#D0D0D0"))
            setPadding(dp(8), dp(8), dp(8), dp(8))
        }
        root.addView(editor, LinearLayout.LayoutParams(MATCH_PARENT, 0, 1f))

        val row1 = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        row1.addView(Button(this).apply { text = "Validate"; setOnClickListener { validate() } }, eq())
        row1.addView(Button(this).apply { text = "Save"; setOnClickListener { save() } }, eq())
        root.addView(row1)

        val row2 = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        row2.addView(Button(this).apply { text = "Browse all settings"; setOnClickListener { browse() } }, eq())
        row2.addView(Button(this).apply { text = "Regenerate from form"; setOnClickListener { regenerate() } }, eq())
        root.addView(row2)
        return root
    }

    private fun eq() = LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f).apply { marginEnd = dp(6) }

    private fun load() {
        status.text = "loading…"
        Thread {
            // Read prefs (keystore) + config OFF the main thread.
            val manual = cfg.manualConfig
            val text = try { cfg.readRawConfig(rt) } catch (e: Exception) { "# error reading config: ${e.message}\n" }
            runOnUiThread {
                editor.setText(text)
                status.text = if (manual) "mode: MANUAL (you own this file)" else "mode: managed (form-generated)"
            }
        }.apply { isDaemon = true }.start()
    }

    private fun validate() {
        val text = editor.text.toString()
        status.text = "validating…"
        Thread {
            val ok: Boolean; val msg: String
            try {
                val test = NativeRuntime(applicationContext, variant = "edit")
                test.configDir.mkdirs()
                test.configFile.writeText(if (text.endsWith("\n")) text else text + "\n")
                // Fully isolated env (HOME/TMPDIR/cwd of the edit variant) + authoritative exit code.
                val (code, outNullable) = test.execStatus(
                    listOf(test.zeroclawBin.absolutePath, "config", "list", "--config-dir", test.configDir.absolutePath),
                    15_000
                ) { test.applyEnv(it) }
                val out = outNullable ?: ""
                ok = code == 0
                msg = if (ok) "✓ valid — parses cleanly"
                      else "✗ invalid (exit ${code ?: "timeout"}):\n" + out.lines().filter { it.isNotBlank() }.takeLast(6).joinToString("\n")
            } catch (e: Exception) { return@Thread runOnUiThread { status.text = "validate error: ${e.message}" } }
            runOnUiThread { status.text = msg }
        }.apply { isDaemon = true }.start()
    }

    private fun save() {
        val text = editor.text.toString()
        Thread {
            try {
                cfg.writeRawConfig(rt, text)
                cfg.manualConfig = true
                runOnUiThread {
                    status.text = "saved — MANUAL mode on. Stop & Start the agent to apply."
                    toast("Saved. Manual mode on.")
                }
            } catch (e: Exception) { runOnUiThread { status.text = "save failed: ${e.message}" } }
        }.apply { isDaemon = true }.start()
    }

    private fun regenerate() {
        AlertDialog.Builder(this)
            .setTitle("Regenerate from form?")
            .setMessage("Overwrite config.toml from the home-screen settings and turn manual mode OFF. Your raw edits will be lost.")
            .setPositiveButton("Regenerate") { _, _ ->
                Thread {
                    try {
                        cfg.manualConfig = false
                        cfg.writeConfig(rt)
                        val text = cfg.readRawConfig(rt)
                        runOnUiThread { editor.setText(text); status.text = "mode: managed (form-generated)" }
                    } catch (e: Exception) { runOnUiThread { status.text = "regenerate failed: ${e.message}" } }
                }.apply { isDaemon = true }.start()
            }
            .setNegativeButton("Cancel", null).show()
    }

    private fun browse() {
        status.text = "loading settings…"
        Thread {
            val out = rt.execWithTimeout(
                listOf(rt.zeroclawBin.absolutePath, "config", "list", "--config-dir", rt.configDir.absolutePath),
                15_000
            ) { rt.applyEnv(it) } ?: "(no output)"
            runOnUiThread {
                status.text = ""
                val tv = TextView(this).apply {
                    text = out; typeface = Typeface.MONOSPACE; textSize = 11f; setTextIsSelectable(true)
                    setPadding(dp(12), dp(12), dp(12), dp(12))
                }
                AlertDialog.Builder(this)
                    .setTitle("All settings (secrets masked)")
                    .setView(ScrollView(this).apply { addView(tv) })
                    .setPositiveButton("Close", null).show()
            }
        }.apply { isDaemon = true }.start()
    }

    private fun toast(s: String) = Toast.makeText(this, s, Toast.LENGTH_LONG).show()
}
