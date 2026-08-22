package org.zerodroid.bridge

import android.os.Handler
import android.os.Looper

/**
 * Coordinates the floating-bubble overlay's visibility across the app.
 *
 * The a11y service ([UiAccessibilityService]) calls [requestHide] immediately before a screenshot
 * or another screen-dependent operation, so the bubble neither appears in captured images nor
 * becomes the active accessibility window. [OverlayService] registers a [Listener]; it detaches
 * its windows for the requested duration and auto-restores them.
 *
 * Ported from CellClaw's OverlayVisibilityController; see `apps/android/NOTICE`. Two deliberate
 * changes:
 *  - a plain Kotlin `object` (no Hilt `@Singleton`/`@Inject`) — zerodroid wires singletons as
 *    objects (see [RuntimeState], [UiBridge]);
 *  - a main-thread [Handler] callback instead of a coroutine `MutableSharedFlow` — the base/`lite`
 *    flavor carries no kotlinx-coroutines dependency, so flows aren't available here.
 *
 * There is at most one overlay observer (a single bubble), so a lone volatile listener slot is
 * enough; no fan-out is needed.
 */
object OverlayVisibilityController {

    /** Callback the overlay implements to hide itself for [durationMs] and then restore. */
    fun interface Listener {
        fun onHideRequested(durationMs: Long)
    }

    private val main = Handler(Looper.getMainLooper())
    @Volatile private var listener: Listener? = null

    /** [OverlayService] registers itself on create; passes null on destroy. */
    fun setListener(l: Listener?) { listener = l }

    /** True when a bubble is live and would honor a hide request — lets callers skip the settle
     *  delay entirely when the overlay is off (no cost when the feature is unused). */
    fun isActive(): Boolean = listener != null

    /**
     * Ask the overlay to hide for [durationMs] ms, then restore. No-op when no bubble is showing.
     * Safe to call from any thread: view mutation is marshalled to the main thread. When already on
     * the main thread the listener runs synchronously, so the bubble is detached before the caller
     * proceeds to capture/dispatch.
     */
    fun requestHide(durationMs: Long = 400) {
        val l = listener ?: return
        if (Looper.myLooper() == Looper.getMainLooper()) l.onHideRequested(durationMs)
        else main.post { l.onHideRequested(durationMs) }
    }
}
