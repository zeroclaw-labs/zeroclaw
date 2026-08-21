package org.zerodroid.bridge

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.Color
import android.graphics.PixelFormat
import android.graphics.drawable.GradientDrawable
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.provider.Settings
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.WindowManager
import android.view.inputmethod.EditorInfo
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import androidx.appcompat.widget.AppCompatTextView
import androidx.core.content.ContextCompat

/**
 * A draggable floating "bubble" that lets the owner drive the on-device agent from any app: tap the
 * bubble → type a task → the agent runs and the reply shows inline. This is the on-device input
 * channel (adb/loopback/wifi are the others).
 *
 * Ported from CellClaw's OverlayService (see `apps/android/NOTICE`) but deliberately stripped and
 * re-pointed:
 *  - cellclaw sent messages IN-PROCESS via an injected AgentLoop and layered on approval queues,
 *    voice, heartbeat, vision/screen-capture tools and live agent-state bubble tinting. All of that
 *    is dropped. This bubble does exactly one thing: POST the text to the local gateway's `/webhook`
 *    ([GatewayClient]) and render the `response`.
 *  - no Hilt: plain foreground [Service], no `@AndroidEntryPoint`/`@Inject`.
 *  - no coroutines (the `lite` flavor has none): a [Handler] drives the auto-restore + submit
 *    hand-back instead of `CoroutineScope`/`delay`.
 *
 * Kept from the reference: the WindowManager bubble on TYPE_APPLICATION_OVERLAY, drag via
 * [BubbleTouchListener], the expand-to-panel input, the outside-tap backdrop, and the
 * [OverlayVisibilityController] hide/restore coordination so screenshots/gestures don't catch it.
 */
class OverlayService : Service() {

    private lateinit var windowManager: WindowManager
    private val main = Handler(Looper.getMainLooper())

    private var bubbleView: TextView? = null
    private lateinit var bubbleParams: WindowManager.LayoutParams

    private var panelView: LinearLayout? = null
    private lateinit var panelParams: WindowManager.LayoutParams
    private var backdropView: View? = null
    private lateinit var backdropParams: WindowManager.LayoutParams
    private var panelVisible = false

    private var inputField: EditText? = null
    private var sendButton: TextView? = null
    private var responseText: TextView? = null

    private var overlayHidden = false
    private val restoreRunnable = Runnable { restoreOverlay() }

    /**
     * CellClaw's state vocabulary, kept intact even where zerodroid does not yet emit a state.
     * The colour mapping below is the source of truth; purple is a panel accent, not a state.
     */
    private enum class BubbleState {
        IDLE,
        MONITORING,
        THINKING,
        EXECUTING_TOOLS,
        WAITING_APPROVAL,
        PAUSED,
        ERROR,
        /** Zerodroid extension: maps to CellClaw's paused/off-duty grey. */
        OFFLINE
    }

    private var bubbleState = BubbleState.OFFLINE
    private var turnInFlight = false

    private val runtimeListener = object : RuntimeState.Listener {
        override fun onGatewayState(state: GatewayProcess.State) {
            main.post { setBubbleState(stateForGateway(state)) }
        }
    }

    private val visibilityListener = OverlayVisibilityController.Listener { durationMs ->
        hideOverlayTemporarily(durationMs)
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        windowManager = getSystemService(WINDOW_SERVICE) as WindowManager
        createChannel()
        goForeground()
        // Never crash if the "draw over other apps" grant is missing/revoked — addView would throw.
        if (Build.VERSION.SDK_INT >= 23 && !Settings.canDrawOverlays(this)) {
            stopSelf()
            return
        }
        try {
            // Derive the very first frame from live state. The old code constructed the bubble in
            // purple and only applied a state colour after the first prompt, so an untouched bubble
            // communicated brand colour rather than agent state.
            bubbleState = stateForGateway(RuntimeState.gatewayState)
            createBubble()
            createPanel()
        } catch (e: Exception) {
            runCatching { bubbleView?.let { windowManager.removeView(it) } }
            stopSelf()
            return
        }
        running = true
        RuntimeState.addListener(runtimeListener)
        // Close the tiny race between the state read above and listener registration.
        setBubbleState(stateForGateway(RuntimeState.gatewayState))
        OverlayVisibilityController.setListener(visibilityListener)
    }

