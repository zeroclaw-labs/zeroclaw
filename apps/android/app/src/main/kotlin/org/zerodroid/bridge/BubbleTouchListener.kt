package org.zerodroid.bridge

import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.View
import android.view.WindowManager
import kotlin.math.abs

/**
 * Drag + tap + long-press gesture handler for the floating bubble window.
 *
 * Ported from CellClaw's BubbleTouchListener; see `apps/android/NOTICE`. It depends only on the
 * WindowManager + the bubble's LayoutParams and carries no product-specific logic. Drag updates the
 * window position live; a press that never crosses [DRAG_THRESHOLD] and isn't held past
 * [LONG_PRESS_TIMEOUT] calls [View.performClick] on ACTION_UP.
 */
class BubbleTouchListener(
    private val windowManager: WindowManager,
    private val layoutParams: WindowManager.LayoutParams,
    private val onDrag: ((x: Int, y: Int) -> Unit)? = null,
    private val onLongPress: (() -> Unit)? = null,
) : View.OnTouchListener {

    private var initialX = 0
    private var initialY = 0
    private var initialTouchX = 0f
    private var initialTouchY = 0f
    private var isDragging = false
    private var longPressTriggered = false
    private var pendingLongPress: Runnable = Runnable {}

    override fun onTouch(view: View, event: MotionEvent): Boolean {
        when (event.action) {
            MotionEvent.ACTION_DOWN -> {
                initialX = layoutParams.x
                initialY = layoutParams.y
                initialTouchX = event.rawX
                initialTouchY = event.rawY
                isDragging = false
                longPressTriggered = false
                pendingLongPress = Runnable {
                    longPressTriggered = true
                    view.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
                    onLongPress?.invoke()
                }
                view.postDelayed(pendingLongPress, LONG_PRESS_TIMEOUT)
                return true
            }
            MotionEvent.ACTION_MOVE -> {
                val dx = event.rawX - initialTouchX
                val dy = event.rawY - initialTouchY
                if (abs(dx) > DRAG_THRESHOLD || abs(dy) > DRAG_THRESHOLD) {
                    isDragging = true
                    view.removeCallbacks(pendingLongPress)
                }
                if (isDragging) {
                    layoutParams.x = initialX + dx.toInt()
                    layoutParams.y = initialY + dy.toInt()
                    windowManager.updateViewLayout(view, layoutParams)
                    onDrag?.invoke(layoutParams.x, layoutParams.y)
                }
                return true
            }
            MotionEvent.ACTION_UP -> {
                view.removeCallbacks(pendingLongPress)
                if (!isDragging && !longPressTriggered) {
                    view.performClick()
                }
                return true
            }
        }
        return false
    }

    companion object {
        private const val DRAG_THRESHOLD = 10f
        private const val LONG_PRESS_TIMEOUT = 500L
    }
}
