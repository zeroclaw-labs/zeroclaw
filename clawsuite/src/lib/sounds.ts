/**
 * Sound Notification System for ClawSuite
 * Uses Web Audio API to synthesize unique sounds without audio files.
 */

// Note frequencies (Hz) - equal temperament tuning
const NOTES = {
  A2: 110.0,
  C3: 130.81,
  A4: 440.0,
  C5: 523.25,
  E5: 659.25,
  G5: 783.99,
  C6: 1046.5,
} as const

export type SoundEvent =
  | 'agentSpawned'
  | 'agentComplete'
  | 'agentFailed'
  | 'chatNotification'
  | 'chatComplete'
  | 'alert'
  | 'thinking'

type OscillatorType = 'sine' | 'triangle' | 'sawtooth' | 'square'

interface SoundPrefs {
  volume: number
  enabled: boolean
}

const STORAGE_KEY = 'clawsuite-sound-prefs'
const DEFAULT_VOLUME = 0.3

// Shared state
let audioContext: AudioContext | null = null
let prefs: SoundPrefs = { volume: DEFAULT_VOLUME, enabled: true }

// Load preferences from localStorage
function loadPrefs(): void {
  if (typeof window === 'undefined') return
  try {
    const stored = localStorage.getItem(STORAGE_KEY)
    if (stored) {
      const parsed = JSON.parse(stored) as Partial<SoundPrefs>
      prefs = {
        volume:
          typeof parsed.volume === 'number'
            ? Math.max(0, Math.min(1, parsed.volume))
            : DEFAULT_VOLUME,
        enabled: typeof parsed.enabled === 'boolean' ? parsed.enabled : true,
      }
    }
  } catch {
    // Ignore parse errors
  }
}

// Save preferences to localStorage
function savePrefs(): void {
  if (typeof window === 'undefined') return
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(prefs))
  } catch {
    // Ignore storage errors
  }
}

// Initialize prefs on load
loadPrefs()

/**
 * Get or create the shared AudioContext
 */
function getAudioContext(): AudioContext | null {
  if (typeof window === 'undefined') return null
  if (!audioContext || audioContext.state === 'closed') {
    audioContext = new AudioContext()
  }
  // Resume if suspended (browser autoplay policy)
  if (audioContext.state === 'suspended') {
    audioContext.resume().catch(() => {})
  }
  return audioContext
}

/**
 * Play a tone with attack/decay envelope
 */
function playTone(
  frequency: number,
  durationMs: number,
  type: OscillatorType = 'sine',
  volumeMultiplier: number = 1,
  startTimeOffset: number = 0,
): void {
  const ctx = getAudioContext()
  if (!ctx || !prefs.enabled) return

  const now = ctx.currentTime + startTimeOffset
  const duration = durationMs / 1000
  const volume = prefs.volume * volumeMultiplier

  // Create oscillator
  const osc = ctx.createOscillator()
  osc.type = type
  osc.frequency.setValueAtTime(frequency, now)

  // Create gain node for envelope
  const gain = ctx.createGain()
  gain.gain.setValueAtTime(0, now)

  // Attack (5ms)
  gain.gain.linearRampToValueAtTime(volume, now + 0.005)
  // Sustain then decay
  gain.gain.setValueAtTime(volume, now + duration * 0.7)
  gain.gain.exponentialRampToValueAtTime(0.001, now + duration)

  // Connect and play
  osc.connect(gain)
  gain.connect(ctx.destination)

  osc.start(now)
  osc.stop(now + duration + 0.01)
}

/**
 * Play a sequence of tones
 */
function playSequence(
  tones: Array<{
    freq: number
    durationMs: number
    type?: OscillatorType
    volume?: number
  }>,
): void {
  let offset = 0
  for (const tone of tones) {
    playTone(
      tone.freq,
      tone.durationMs,
      tone.type ?? 'sine',
      tone.volume ?? 1,
      offset,
    )
    offset += tone.durationMs / 1000
  }
}

// === Sound Functions ===