    override fun onDestroy() {
        running = false
        RuntimeState.removeListener(runtimeListener)
        OverlayVisibilityController.setListener(null)
        main.removeCallbacksAndMessages(null)
        if (panelVisible) {
            runCatching { panelView?.let { windowManager.removeView(it) } }
            runCatching { backdropView?.let { windowManager.removeView(it) } }
            panelVisible = false
        }
        runCatching { bubbleView?.let { windowManager.removeView(it) } }
        bubbleView = null
        panelView = null
        backdropView = null
        super.onDestroy()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int = START_STICKY

    private fun dpToPx(dp: Int): Int =
        TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, dp.toFloat(), resources.displayMetrics).toInt()

    // ── Bubble ───────────────────────────────────────────────────────────
    private fun createBubble() {
        val size = dpToPx(48)
        val bubble = BubbleView(this) { togglePanel() }.apply {
            text = "z"
            setTextColor(themeColor(R.color.cellclaw_overlay_text_primary))
            textSize = 22f
            gravity = Gravity.CENTER
            background = GradientDrawable().apply {
                shape = GradientDrawable.OVAL
                setColor(bubbleColor(bubbleState))
            }
        }
        bubbleParams = WindowManager.LayoutParams(
            size, size,
            WindowManager.LayoutParams.TYPE_APPLICATION_OVERLAY,
            WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE or
                WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN or
                WindowManager.LayoutParams.FLAG_SECURE,
            PixelFormat.TRANSLUCENT
        ).apply {
            gravity = Gravity.TOP or Gravity.START
            x = dpToPx(16)
            y = dpToPx(200)
        }
        bubble.setOnTouchListener(BubbleTouchListener(
            windowManager, bubbleParams,
            onDrag = { _, _ -> if (panelVisible) repositionPanel() },
            onLongPress = { openApp() }
        ))
        windowManager.addView(bubble, bubbleParams)
        bubbleView = bubble
    }

    // ── Panel (input + response) ─────────────────────────────────────────
    private fun createPanel() {
        val panel = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            background = GradientDrawable().apply {
                setColor(themeColor(R.color.cellclaw_overlay_surface))
                cornerRadius = dpToPx(16).toFloat()
            }
            setPadding(dpToPx(16), dpToPx(12), dpToPx(16), dpToPx(12))
        }

