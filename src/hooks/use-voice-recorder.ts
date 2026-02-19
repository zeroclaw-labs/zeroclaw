'use client'

import { useCallback, useRef, useState } from 'react'

type RecorderState = 'idle' | 'recording' | 'processing'

type UseVoiceRecorderOptions = {
  /** Max recording duration in ms. Default: 120000 (2 min) */
  maxDurationMs?: number
  /** Called with the recorded audio blob + duration */
  onRecorded?: (blob: Blob, durationMs: number) => void
  onError?: (error: string) => void
}

type UseVoiceRecorderReturn = {
  state: RecorderState
  isRecording: boolean
  isSupported: boolean
  durationMs: number
  start: () => void
  stop: () => void
}

export function useVoiceRecorder(
  options: UseVoiceRecorderOptions = {},
): UseVoiceRecorderReturn {
  const { maxDurationMs = 120_000, onRecorded, onError } = options
  const [state, setState] = useState<RecorderState>('idle')
  const [durationMs, setDurationMs] = useState(0)
  const recorderRef = useRef<MediaRecorder | null>(null)
  const chunksRef = useRef<Blob[]>([])
  const startTimeRef = useRef(0)
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null)
  const maxTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  const callbacksRef = useRef({ onRecorded, onError })
  callbacksRef.current = { onRecorded, onError }

  const isSupported =
    typeof window !== 'undefined' &&
    typeof navigator !== 'undefined' &&
    Boolean(navigator.mediaDevices?.getUserMedia) &&
    typeof MediaRecorder !== 'undefined'

  const cleanup = useCallback(() => {
    if (timerRef.current) {
      clearInterval(timerRef.current)
      timerRef.current = null
    }
    if (maxTimerRef.current) {
      clearTimeout(maxTimerRef.current)
      maxTimerRef.current = null
    }
  }, [])

  const stop = useCallback(() => {
    const recorder = recorderRef.current
    if (!recorder || recorder.state === 'inactive') {
      setState('idle')
      return
    }
    recorder.stop()
    // Stream tracks cleanup
    recorder.stream.getTracks().forEach((t) => t.stop())
    cleanup()
  }, [cleanup])

  const start = useCallback(async () => {
    if (!isSupported) {
      callbacksRef.current.onError?.('Audio recording not supported')
      return
    }

    // Stop any existing recording
    if (recorderRef.current && recorderRef.current.state !== 'inactive') {
      recorderRef.current.stop()
      recorderRef.current.stream.getTracks().forEach((t) => t.stop())
    }
    cleanup()

    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true })

      // Prefer webm/opus, fall back to whatever is available
      const mimeType = MediaRecorder.isTypeSupported('audio/webm;codecs=opus')
        ? 'audio/webm;codecs=opus'
        : MediaRecorder.isTypeSupported('audio/webm')
          ? 'audio/webm'
          : 'audio/mp4'

      const recorder = new MediaRecorder(stream, { mimeType })
      chunksRef.current = []
      startTimeRef.current = Date.now()
      setDurationMs(0)

      recorder.ondataavailable = (event) => {
        if (event.data.size > 0) {
          chunksRef.current.push(event.data)
        }
      }

      recorder.onstop = () => {
        setState('processing')
        const blob = new Blob(chunksRef.current, { type: mimeType })
        const duration = Date.now() - startTimeRef.current
        chunksRef.current = []
        recorderRef.current = null
        setState('idle')

        if (blob.size > 0 && duration > 500) {
          callbacksRef.current.onRecorded?.(blob, duration)
        }
      }

      recorder.onerror = () => {
        callbacksRef.current.onError?.('Recording failed')
        setState('idle')
        cleanup()
      }

      recorderRef.current = recorder
      recorder.start(100) // collect chunks every 100ms
      setState('recording')

      // Duration counter
      timerRef.current = setInterval(() => {
        setDurationMs(Date.now() - startTimeRef.current)
      }, 100)

      // Max duration auto-stop
      maxTimerRef.current = setTimeout(() => {
        stop()
      }, maxDurationMs)
    } catch (err) {
      const msg =
        err instanceof Error ? err.message : 'Microphone access denied'
      callbacksRef.current.onError?.(msg)
      setState('idle')
    }
  }, [isSupported, cleanup, stop, maxDurationMs])

  return {
    state,
    isRecording: state === 'recording',
    isSupported,
    durationMs,
    start,
    stop,
  }
}
