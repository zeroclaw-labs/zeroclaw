export function hapticTap() {
  try {
    navigator.vibrate?.(8)
  } catch {}
}