        // Header: title + open-app + collapse. The circle remains available so the user can
        // reopen chat without returning to zerodroid's main screen.
        val header = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
        }
        header.addView(TextView(this).apply {
            text = "zerodroid"
            setTextColor(themeColor(R.color.cellclaw_overlay_accent_text))
            textSize = 12f
        }, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f))
        header.addView(headerButton("Open") { openApp() })
        header.addView(headerButton("✕") { togglePanel() })   // ✕
        panel.addView(header, LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT))

        // Response area (scrolls, capped height).
        val scroll = ScrollView(this).apply {
            setPadding(0, dpToPx(8), 0, dpToPx(8))
        }
        val resp = TextView(this).apply {
            text = "Tap a task below and I'll run it on your phone."
            setTextColor(themeColor(R.color.cellclaw_overlay_text_primary))
            textSize = 13f
        }
        scroll.addView(resp)
        panel.addView(scroll, LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.MATCH_PARENT, dpToPx(160)))
        responseText = resp

        // Input row: EditText + Send.
        val askRow = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        val input = EditText(this).apply {
            hint = "Ask the agent…"
            setHintTextColor(themeColor(R.color.cellclaw_overlay_text_hint))
            setTextColor(themeColor(R.color.cellclaw_overlay_text_primary))
            setBackgroundColor(themeColor(R.color.cellclaw_overlay_input_surface))
            setPadding(dpToPx(12), dpToPx(8), dpToPx(12), dpToPx(8))
            isSingleLine = true
            imeOptions = EditorInfo.IME_ACTION_SEND
            setOnEditorActionListener { _, actionId, _ ->
                if (actionId == EditorInfo.IME_ACTION_SEND ||
                    actionId == EditorInfo.IME_ACTION_DONE ||
                    actionId == EditorInfo.IME_ACTION_GO) { submit(); true } else false
            }
            // The panel drops focusability after each submit so the agent can type into the app
            // it is driving; touching the field is how the user asks for it back.
            setOnTouchListener { v, event ->
                if (event.action == android.view.MotionEvent.ACTION_DOWN) {
                    acquireInputFocus()
                    v.requestFocus()
                } else if (event.action == android.view.MotionEvent.ACTION_UP) {
                    v.performClick()
                }
                false
            }
        }
        askRow.addView(input, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f))
        val send = TextView(this).apply {
            text = "Send"
            setTextColor(themeColor(R.color.cellclaw_overlay_text_primary))
            textSize = 14f
            gravity = Gravity.CENTER
            background = GradientDrawable().apply {
                // Purple remains exactly where CellClaw uses it: as a control accent, not as the
                // bubble's default state.
                setColor(themeColor(R.color.cellclaw_overlay_accent))
                cornerRadius = dpToPx(6).toFloat()
            }
            setPadding(dpToPx(14), dpToPx(8), dpToPx(14), dpToPx(8))
            setOnClickListener { submit() }
        }
        askRow.addView(send, LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.MATCH_PARENT
        ).apply { marginStart = dpToPx(6) })
        panel.addView(askRow, LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT))

        inputField = input
        sendButton = send
        panelView = panel

        // Focusable so the EditText gets the soft keyboard; NOT_TOUCH_MODAL lets outside taps reach
        // the backdrop below it.
        panelParams = WindowManager.LayoutParams(
            dpToPx(300),
            WindowManager.LayoutParams.WRAP_CONTENT,
            WindowManager.LayoutParams.TYPE_APPLICATION_OVERLAY,
            WindowManager.LayoutParams.FLAG_NOT_TOUCH_MODAL or
                WindowManager.LayoutParams.FLAG_SECURE,
            PixelFormat.TRANSLUCENT
        ).apply { gravity = Gravity.TOP or Gravity.START }

        // Full-screen transparent backdrop; tapping outside the panel collapses it.
        backdropView = View(this).apply {
            setBackgroundColor(Color.TRANSPARENT)
            setOnClickListener { togglePanel() }
        }
        backdropParams = WindowManager.LayoutParams(
            WindowManager.LayoutParams.MATCH_PARENT,
            WindowManager.LayoutParams.MATCH_PARENT,
            WindowManager.LayoutParams.TYPE_APPLICATION_OVERLAY,
            WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE or
                WindowManager.LayoutParams.FLAG_NOT_TOUCH_MODAL or
                WindowManager.LayoutParams.FLAG_SECURE,
            PixelFormat.TRANSLUCENT
        )
    }

    private fun headerButton(label: String, onClick: () -> Unit) = TextView(this).apply {
        text = label
        setTextColor(themeColor(R.color.cellclaw_overlay_text_primary))
        textSize = 13f
        gravity = Gravity.CENTER
        setPadding(dpToPx(10), dpToPx(4), dpToPx(10), dpToPx(4))
        setOnClickListener { onClick() }
    }

    private fun repositionPanel() {
        panelParams.x = bubbleParams.x + dpToPx(56)
        panelParams.y = bubbleParams.y
        runCatching { panelView?.let { windowManager.updateViewLayout(it, panelParams) } }
    }

    private fun togglePanel() {
        if (panelVisible) {
            runCatching { panelView?.let { windowManager.removeView(it) } }
            runCatching { backdropView?.let { windowManager.removeView(it) } }
            panelVisible = false
        } else {
            panelParams.x = bubbleParams.x + dpToPx(56)
            panelParams.y = bubbleParams.y
            panelVisible = true
            if (!overlayHidden) {
                backdropView?.let { windowManager.addView(it, backdropParams) }
                panelView?.let { windowManager.addView(it, panelParams) }
            }
        }
    }

    // ── Submit → gateway → response ──────────────────────────────────────
    private fun submit() {
        val field = inputField ?: return
        val text = field.text.toString().trim()
        if (text.isEmpty()) return
        field.text.clear()
        sendButton?.isEnabled = false
        showResponse("Thinking…")
        turnInFlight = true
        setBubbleState(BubbleState.THINKING)
        // Give focus back before the agent starts working, or its first `text` action types into
        // this box instead of the app on screen.
        releaseInputFocus()

        // Fast path: if the supervising service reports the gateway down, don't even dial.
        if (RuntimeState.gatewayState != GatewayProcess.State.RUNNING) {
            main.post {
                showResponse("Agent not running — open zerodroid and tap Start.")
                sendButton?.isEnabled = true
                turnInFlight = false
                setBubbleState(BubbleState.OFFLINE)
            }
            return
        }
        Thread {
            val config = ConfigStore(applicationContext)
            val reply = if (config.manualConfig) {
                GatewayClient.Reply(
                    response = null,
                    model = null,
                    error = "Floating bubble is unavailable while manual config is enabled.",
                )
            } else {
                GatewayClient.ask(
                    config.gatewayPort,
                    config.activeGatewayPathPrefix,
                    config.activeGatewayWebhookSecret,
                    config.activeGatewayAdminSecret,
                    config.overlayPairingToken,
                    text,
                ).also { config.overlayPairingToken = it.pairingToken }
            }
            main.post {
                sendButton?.isEnabled = true
                turnInFlight = false
                val out = when {
                    reply.gatewayDown -> "Agent not running — open zerodroid and tap Start."
                    reply.response != null -> reply.response
                    reply.error != null -> "Error: ${reply.error}"
                    else -> "No response."
                }
                showResponse(out)
                setBubbleState(
                    when {
                        reply.gatewayDown -> BubbleState.OFFLINE
                        reply.response != null -> BubbleState.IDLE
                        else -> BubbleState.ERROR
                    }
                )
            }
        }.apply { isDaemon = true; name = "overlay-webhook" }.start()
    }

    private fun showResponse(text: String) {
        if (!panelVisible) togglePanel()
        responseText?.text = text
    }

    private fun themeColor(resourceId: Int): Int = ContextCompat.getColor(this, resourceId)

    /** Exact CellClaw state palette. States with the same meaning deliberately share a colour. */
    private fun bubbleColor(state: BubbleState): Int = themeColor(
        when (state) {
            BubbleState.IDLE -> R.color.cellclaw_state_idle
            BubbleState.MONITORING -> R.color.cellclaw_state_monitoring
            BubbleState.THINKING,
            BubbleState.EXECUTING_TOOLS,
            BubbleState.WAITING_APPROVAL -> R.color.cellclaw_state_busy
            BubbleState.PAUSED,
            BubbleState.OFFLINE -> R.color.cellclaw_state_paused
            BubbleState.ERROR -> R.color.cellclaw_state_error
        }
    )

    /** Map the supervisor's live state onto CellClaw's bubble-state vocabulary. */
    private fun stateForGateway(state: GatewayProcess.State): BubbleState = when (state) {
        GatewayProcess.State.RUNNING -> if (turnInFlight) BubbleState.THINKING else BubbleState.IDLE
        GatewayProcess.State.STARTING,
        GatewayProcess.State.RESTARTING -> BubbleState.EXECUTING_TOOLS
        GatewayProcess.State.FAILED -> BubbleState.ERROR
        GatewayProcess.State.STOPPED -> BubbleState.OFFLINE
    }

    /** Tint the bubble to match what the agent is doing. Cheap enough to call unconditionally. */
    private fun setBubbleState(state: BubbleState) {
        bubbleState = state
        val bg = bubbleView?.background as? GradientDrawable ?: return
        bg.setColor(bubbleColor(state))
    }

    /**
     * Hand input focus back to whatever is underneath.
     *
     * The panel window has to be focusable for the user to type a prompt, which means its EditText
     * holds GLOBAL input focus afterwards. The accessibility service types into whatever holds that
     * focus, so leaving it here makes the agent type its next message into this box instead of into
     * the app it is driving. Dropping focusability after a submit returns focus to the real app;
     * touching the field takes it back.
     */
    private fun releaseInputFocus() {
        inputField?.clearFocus()
        panelParams.flags = panelParams.flags or WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE
        if (panelVisible) runCatching { windowManager.updateViewLayout(panelView, panelParams) }
    }

    /** Take focus back so the user can type. Paired with [releaseInputFocus]. */
    private fun acquireInputFocus() {
        panelParams.flags = panelParams.flags and WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE.inv()
        if (panelVisible) runCatching { windowManager.updateViewLayout(panelView, panelParams) }
    }

    // ── Visibility hide/restore (screenshot + gesture coordination) ──────
    private fun hideOverlayTemporarily(durationMs: Long) {
        // Consecutive hides already coalesce, but only within one hide window. During a turn the
        // agent captures, thinks for a second or two, captures again — each gap longer than the
        // window restores the overlay just long enough to flash it. Holding it hidden across those
        // gaps while a turn is in flight trades "visible during a busy turn" for "not strobing",
        // which is the better bargain: the result lands moments later anyway.
        val effective = if (bubbleState == BubbleState.THINKING) {
            maxOf(durationMs, THINKING_HIDE_MS)
        } else {
            durationMs
        }
        main.removeCallbacks(restoreRunnable)
        if (!overlayHidden) {
            overlayHidden = true
            // INVISIBLE is not enough for AccessibilityService: the overlay can remain
            // rootInActiveWindow and make every read/action look as though zerodroid itself is the
            // target. Detach the windows so Android promotes the resumed app underneath; the same
            // View instances (including the response text) are reattached after the hide window.
            runCatching { bubbleView?.let { windowManager.removeView(it) } }
            if (panelVisible) {
                runCatching { panelView?.let { windowManager.removeView(it) } }
                runCatching { backdropView?.let { windowManager.removeView(it) } }
            }
        }
        main.postDelayed(restoreRunnable, effective)
    }

    private fun restoreOverlay() {
        if (overlayHidden) {
            overlayHidden = false
            runCatching { bubbleView?.let { windowManager.addView(it, bubbleParams) } }
            if (panelVisible) {
                runCatching { backdropView?.let { windowManager.addView(it, backdropParams) } }
                runCatching { panelView?.let { windowManager.addView(it, panelParams) } }
            }
        }
    }

    // ── Affordances ──────────────────────────────────────────────────────
    private fun openApp() {
        if (panelVisible) togglePanel()
        startActivity(Intent(this, MainActivity::class.java).apply {
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP
        })
    }

    // ── Foreground plumbing ──────────────────────────────────────────────
    private fun createChannel() {
        if (Build.VERSION.SDK_INT >= 26) {
            getSystemService(NotificationManager::class.java).createNotificationChannel(
                NotificationChannel(CH, "zerodroid bubble", NotificationManager.IMPORTANCE_MIN)
            )
        }
    }

    @Suppress("DEPRECATION")
    private fun goForeground() {
        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, CH)
        } else {
            Notification.Builder(this)
        }
        val noti = builder
            .setContentTitle("zerodroid bubble")
            .setContentText("Tap the floating bubble to drive the agent.")
            .setSmallIcon(android.R.drawable.ic_menu_edit)
            .setContentIntent(
                PendingIntent.getActivity(
                    this, 0, Intent(this, MainActivity::class.java),
                    PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
                )
            )
            .setOngoing(true)
            .build()
        if (Build.VERSION.SDK_INT >= 34)
            startForeground(NOTI_ID, noti, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        else
            startForeground(NOTI_ID, noti)
    }

    companion object {
        /** How long to keep the overlay hidden between captures while a turn is running. */
        private const val THINKING_HIDE_MS = 2_500L

        private const val CH = "zerodroid-overlay"
        private const val NOTI_ID = 2

        /** Live "is the bubble service up?" flag — MainActivity reflects it in the toggle. */
        @Volatile var running: Boolean = false
            private set

        fun start(ctx: Context) {
            val i = Intent(ctx, OverlayService::class.java)
            if (Build.VERSION.SDK_INT >= 26) ctx.startForegroundService(i) else ctx.startService(i)
        }

        fun stop(ctx: Context) {
            ctx.stopService(Intent(ctx, OverlayService::class.java))
        }
    }

    /** A real click boundary lets accessibility services activate the draggable bubble. */
    private class BubbleView(context: android.content.Context, private val activate: () -> Unit) :
        AppCompatTextView(context) {
        override fun performClick(): Boolean {
            super.performClick()
            activate()
            return true
        }
    }
}
