/**
 * Follow-the-tail policy for the WebChat transcript (#9562).
 *
 * The pane follows new output only while the reader is near the bottom. The
 * decision is made from the pane's current scroll position, not from the input
 * gesture that produced it, so wheel, touch, keyboard, and native scrollbar
 * drags all behave the same. It also means the component's own scroll-to-bottom
 * (which always lands inside the threshold) cannot be mistaken for a manual
 * scroll-up.
 */

/** Distance from the tail that still counts as "reading the live output". */
export const NEAR_BOTTOM_PX = 150;

export interface ScrollMetrics {
  scrollHeight: number;
  scrollTop: number;
  clientHeight: number;
}

/** Non-negative pixel distance from the visible bottom of a scroll pane. */
export function distanceFromBottom(metrics: ScrollMetrics): number {
  return Math.max(0, metrics.scrollHeight - metrics.scrollTop - metrics.clientHeight);
}

/** True while the pane is close enough to the tail to keep following it. */
export function isNearBottom(metrics: ScrollMetrics, threshold = NEAR_BOTTOM_PX): boolean {
  return distanceFromBottom(metrics) <= threshold;
}
