'use client'

import { useCallback, useEffect, useRef, useState } from 'react'

type VoiceInputState = 'idle' | 'listening' | 'processing' | 'error'

type UseVoiceInputOptions = {
  /** Language for speech recognition (BCP-47). Default: 'en-US' */
  lang?: string
  /** Insert interim (partial) results as they arrive */
  interim?: boolean
  /** Called with final transcript text */
  onResult?: (text: string) => void
  /** Called with interim transcript text */
  onInterim?: (text: string) => void
  /** Called on error */
  onError?: (error: string) => void
}

type UseVoiceInputReturn = {
  state: VoiceInputState
  isListening: boolean
  isSupported: boolean
  transcript: string
  start: () => void
  stop: () => void
  toggle: () => void
}

// Web Speech API types (not available in all TS configs)
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type SpeechRecognitionInstance = any
type SpeechRecognitionConstructor = new () => SpeechRecognitionInstance

function getSpeechRecognition(): SpeechRecognitionConstructor | null {
  if (typeof window === 'undefined') return null
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const win = window as any
  return win.SpeechRecognition ?? win.webkitSpeechRecognition ?? null
}

export function useVoiceInput(
  options: UseVoiceInputOptions = {},
): UseVoiceInputReturn {
  const {
    lang = 'en-US',
    interim = true,
    onResult,
    onInterim,
    onError,
  } = options
  const [state, setState] = useState<VoiceInputState>('idle')
  const [transcript, setTranscript] = useState('')
  const recognitionRef = useRef<SpeechRecognitionInstance | null>(null)
  const isSupported =
    typeof window !== 'undefined' && Boolean(getSpeechRecognition())

  // Keep callbacks fresh without re-creating recognition
  const callbacksRef = useRef({ onResult, onInterim, onError })
  callbacksRef.current = { onResult, onInterim, onError }

  const stop = useCallback(() => {
    const recognition = recognitionRef.current
    if (!recognition) return
    try {
      recognition.stop()
    } catch {
      // already stopped
    }
    setState('idle')
  }, [])

  const start = useCallback(() => {
    const SpeechRecognition = getSpeechRecognition()
    if (!SpeechRecognition) {
      callbacksRef.current.onError?.(
        'Speech recognition not supported in this browser',
      )
      setState('error')
      return
    }

    // Stop existing
    if (recognitionRef.current) {
      try {
        recognitionRef.current.stop()
      } catch {
        /* */
      }
    }

    const recognition = new SpeechRecognition()
    recognition.lang = lang
    recognition.interimResults = interim
    recognition.continuous = true
    recognition.maxAlternatives = 1

    recognition.onstart = () => {
      setState('listening')
      setTranscript('')
    }

    recognition.onresult = (event: any) => {
      let finalText = ''
      let interimText = ''

      for (let i = event.resultIndex; i < event.results.length; i++) {
        const result = event.results[i]
        if (!result?.[0]) continue
        const text = result[0].transcript
        if (result.isFinal) {
          finalText += text
        } else {
          interimText += text
        }
      }

      if (finalText) {
        setTranscript(finalText)
        callbacksRef.current.onResult?.(finalText)
      }
      if (interimText) {
        setTranscript(interimText)
        callbacksRef.current.onInterim?.(interimText)
      }
    }

    recognition.onerror = (event: any) => {
      if (event.error === 'aborted' || event.error === 'no-speech') {
        setState('idle')
        return
      }
      setState('error')
      callbacksRef.current.onError?.(event.error)
    }

    recognition.onend = () => {
      setState('idle')
      recognitionRef.current = null
    }

    recognitionRef.current = recognition
    recognition.start()
  }, [lang, interim])

  const toggle = useCallback(() => {
    if (state === 'listening') {
      stop()
    } else {
      start()
    }
  }, [state, start, stop])

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      if (recognitionRef.current) {
        try {
          recognitionRef.current.stop()
        } catch {
          /* */
        }
      }
    }
  }, [])

  return {
    state,
    isListening: state === 'listening',
    isSupported,
    transcript,
    start,
    stop,
    toggle,
  }
}
