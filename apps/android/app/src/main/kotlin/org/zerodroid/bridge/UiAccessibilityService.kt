package org.zerodroid.bridge

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.content.BroadcastReceiver
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.graphics.Bitmap
import android.graphics.Path
import android.graphics.Rect
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.ResultReceiver
import android.util.DisplayMetrics
import android.util.Log
import android.view.Display
import android.view.WindowManager
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import androidx.core.content.ContextCompat
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream

/**
 * In-APK AccessibilityService that reads and controls the screen of any app on an unrooted phone.
 *
 * Ported from CellClaw's CellClawAccessibility; see `apps/android/NOTICE`. The differences
 * from the reference are deliberate:
 *  - speaks `docs/book/src/tools/android-bridge-protocol.md` — [readScreen] emits
 *    PROTOCOL `Node` objects and results carry an `error_code` from the contract's closed set, so
 *    [UiSocketServer] only has to wrap them in the `{ok, data|error}` envelope;
 *  - no kotlinx.serialization / coroutines — uses org.json (already a dep) and lets the broker
 *    ([UiBridge]) block on a CountDownLatch, matching the rest of the bridge;
 *  - no product-specific voice hotkey, no app-tuned swipe geometry — [handleSwipe] takes explicit
 *    coordinates from the client; screen size is read from the real display, never hard-coded;
 *  - [handleScreenshot] returns a file path (PNG under cacheDir) rather than inline bytes, because a
 *    base64 screenshot would blow the ~1 MB Binder transaction limit of the ResultReceiver. The
 *    socket server reads + base64-encodes + unlinks the file.
 *
 * The service is driven by an app-private broadcast ([ACTION_COMMAND]); each command carries a
 * ResultReceiver the service answers through. It runs in the default process (not a separate one),
 * so the receiver is registered NOT_EXPORTED — only our own UID can command it.
 */
class UiAccessibilityService : AccessibilityService() {

    // Real display size, filled on connect and refreshed before any gesture. Never hard-coded to a
    // specific device — a11y controls whatever phone it is enabled on.
    private var screenWidth = 0
    private var screenHeight = 0

    // onReceive runs on the main thread; a Handler lets us settle the screen (hide the floating
    // bubble) before a capture/gesture without blocking the caller.
    private val mainHandler = Handler(Looper.getMainLooper())

    private val actionReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (intent.action != ACTION_COMMAND) return
            val resultReceiver = intent.getResultReceiverCompat("result_receiver")
            val action = intent.getStringExtra("action") ?: return