/**
 * Quick ascending two-tone chime (C5→E5, 100ms each, sine wave)
 * Used when a new agent is spawned
 */
export function playAgentSpawned(): void {
  playSequence([
    { freq: NOTES.C5, durationMs: 100, type: 'sine' },
    { freq: NOTES.E5, durationMs: 100, type: 'sine' },
  ])
}

/**
 * Satisfying success ding (G5, 150ms with soft decay, triangle wave)
 * Used when an agent completes successfully
 */
export function playAgentComplete(): void {
  const ctx = getAudioContext()
  if (!ctx || !prefs.enabled) return

  const now = ctx.currentTime
  const duration = 0.15
  const volume = prefs.volume

  const osc = ctx.createOscillator()
  osc.type = 'triangle'
  osc.frequency.setValueAtTime(NOTES.G5, now)

  const gain = ctx.createGain()
  gain.gain.setValueAtTime(0, now)
  // Quick attack
  gain.gain.linearRampToValueAtTime(volume, now + 0.003)
  // Long soft decay for satisfying ring
  gain.gain.exponentialRampToValueAtTime(0.001, now + duration + 0.2)

  osc.connect(gain)
  gain.connect(ctx.destination)

  osc.start(now)
  osc.stop(now + duration + 0.25)
}

/**
 * Low error tone (C3→A2, 200ms, sawtooth wave, quieter)
 * Used when an agent fails
 */
export function playAgentFailed(): void {
  playSequence([
    { freq: NOTES.C3, durationMs: 200, type: 'sawtooth', volume: 0.6 },
    { freq: NOTES.A2, durationMs: 200, type: 'sawtooth', volume: 0.6 },
  ])
}

/**
 * Soft ping (E5, 80ms, sine wave, very subtle)
 * Used for chat notifications
 */
export function playChatNotification(): void {
  playTone(NOTES.E5, 80, 'sine', 0.5)
}

/**
 * Gentle two-note descend (E5→C5, 80ms each, sine wave)
 * Used when chat completes
 */
export function playChatComplete(): void {
  playSequence([
    { freq: NOTES.E5, durationMs: 80, type: 'sine', volume: 0.7 },
    { freq: NOTES.C5, durationMs: 80, type: 'sine', volume: 0.7 },
  ])
}

/**
 * Attention grab (A4→E5→A4, 100ms each, square wave)
 * Used for important alerts
 */
export function playAlert(): void {
  playSequence([
    { freq: NOTES.A4, durationMs: 100, type: 'square', volume: 0.5 },
    { freq: NOTES.E5, durationMs: 100, type: 'square', volume: 0.5 },
    { freq: NOTES.A4, durationMs: 100, type: 'square', volume: 0.5 },
  ])
}

/**
 * Very subtle tick (C6, 30ms, sine wave, very quiet)
 * Used to indicate thinking/processing
 */
export function playThinking(): void {
  playTone(NOTES.C6, 30, 'sine', 0.33) // 0.1 relative to 0.3 default
}

// === Control Functions ===

/**
 * Set the master volume (0 to 1)
 */
export function setSoundVolume(vol: number): void {
  prefs.volume = Math.max(0, Math.min(1, vol))
  savePrefs()
}

/**
 * Get current volume
 */
export function getSoundVolume(): number {
  return prefs.volume
}

/**
 * Enable or disable all sounds
 */
export function setSoundEnabled(enabled: boolean): void {
  prefs.enabled = enabled
  savePrefs()
}

/**
 * Check if sounds are enabled
 */
export function isSoundEnabled(): boolean {
  return prefs.enabled
}

/**
 * Play a sound by event name
 */
export function playSound(event: SoundEvent): void {
  switch (event) {
    case 'agentSpawned':
      playAgentSpawned()
      break
    case 'agentComplete':
      playAgentComplete()
      break
    case 'agentFailed':
      playAgentFailed()
      break
    case 'chatNotification':
      playChatNotification()
      break
    case 'chatComplete':
      playChatComplete()
      break
    case 'alert':
      playAlert()
      break
    case 'thinking':
      playThinking()
      break
  }
}