            when (action) {
                "read" -> sendResult(resultReceiver, readScreen(intent.getIntExtra("max_depth", 15)))
                "foreground" -> sendResult(resultReceiver, foreground())
                // Coordinate gestures inject at screen points; hide the floating bubble first so it
                // can't intercept the tap (mirrors cellclaw AppControlTool's requestHide(500)).
                "tap" -> deferGesture(resultReceiver, HIDE_GESTURE_MS) {
                    guardedMutation(intent) {
                        handleTap(
                            intent.getIntExtra("x", Int.MIN_VALUE),
                            intent.getIntExtra("y", Int.MIN_VALUE),
                            intent.getStringExtra("text")
                        )
                    }
                }
                "swipe" -> deferGesture(resultReceiver, HIDE_GESTURE_MS) {
                    guardedMutation(intent) {
                        handleSwipe(
                            intent.getIntExtra("x1", 0), intent.getIntExtra("y1", 0),
                            intent.getIntExtra("x2", 0), intent.getIntExtra("y2", 0),
                            intent.getIntExtra("duration_ms", 300).toLong()
                        )
                    }
                }
                "scroll" -> deferGesture(resultReceiver, HIDE_GESTURE_MS) {
                    guardedMutation(intent) {
                        handleScroll(
                            intent.getStringExtra("direction") ?: "forward",
                            intent.getIntExtra("x", Int.MIN_VALUE),
                            intent.getIntExtra("y", Int.MIN_VALUE)
                        )
                    }
                }
                "text" -> sendResult(resultReceiver, guardedMutation(intent) {
                    handleType(intent.getStringExtra("text") ?: "")
                })
                "key" -> sendResult(resultReceiver, guardedMutation(intent) {
                    handleKey(intent.getStringExtra("key") ?: "")
                })
                "dialog" -> sendResult(resultReceiver, guardedMutation(intent, systemDialog = true) {
                    handleSystemDialog(intent.getStringExtra("button") ?: "")
                })
                // Hide the bubble so it never lands in the capture (mirrors cellclaw
                // ScreenCaptureTool's requestHide(800)); answered async from the screenshot callback.
                "screenshot" -> deferScreenshot(resultReceiver, intent.getIntExtra("max_width", 540))
                else -> sendResult(resultReceiver, err("unsupported_op", "Unknown action: $action"))
            }
        }
    }

    private fun sendResult(receiver: ResultReceiver?, result: JSONObject) {
        receiver?.send(0, Bundle().apply { putString("result", result.toString()) })
    }

    /**
     * Run a coordinate gesture with the floating bubble hidden. When no bubble is showing this is a
     * straight synchronous call (no added latency); otherwise the bubble is asked to hide, and the
     * gesture is dispatched after a short settle so the hidden window is composited out first.
     */
    private fun deferGesture(receiver: ResultReceiver?, hideMs: Long, action: () -> JSONObject) {
        if (!OverlayVisibilityController.isActive()) { sendResult(receiver, action()); return }
        OverlayVisibilityController.requestHide(hideMs)
        mainHandler.postDelayed({ sendResult(receiver, action()) }, SETTLE_MS)
    }

    /** As [deferGesture] but for the async screenshot path. */
    private fun deferScreenshot(receiver: ResultReceiver?, maxWidth: Int) {
        if (!OverlayVisibilityController.isActive()) { handleScreenshot(receiver, maxWidth); return }
        OverlayVisibilityController.requestHide(HIDE_SCREENSHOT_MS)
        mainHandler.postDelayed({ handleScreenshot(receiver, maxWidth) }, SETTLE_MS)
    }

    override fun onServiceConnected() {
        super.onServiceConnected()
        updateScreenDimensions()
        val filter = IntentFilter(ACTION_COMMAND)
        // Same-process, same-UID command channel: AndroidX backports NOT_EXPORTED enforcement to
        // API 30-32, where plain registerReceiver(receiver, filter) would otherwise leave this
        // powerful accessibility command surface reachable by other apps. The broker additionally
        // setPackage()s every outgoing broadcast to us.
        ContextCompat.registerReceiver(
            this,
            actionReceiver,
            filter,
            ContextCompat.RECEIVER_NOT_EXPORTED
        )
        Log.i(TAG, "UI accessibility service connected (${screenWidth}x${screenHeight})")
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) { /* pull model — no event work */ }

    override fun onInterrupt() {}

    override fun onDestroy() {
        // onDestroy can fire before onServiceConnected wired the receiver; guard the unregister.
        runCatching { unregisterReceiver(actionReceiver) }
        super.onDestroy()
    }

    private fun updateScreenDimensions() {
        val wm = getSystemService(Context.WINDOW_SERVICE) as WindowManager
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            val bounds = wm.currentWindowMetrics.bounds
            screenWidth = bounds.width()
            screenHeight = bounds.height()
        } else {
            val dm = DisplayMetrics()
            @Suppress("DEPRECATION")
            wm.defaultDisplay.getRealMetrics(dm)
            screenWidth = dm.widthPixels
            screenHeight = dm.heightPixels
        }
    }

    // ── foreground ──────────────────────────────────────────────────────
    private fun foreground(): JSONObject {
        val pkg = rootInActiveWindow?.packageName?.toString() ?: "unknown"
        return JSONObject().put("package", pkg)
    }

    /** Revalidate the model's target at the final privileged boundary, immediately before input.
     *  The earlier Rust-side foreground call is useful feedback but cannot close the race where a
     *  notification or user gesture changes windows between two socket requests. */
    private fun guardedMutation(
        intent: Intent,
        systemDialog: Boolean = false,
        action: () -> JSONObject,
    ): JSONObject {
        val expected = intent.getStringExtra("expect_package")?.trim()
        val actual = rootInActiveWindow?.packageName?.toString()
        UiSecurityPolicy.mutationRejection(
            expected,
            actual,
            packageName,
            systemDialogPackages,
            systemDialog,
        )?.let {
            return err(it.code, it.message)
        }
        return action()
    }

    // ── tap ─────────────────────────────────────────────────────────────
    private fun handleTap(x: Int, y: Int, text: String?): JSONObject {
        if (text != null) {
            val root = rootInActiveWindow ?: return err("service_unavailable", "No active window")
            val nodes = root.findAccessibilityNodeInfosByText(text)
            if (nodes.isNullOrEmpty()) return err("not_found", "Element not found: '$text'")
            val clickable = findClickableParent(nodes.first()) ?: nodes.first()
            return clicked(clickable, "'$text'")
        }
        if (x == Int.MIN_VALUE || y == Int.MIN_VALUE) return err("bad_args", "tap needs x,y or text")
        val path = Path().apply { moveTo(x.toFloat(), y.toFloat()) }
        val gesture = GestureDescription.Builder()
            .addStroke(GestureDescription.StrokeDescription(path, 0, 100)).build()
        return dispatch(gesture, "tap at ($x,$y)")
    }

    private fun findClickableParent(node: AccessibilityNodeInfo): AccessibilityNodeInfo? {
        var current: AccessibilityNodeInfo? = node
        while (current != null) {
            if (current.isClickable) return current
            current = current.parent
        }
        return null
    }

    // ── swipe (arbitrary stroke; coordinates come from the client) ───────
    private fun handleSwipe(x1: Int, y1: Int, x2: Int, y2: Int, durationMs: Long): JSONObject {
        val dur = durationMs.coerceIn(1, 60_000)
        val path = Path().apply { moveTo(x1.toFloat(), y1.toFloat()); lineTo(x2.toFloat(), y2.toFloat()) }
        val gesture = GestureDescription.Builder()
            .addStroke(GestureDescription.StrokeDescription(path, 0, dur)).build()
        return dispatch(gesture, "swipe ($x1,$y1)->($x2,$y2)")
    }

    // ── scroll ──────────────────────────────────────────────────────────
    private fun handleScroll(direction: String, x: Int, y: Int): JSONObject {
        val forward = when (direction) {
            "forward", "down" -> true
            "backward", "up" -> false
            else -> return err("bad_args", "Invalid scroll direction: $direction")
        }
        val root = rootInActiveWindow
        val scrollable = root?.let {
            if (x != Int.MIN_VALUE && y != Int.MIN_VALUE) findScrollableAt(it, x, y) else null
        } ?: root?.let { findScrollableNode(it) }
        if (scrollable != null) {
            // A scrollable that is already at its end returns false; fall through to the gesture
            // rather than reporting a scroll that did not happen.
            val acted = scrollable.performAction(
                if (forward) AccessibilityNodeInfo.ACTION_SCROLL_FORWARD
                else AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD
            )
            if (acted) return dispatched()
        }
        // Gesture fallback using the real display size (no hard-coded pixels).
        if (screenWidth == 0 || screenHeight == 0) updateScreenDimensions()
        val cx = screenWidth / 2f
        val dist = screenHeight * 0.3f
        val midY = screenHeight / 2f
        val (startY, endY) = if (forward) (midY + dist) to (midY - dist) else (midY - dist) to (midY + dist)
        val path = Path().apply { moveTo(cx, startY); lineTo(cx, endY) }
        val gesture = GestureDescription.Builder()
            .addStroke(GestureDescription.StrokeDescription(path, 0, 300)).build()
        return dispatch(gesture, "scroll $direction")
    }

    private fun findScrollableNode(node: AccessibilityNodeInfo): AccessibilityNodeInfo? {
        if (node.isScrollable) return node
        for (i in 0 until node.childCount) {
            val child = node.getChild(i) ?: continue
            findScrollableNode(child)?.let { return it }
        }
        return null
    }

    private fun findScrollableAt(node: AccessibilityNodeInfo, x: Int, y: Int): AccessibilityNodeInfo? {
        val b = Rect(); node.getBoundsInScreen(b)
        if (!b.contains(x, y)) return null
        for (i in 0 until node.childCount) {
            val child = node.getChild(i) ?: continue
            findScrollableAt(child, x, y)?.let { return it }
        }
        return if (node.isScrollable) node else null
    }

    // ── text (set text on input-focused node) ───────────────────────────
    private fun handleType(text: String): JSONObject {
        val focused = findFocus(AccessibilityNodeInfo.FOCUS_INPUT)
            ?: return err("no_focus", "No input-focused field")

        // Never type into our own UI. The floating bubble's input keeps global input focus after
        // the user submits a prompt, so findFocus returns the bubble's own box and the agent types
        // its reply into the thing it was asked from, instead of into the app on screen. Hiding the
        // overlay does not help: visibility is not focus. The agent drives OTHER apps by
        // definition, so a focused node belonging to this package is always the wrong target.
        if (focused.packageName?.toString() == packageName) {
            return err(
                "no_focus",
                "Input focus is on zerodroid's own overlay, not the target app. Tap the field you " +
                    "want to type into first."
            )
        }
        val args = Bundle().apply {
            putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, text)
        }
        // A field can refuse ACTION_SET_TEXT (custom editors, IME-only inputs). Saying "set": true
        // regardless would have the agent compose a reply into a box that never received it.
        if (focused.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, args)) {
            return JSONObject().put("set", true).put("method", "set_text")
        }
        // Fallback: put the text on the clipboard and ask the field to paste it. Many custom
        // editors that reject SET_TEXT still honour ACTION_PASTE, because that is the path the
        // system's own long-press menu uses. Chat composers are the common case, and they are
        // exactly the fields worth typing into.
        if (pasteViaClipboard(focused, text)) {
            return JSONObject().put("set", true).put("method", "paste")
        }
        return err("internal", "focused field refused both ACTION_SET_TEXT and ACTION_PASTE")
    }

    /**
     * Clipboard-then-paste fallback. The user's existing clipboard is saved and restored on a
     * delay, since silently eating whatever they had copied is a nasty side effect of a tool call.
     * Restoration is best-effort: it must not run before the paste has been consumed.
     */
    private fun pasteViaClipboard(node: AccessibilityNodeInfo, text: String): Boolean {
        val cm = getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager ?: return false
        val previous = runCatching { cm.primaryClip }.getOrNull()
        return try {
            cm.setPrimaryClip(ClipData.newPlainText("zerodroid", text))
            // Focus first: ACTION_PASTE targets the node, and some editors ignore it unless the
            // node also holds input focus.
            node.performAction(AccessibilityNodeInfo.ACTION_FOCUS)
            node.performAction(AccessibilityNodeInfo.ACTION_PASTE)
        } catch (e: Exception) {
            Log.w(TAG, "clipboard paste fallback failed: ${e.message}")
            false
        } finally {
            if (previous != null) {
                mainHandler.postDelayed({
                    runCatching { cm.setPrimaryClip(previous) }
                }, CLIPBOARD_RESTORE_MS)
            }
        }
    }

    // ── key (global actions + IME enter) ────────────────────────────────
    private fun handleKey(key: String): JSONObject {
        val acted = when (key) {
            "back" -> performGlobalAction(GLOBAL_ACTION_BACK)
            "home" -> performGlobalAction(GLOBAL_ACTION_HOME)
            "recents" -> performGlobalAction(GLOBAL_ACTION_RECENTS)
            "enter" -> {
                val focused = findFocus(AccessibilityNodeInfo.FOCUS_INPUT)
                    ?: return err("no_focus", "No input-focused field for enter")
                if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
                    return err("unsupported_op", "enter needs Android 11+")
                }
                focused.performAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_IME_ENTER.id)
            }
            else -> return err("bad_args", "Unknown key: $key")
        }
        if (!acted) return err("internal", "key '$key' was refused by the system")
        return dispatched()
    }

    // ── read (UI tree → PROTOCOL nodes) ─────────────────────────────────
    private fun readScreen(maxDepth: Int): JSONObject {
        val root = rootInActiveWindow
            ?: return JSONObject().put("foreground_package", "unknown")
                .put("system_dialog", false).put("nodes", JSONArray())
        val pkg = root.packageName?.toString() ?: "unknown"
        UiSecurityPolicy.observationRejection(pkg, packageName)?.let {
            return err(it.code, it.message)
        }
        val nodes = JSONArray()
        traverseNode(root, nodes, maxDepth, 0)

        val out = JSONObject().put("foreground_package", pkg)
        val isDialog = pkg in systemDialogPackages
        out.put("system_dialog", isDialog)
        if (isDialog) {
            val buttons = JSONArray()
            for (i in 0 until nodes.length()) {
                val n = nodes.getJSONObject(i)
                if (n.optBoolean("clickable")) {
                    (n.optString("text").ifEmpty { n.optString("desc") })
                        .takeIf { it.isNotBlank() }?.let { buttons.put(it) }
                }
            }
            out.put("dialog", JSONObject().put("kind", classifyDialog(pkg, nodes)).put("buttons", buttons))
        }
        out.put("nodes", nodes)
        return out
    }

    private fun traverseNode(node: AccessibilityNodeInfo, out: JSONArray, maxDepth: Int, depth: Int) {
        if (depth > maxDepth) return
        val password = node.isPassword
        val visible = UiSecurityPolicy.visibleText(
            password,
            node.text?.toString(),
            node.contentDescription?.toString(),
        )
        val text = visible.text
        val desc = visible.description
        // Mirror CellClaw's filter: keep only nodes with text, a description, or that are interactive.
        if (text != null || desc != null || node.isClickable || node.isEditable) {
            val b = Rect(); node.getBoundsInScreen(b)
            val n = JSONObject()
            if (!text.isNullOrEmpty()) n.put("text", text)
            if (!desc.isNullOrEmpty()) n.put("desc", desc)
            node.className?.toString()?.let { n.put("class", it) }
            node.viewIdResourceName?.let { n.put("resource_id", it) }
            n.put("clickable", node.isClickable)
            n.put("editable", node.isEditable)
            if (password) n.put("password", true)
            n.put("bounds", JSONObject().put("l", b.left).put("t", b.top).put("r", b.right).put("b", b.bottom))
            n.put("center", JSONObject().put("x", b.centerX()).put("y", b.centerY()))
            out.put(n)
        }
        for (i in 0 until node.childCount) {
            val child = node.getChild(i) ?: continue
            traverseNode(child, out, maxDepth, depth + 1)
        }
    }

    /** Known packages whose foreground window is a system dialog (permission / install prompts). */
    private val systemDialogPackages = setOf(
        "com.google.android.permissioncontroller",
        "com.android.permissioncontroller",
        "com.android.packageinstaller",
        "com.google.android.packageinstaller",
        "android",
        "com.android.systemui"
    )

    private fun classifyDialog(pkg: String, nodes: JSONArray): String {
        val texts = buildList {
            for (i in 0 until nodes.length()) {
                val n = nodes.getJSONObject(i)
                if (!n.optBoolean("clickable")) n.optString("text").takeIf { it.isNotBlank() }?.let { add(it) }
            }
        }
        return when {
            texts.any { it.contains("allow", true) && it.contains("access", true) } -> "permission_request"
            texts.any { it.contains("permission", true) } -> "permission_request"
            texts.any { it.contains("install", true) } -> "install_prompt"
            pkg == "com.android.systemui" -> "system_prompt"
            else -> "system_dialog"
        }
    }

    // ── dialog (click a system-dialog button, synonym-expanded) ─────────
    private fun handleSystemDialog(button: String): JSONObject {
        val root = rootInActiveWindow ?: return err("service_unavailable", "No active window")
        val pkg = root.packageName?.toString() ?: "unknown"
        if (pkg !in systemDialogPackages) return err("not_found", "No system dialog showing (foreground: $pkg)")

        // Try the literal label first, then the synonyms. A label that is present but whose node
        // refuses the click is treated as a miss so the next synonym still gets a turn.
        val alternatives = when (button.lowercase()) {
            "allow" -> listOf("Allow", "ALLOW", "While using the app", "Only this time")
            "deny" -> listOf("Deny", "DENY", "Don't allow", "Don't Allow")
            else -> return err("bad_args", "dialog button must be allow or deny")
        }
        for (label in alternatives) {
            val nodes = root.findAccessibilityNodeInfosByText(label)
            if (nodes.isNullOrEmpty()) continue
            val target = findClickableParent(nodes.first()) ?: nodes.first()
            if (target.performAction(AccessibilityNodeInfo.ACTION_CLICK)) {
                return JSONObject().put("handled", true)
            }
        }
        return err("not_found", "Button '$button' not found (or refused the click) in system dialog")
    }

    // ── screenshot (PNG to cacheDir; server reads + base64s + unlinks) ──
    private fun handleScreenshot(receiver: ResultReceiver?, maxWidth: Int) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
            sendResult(receiver, err("screenshot_failed", "takeScreenshot needs Android 11+ (API 30)"))
            return
        }
        val pkg = rootInActiveWindow?.packageName?.toString()
        UiSecurityPolicy.observationRejection(pkg, packageName)?.let {
            sendResult(receiver, err(it.code, it.message))
            return
        }
        val width = maxWidth.coerceIn(64, 4096)
        takeScreenshot(Display.DEFAULT_DISPLAY, mainExecutor, object : TakeScreenshotCallback {
            override fun onSuccess(result: ScreenshotResult) {
                var hb = result.hardwareBuffer
                try {
                    val raw = Bitmap.wrapHardwareBuffer(hb, result.colorSpace)
                        ?: return sendResult(receiver, err("screenshot_failed", "null bitmap"))
                    val soft = raw.copy(Bitmap.Config.ARGB_8888, false)
                    raw.recycle()
                    // max_width is a ceiling, not a target: never upscale a screen that is already
                    // narrower, which would only spend bytes to invent pixels.
                    val outW = minOf(width, soft.width)
                    val scaledH = (soft.height * (outW.toFloat() / soft.width)).toInt().coerceAtLeast(1)
                    val scaled = Bitmap.createScaledBitmap(soft, outW, scaledH, true)
                    if (scaled !== soft) soft.recycle()

                    // Refuse to hand back a frame with nothing in it. Slow-rendering apps are
                    // routinely captured mid-draw, and a blank image is the worst possible output:
                    // the model cannot tell "nothing rendered" from "nothing is there", so it fills
                    // the gap with plausible fiction. An error makes it retry instead.
                    val uniform = uniformFraction(scaled)
                    if (uniform >= BLANK_FRACTION) {
                        scaled.recycle()
                        return sendResult(receiver, err("screenshot_failed",
                            "blank frame: ${(uniform * 100).toInt()}% one colour — the screen was " +
                                "probably mid-render, capture again"))
                    }

                    val dir = File(cacheDir, "screenshots").apply { mkdirs() }
                    val file = File(dir, "ui_${System.currentTimeMillis()}.png")
                    FileOutputStream(file).use { scaled.compress(Bitmap.CompressFormat.PNG, 90, it) }
                    val w = scaled.width; val h = scaled.height
                    scaled.recycle()
                    sendResult(receiver, JSONObject().put("file_path", file.absolutePath).put("width", w).put("height", h))
                } catch (e: Exception) {
                    sendResult(receiver, err("screenshot_failed", "save failed: ${e.message}"))
                } finally {
                    runCatching { hb.close() }
                }
            }
            override fun onFailure(errorCode: Int) {
                sendResult(receiver, err("screenshot_failed", "error code $errorCode"))
            }
        })
    }

    /**
     * Fraction of sampled pixels sharing the single most common colour. A real UI is busy; a
     * mid-render or blanked frame is overwhelmingly one flat colour. Sampled on a grid rather than
     * per-pixel so this stays cheap on every capture.
     *
     * The status bar always renders, so a "blank" screen is never literally 100% uniform — the
     * threshold has to sit below that.
     */
    private fun uniformFraction(bmp: Bitmap): Double {
        val counts = HashMap<Int, Int>()
        var n = 0
        val stepX = maxOf(1, bmp.width / BLANK_SAMPLES)
        val stepY = maxOf(1, bmp.height / BLANK_SAMPLES)
        var y = 0
        while (y < bmp.height) {
            var x = 0
            while (x < bmp.width) {
                val c = bmp.getPixel(x, y)
                counts[c] = (counts[c] ?: 0) + 1
                n++
                x += stepX
            }
            y += stepY
        }
        if (n == 0) return 0.0
        return (counts.values.maxOrNull() ?: 0).toDouble() / n
    }

    // ── helpers ─────────────────────────────────────────────────────────
    private fun dispatched() = JSONObject().put("dispatched", true)

    /**
     * Dispatch a gesture and report what actually happened. `dispatchGesture` returns false when
     * the system refuses the stroke (another gesture in flight, service not in a state to inject),
     * and reporting `dispatched: true` regardless would tell the agent a tap landed when nothing
     * touched the screen, so it would carry on reasoning from a screen that never changed.
     */
    private fun dispatch(gesture: GestureDescription, what: String): JSONObject =
        if (dispatchGesture(gesture, null, null)) dispatched()
        else err("internal", "system refused to dispatch $what")

    /** As [dispatch] but for node clicks, which can be refused just as silently. */
    private fun clicked(node: AccessibilityNodeInfo, what: String): JSONObject =
        if (node.performAction(AccessibilityNodeInfo.ACTION_CLICK)) dispatched()
        else err("internal", "element $what refused the click")

    private fun err(code: String, message: String) =
        JSONObject().put("error_code", code).put("error", message)

    @Suppress("DEPRECATION")
    private fun Intent.getResultReceiverCompat(key: String): ResultReceiver? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU)
            getParcelableExtra(key, ResultReceiver::class.java)
        else getParcelableExtra(key)

    companion object {
        private const val TAG = "zerodroid-ui-a11y"
        const val ACTION_COMMAND = "org.zerodroid.bridge.UI_ACTION"

        // Bubble hide windows before a capture/gesture (mirror cellclaw: 800ms screenshot, 500ms
        // tap/swipe) and the settle delay that lets the hidden window composite out first.
        private const val HIDE_SCREENSHOT_MS = 800L
        private const val HIDE_GESTURE_MS = 500L
        private const val SETTLE_MS = 150L

        // Long enough that the target app has consumed the paste before we hand the user
        // their own clipboard back.
        private const val CLIPBOARD_RESTORE_MS = 1500L

        // Blank-frame guard. 92% rather than ~100% because the status bar and nav bar always
        // render even when the app body has not, so a genuinely useless frame still carries a
        // sliver of real pixels.
        private const val BLANK_FRACTION = 0.92
        private const val BLANK_SAMPLES = 80
    }
}
