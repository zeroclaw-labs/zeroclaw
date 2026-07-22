/**
 * useVoiceSession — the conversation brain of the Face experience.
 *
 * Owns the chat WebSocket, microphone, TTS playback and vision capture, and
 * exposes a small state machine:
 *
 *   idle → listening → transcribing → thinking → speaking → idle
 *                                        ↑___________|  (barge-in returns to listening)
 *
 * Latency-critical paths:
 *  - utterance WAV → POST /api/voice/transcribe (Groq/Whisper, ~300ms)
 *  - transcript → {"type":"speech_end"} over the already-open WS
 *  - server streams text `chunk` frames and sentence-sized `tts_chunk`
 *    audio; first audio typically lands before the text finishes
 *  - barge-in: local playback cancel is immediate; the server abort rides
 *    the same socket
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { apiFetch } from '../lib/api';
import { WebSocketClient } from '../lib/ws';
import { StreamingAudioPlayer } from '../lib/voice/audioPlayer';
import { MicCapture } from '../lib/voice/micCapture';
import { storeVoiceEffect, type VoiceEffectPreset } from '../lib/voice/robotVoice';
import type { WsMessage } from '../types/api';

export type VoicePhase =
  | 'boot' // connecting / waiting for mic permission
  | 'idle'
  | 'listening'
  | 'transcribing'
  | 'thinking'
  | 'speaking'
  | 'error';

export type ListenMode = 'push' | 'continuous';
export type VisionSource = 'off' | 'camera' | 'screen';

export interface VoiceSessionState {
  phase: VoicePhase;
  mode: ListenMode;
  vision: VisionSource;
  connected: boolean;
  micReady: boolean;
  /** last user transcript */
  transcript: string;
  /** streaming assistant text for captions */
  reply: string;
  /** current tool activity, if any (drives "working" mascot state) */
  toolActivity: string | null;
  error: string | null;
}

/** Emotion/gesture cue parsed from an inline control tag (contract B/C).
 * Fields mirror the server frame: either may be null/absent. */
export interface MascotCue {
  emotion: string | null;
  gesture: string | null;
}

export interface VoiceSessionApi {
  state: VoiceSessionState;
  /** press/release for push-to-talk */
  pressTalk: () => void;
  releaseTalk: () => void;
  setMode: (m: ListenMode) => void;
  setVision: (v: VisionSource) => void;
  sendText: (text: string) => void;
  bargeIn: () => void;
  /** instantaneous levels for the mascot */
  outputLevel: () => number;
  inputLevel: () => number;
  /** camera preview stream when vision === 'camera' (for a corner thumbnail) */
  cameraStream: MediaStream | null;
  /** Switch the voice character (droid / vox / core / human); persists. */
  setVoiceEffect: (preset: VoiceEffectPreset) => void;
  /** Subscribe to mascot cues (emotion/gesture from inline control tags),
   * fired when the sentence-unit they target actually starts playing.
   * Returns an unsubscribe function. */
  subscribeMascotCue: (cb: (cue: MascotCue) => void) => () => void;
}

interface TranscribeResponse {
  text: string;
}

async function blobToB64(blob: Blob): Promise<string> {
  const buf = await blob.arrayBuffer();
  const bytes = new Uint8Array(buf);
  let bin = '';
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    bin += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return btoa(bin);
}

export function useVoiceSession(agentAlias: string): VoiceSessionApi {
  const [state, setState] = useState<VoiceSessionState>({
    phase: 'boot',
    mode: 'push',
    vision: 'off',
    connected: false,
    micReady: false,
    transcript: '',
    reply: '',
    toolActivity: null,
    error: null,
  });
  const [cameraStream, setCameraStream] = useState<MediaStream | null>(null);

  const wsRef = useRef<WebSocketClient | null>(null);
  const playerRef = useRef<StreamingAudioPlayer | null>(null);
  const micRef = useRef<MicCapture | null>(null);
  const videoElRef = useRef<HTMLVideoElement | null>(null);
  const visionStreamRef = useRef<MediaStream | null>(null);
  const phaseRef = useRef<VoicePhase>('boot');
  const modeRef = useRef<ListenMode>('push');
  const visionRef = useRef<VisionSource>('off');
  const turnActiveRef = useRef(false);
  /** Bumped on every new turn AND every barge-in; stale tts_chunk frames
   * from a cancelled turn carry the old generation and are dropped. */
  const turnGenRef = useRef(0);
  /** Wall-clock of the last frame that proved the turn is alive (chunk,
   * thinking, tool activity, tts audio). Drives the stuck-turn watchdog. */
  const turnHeartbeatRef = useRef(0);
  /** Bumped on every setVision; late getUserMedia results from a previous
   * request are stopped instead of leaking a live camera/screen track. */
  const visionGenRef = useRef(0);

  // ---- caption debounce (plan #12) ----
  // The full accumulated reply text, updated synchronously on every delta;
  // `state.reply` only catches up on a flush so fast token streams don't
  // force a re-render per token.
  const replyAccRef = useRef('');
  const replyFlushTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const flushReply = useCallback(() => {
    if (replyFlushTimerRef.current) {
      clearTimeout(replyFlushTimerRef.current);
      replyFlushTimerRef.current = null;
    }
    setState((s) => (s.reply === replyAccRef.current ? s : { ...s, reply: replyAccRef.current }));
  }, []);

  /** Flush now if `immediate`, else on a short trailing debounce — collapses
   * a burst of token deltas into one render while still surfacing a
   * finished sentence right away. */
  const scheduleReplyFlush = useCallback(
    (immediate: boolean) => {
      if (immediate) {
        flushReply();
        return;
      }
      if (replyFlushTimerRef.current) clearTimeout(replyFlushTimerRef.current);
      replyFlushTimerRef.current = setTimeout(() => {
        replyFlushTimerRef.current = null;
        flushReply();
      }, 50);
    },
    [flushReply],
  );

  const resetReply = useCallback(() => {
    if (replyFlushTimerRef.current) {
      clearTimeout(replyFlushTimerRef.current);
      replyFlushTimerRef.current = null;
    }
    replyAccRef.current = '';
  }, []);

  // ---- mascot cues (contract B/C) ----
  // Cues arrive over the socket as soon as the server parses a control
  // tag — well before the sentence they target actually starts playing —
  // so they're buffered by seq and released by the player's onSeqStart.
  const pendingCuesRef = useRef(new Map<number, MascotCue[]>());
  const lastStartedSeqRef = useRef(-1);
  const mascotCueListenersRef = useRef(new Set<(cue: MascotCue) => void>());

  const emitMascotCue = useCallback((cue: MascotCue) => {
    for (const cb of mascotCueListenersRef.current) cb(cue);
  }, []);

  const subscribeMascotCue = useCallback((cb: (cue: MascotCue) => void) => {
    mascotCueListenersRef.current.add(cb);
    return () => {
      mascotCueListenersRef.current.delete(cb);
    };
  }, []);

  const clearMascotCues = useCallback(() => {
    pendingCuesRef.current.clear();
    lastStartedSeqRef.current = -1;
  }, []);

  const patch = useCallback((p: Partial<VoiceSessionState>) => {
    if (p.phase) phaseRef.current = p.phase;
    if (p.mode) modeRef.current = p.mode;
    if (p.vision !== undefined) visionRef.current = p.vision;
    setState((s) => ({ ...s, ...p }));
  }, []);

  // ---- vision frame capture ----
  const captureFrame = useCallback(async (): Promise<string | null> => {
    const stream = visionStreamRef.current;
    if (!stream || visionRef.current === 'off') return null;
    let video = videoElRef.current;
    if (!video) {
      video = document.createElement('video');
      video.muted = true;
      video.playsInline = true;
      videoElRef.current = video;
    }
    if (video.srcObject !== stream) {
      video.srcObject = stream;
      await video.play().catch(() => undefined);
    }
    if (!video.videoWidth) return null;
    const canvas = document.createElement('canvas');
    // Cap the long edge at 1280 to keep tokens + upload small.
    const scale = Math.min(1, 1280 / Math.max(video.videoWidth, video.videoHeight));
    canvas.width = Math.round(video.videoWidth * scale);
    canvas.height = Math.round(video.videoHeight * scale);
    canvas.getContext('2d')!.drawImage(video, 0, 0, canvas.width, canvas.height);
    return canvas.toDataURL('image/jpeg', 0.7);
  }, []);

  const stopVision = useCallback(() => {
    visionStreamRef.current?.getTracks().forEach((t) => t.stop());
    visionStreamRef.current = null;
    setCameraStream(null);
  }, []);

  const setVision = useCallback(
    (v: VisionSource) => {
      stopVision();
      const gen = ++visionGenRef.current;
      patch({ vision: v });
      if (v === 'off') return;
      const get =
        v === 'camera'
          ? navigator.mediaDevices.getUserMedia({ video: { width: 1280 } })
          : navigator.mediaDevices.getDisplayMedia({ video: true });
      get
        .then((stream) => {
          if (gen !== visionGenRef.current) {
            // superseded by a later setVision (or unmount) — don't leak
            stream.getTracks().forEach((t) => t.stop());
            return;
          }
          visionStreamRef.current = stream;
          if (v === 'camera') setCameraStream(stream);
        })
        .catch(() => {
          if (gen === visionGenRef.current) patch({ vision: 'off' });
        });
    },
    [patch, stopVision],
  );

  // ---- turn lifecycle ----
  const startVoiceTurn = useCallback(
    async (transcript: string) => {
      const ws = wsRef.current;
      if (!ws || !transcript.trim()) {
        patch({ phase: 'idle' });
        return;
      }
      if (!ws.connected) {
        patch({ phase: 'idle', error: 'not connected — reconnecting…' });
        return;
      }
      resetReply();
      patch({ phase: 'thinking', transcript, reply: '', toolActivity: null, error: null });
      turnActiveRef.current = true;
      turnHeartbeatRef.current = Date.now();
      turnGenRef.current++;
      scheduleThinkPulse(1800);
      playerRef.current?.beginUtterance();
      clearMascotCues();
      const image = await captureFrame();
      ws.sendRaw({
        type: 'speech_end',
        transcript,
        ...(image ? { images: [image] } : {}),
      });
    },
    [captureFrame, clearMascotCues, patch, resetReply],
  );

  const transcribe = useCallback(async (wav: Blob): Promise<string> => {
    const audio_b64 = await blobToB64(wav);
    const res = await apiFetch<TranscribeResponse>('/api/voice/transcribe', {
      method: 'POST',
      body: JSON.stringify({ audio_b64, format: 'wav' }),
    });
    return res.text?.trim() ?? '';
  }, []);

  /** Thinking-presence pulse: while the model is silent (no text, no
   * audio) a soft low blip every few seconds says "still here, working".
   * Cleared the moment anything streams. */
  const thinkPulseRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const stopThinkPulse = useCallback(() => {
    if (thinkPulseRef.current) {
      clearTimeout(thinkPulseRef.current);
      thinkPulseRef.current = null;
    }
  }, []);

  const scheduleThinkPulse = useCallback(
    (delayMs: number) => {
      stopThinkPulse();
      thinkPulseRef.current = setTimeout(() => {
        thinkPulseRef.current = null;
        if (!turnActiveRef.current) return;
        if (replyAccRef.current.length > 0) return;
        if (playerRef.current?.playing) return;
        playerRef.current?.chime('thinking');
        scheduleThinkPulse(3500);
      }, delayMs);
    },
    [stopThinkPulse],
  );

  /** Eager transcription started from an utterance PREVIEW (the mic thinks
   * the user probably finished ~250ms before it's sure). Keyed by
   * utterance id; consumed by handleUtterance when the preview held. */
  const previewRef = useRef<{ id: number; promise: Promise<string> } | null>(null);

  const handleUtterancePreview = useCallback(
    (wav: Blob, utteranceId: number) => {
      previewRef.current = {
        id: utteranceId,
        promise: transcribe(wav).catch(() => ''),
      };
    },
    [transcribe],
  );

  const handleUtterance = useCallback(
    async (
      wav: Blob,
      info?: { utteranceId: number; previewValid: boolean },
    ) => {
      patch({ phase: 'transcribing' });
      try {
        // "Heard you" — instant, before any network round-trip. Inside the
        // try: a closed AudioContext (WebKit backgrounding quirk) must fail
        // into the same recovery path as a network error, never strand the
        // UI in 'transcribing'.
        playerRef.current?.chime('commit');
        const preview = previewRef.current;
        previewRef.current = null;
        let text = '';
        if (info?.previewValid && preview && preview.id === info.utteranceId) {
          // The eager transcript covers the same audio — usually already
          // resolved by now, making STT latency effectively zero.
          text = await preview.promise;
        }
        if (!text) text = await transcribe(wav);
        if (text) {
          await startVoiceTurn(text);
        } else {
          patch({ phase: 'idle' });
        }
      } catch (e) {
        patch({
          phase: 'idle',
          error: e instanceof Error ? e.message : 'transcription failed',
        });
      }
    },
    [patch, startVoiceTurn, transcribe],
  );

  const bargeIn = useCallback(() => {
    stopThinkPulse();
    playerRef.current?.cancel();
    wsRef.current?.sendRaw({ type: 'barge_in' });
    turnActiveRef.current = false;
    flushReply();
    clearMascotCues();
    patch({ phase: modeRef.current === 'continuous' ? 'idle' : 'idle', toolActivity: null });
  }, [clearMascotCues, flushReply, patch]);

  // ---- boot ----
  useEffect(() => {
    let disposed = false;

    const player = new StreamingAudioPlayer();
    playerRef.current = player;
    player.onPlaybackStart = () => {
      patch({ phase: 'speaking' });
    };
    player.onPlaybackEnd = () => {
      if (!turnActiveRef.current) {
        patch({ phase: 'idle' });
      }
    };
    player.onUnitStart = (seq) => {
      lastStartedSeqRef.current = Math.max(lastStartedSeqRef.current, seq);
      const cues = pendingCuesRef.current.get(seq);
      if (cues) {
        pendingCuesRef.current.delete(seq);
        for (const cue of cues) emitMascotCue(cue);
      }
    };

    const endTurn = () => {
      stopThinkPulse();
      turnActiveRef.current = false;
      flushReply();
      clearMascotCues();
    };

    const ws = new WebSocketClient({ agentAlias });
    wsRef.current = ws;
    ws.onOpen = () => patch({ connected: true });
    ws.onClose = () => {
      // A drop mid-turn would otherwise strand the UI in 'thinking'.
      if (turnActiveRef.current) {
        endTurn();
        player.cancel();
        patch({ connected: false, phase: 'idle', toolActivity: null, error: 'connection lost' });
      } else {
        patch({ connected: false });
      }
    };
    ws.onMessage = (msg: WsMessage) => {
      // Any frame at all proves the server/turn is alive.
      turnHeartbeatRef.current = Date.now();
      switch (msg.type) {
        case 'chunk': {
          stopThinkPulse();
          const delta = msg.content ?? '';
          replyAccRef.current += delta;
          // Flush right away at a sentence boundary; otherwise coalesce a
          // burst of token deltas onto a short trailing debounce.
          scheduleReplyFlush(/[.!?…。！？]\s*$/.test(delta));
          break;
        }
        case 'chunk_reset':
          replyAccRef.current = '';
          flushReply();
          break;
        case 'tts_chunk':
          // Frames from a barged-in turn may still be in flight — drop them.
          if (turnActiveRef.current && msg.audio_b64) {
            void player.enqueue(
              msg.audio_b64,
              typeof msg.seq === 'number' ? msg.seq : undefined,
              msg.format,
              typeof msg.unit_seq === 'number' ? msg.unit_seq : undefined,
            );
          }
          break;
        case 'tts_cancel':
          player.cancel();
          clearMascotCues();
          break;
        case 'mascot_cue': {
          if (!turnActiveRef.current) break; // stale cue from a finished/cancelled turn
          const seq = typeof msg.seq === 'number' ? msg.seq : 0;
          const cue: MascotCue = { emotion: msg.emotion ?? null, gesture: msg.gesture ?? null };
          if (seq <= lastStartedSeqRef.current) {
            // That sentence unit already started playing — fire now.
            emitMascotCue(cue);
          } else {
            const list = pendingCuesRef.current.get(seq) ?? [];
            list.push(cue);
            pendingCuesRef.current.set(seq, list);
          }
          break;
        }
        case 'tool_call':
          patch({ toolActivity: msg.name ?? 'tool' });
          break;
        case 'tool_result':
          patch({ toolActivity: null });
          break;
        case 'done':
        case 'aborted':
          endTurn();
          patch({ toolActivity: null });
          // If nothing is playing (text-only reply or cancelled), settle.
          if (!player.playing) patch({ phase: 'idle' });
          break;
        case 'error':
          endTurn();
          patch({ phase: 'idle', error: msg.message ?? 'agent error', toolActivity: null });
          break;
        default:
          break;
      }
    };
    ws.connect();

    const mic = new MicCapture();
    micRef.current = mic;
    mic.onUtterance = (wav, _durationMs, info) => void handleUtterance(wav, info);
    mic.onUtterancePreview = handleUtterancePreview;
    mic.onSpeechStart = () => {
      // Onset while a turn is in flight (thinking, tool gap, or speaking)
      // is a real interruption — cancel it before starting the new
      // capture. turnActiveRef is the single source of truth for turn
      // state, so the mic itself doesn't need a "ducked" mode.
      if (turnActiveRef.current) bargeIn();
      if (phaseRef.current === 'idle') patch({ phase: 'listening' });
    };
    // The instant speech MIGHT be starting, audibly yield: duck playback
    // to 20%. Confirmed onset barges in (above); a false alarm restores
    // full volume. This is what makes the companion feel like it stops
    // to listen the moment you open your mouth.
    mic.onPossibleSpeech = () => {
      if (player.playing) player.duck();
    };
    mic.onSpeechReset = () => {
      if (player.playing) player.unduck();
    };
    mic
      .init()
      .then(() => {
        if (disposed) return;
        patch({ micReady: true, phase: 'idle' });
      })
      .catch(() => {
        if (disposed) return;
        patch({ phase: 'error', error: 'microphone permission denied' });
      });

    // Feed the player's live output envelope into the mic's playback-aware
    // VAD every frame, same cadence as the mascot's talk-level rAF loop.
    let levelRaf = 0;
    const pushPlaybackLevel = () => {
      micRef.current?.setPlaybackLevel(player.level());
      levelRaf = requestAnimationFrame(pushPlaybackLevel);
    };
    levelRaf = requestAnimationFrame(pushPlaybackLevel);

    // Stuck-turn watchdog: a turn whose server frames stop arriving for 90s
    // (hung provider, silently dropped socket) must not strand the face in
    // 'thinking' forever — reset to idle with a visible error. Interval, not
    // rAF, so it still fires while the tab is hidden.
    const watchdog = setInterval(() => {
      if (!turnActiveRef.current) return;
      if (Date.now() - turnHeartbeatRef.current < 90_000) return;
      endTurn();
      player.cancel();
      wsRef.current?.sendRaw({ type: 'barge_in' });
      patch({ phase: 'idle', toolActivity: null, error: 'they went quiet — try again' });
    }, 5_000);

    return () => {
      disposed = true;
      stopThinkPulse();
      clearInterval(watchdog);
      cancelAnimationFrame(levelRaf);
      mic.dispose();
      player.dispose();
      ws.disconnect();
      stopVision();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agentAlias]);

  // ---- controls ----
  const pressTalk = useCallback(() => {
    const mic = micRef.current;
    const player = playerRef.current;
    if (!mic?.running) return;
    player?.unlock();
    if (player?.playing) {
      // talking over the agent = barge in, then listen
      bargeIn();
    }
    mic.startPushToTalk();
    resetReply();
    patch({ phase: 'listening', reply: '', transcript: '' });
  }, [bargeIn, patch, resetReply]);

  const releaseTalk = useCallback(() => {
    micRef.current?.stopPushToTalk();
  }, []);

  const setMode = useCallback(
    (m: ListenMode) => {
      patch({ mode: m });
      playerRef.current?.unlock();
      micRef.current?.setContinuous(m === 'continuous');
    },
    [patch],
  );

  const sendText = useCallback(
    (text: string) => {
      if (!text.trim()) return;
      playerRef.current?.unlock();
      void startVoiceTurn(text.trim());
    },
    [startVoiceTurn],
  );

  return {
    state,
    pressTalk,
    releaseTalk,
    setMode,
    setVision,
    sendText,
    bargeIn,
    setVoiceEffect: (preset: VoiceEffectPreset) => {
      storeVoiceEffect(preset);
      playerRef.current?.setVoiceEffect(preset);
    },
    outputLevel: () => playerRef.current?.level() ?? 0,
    inputLevel: () => micRef.current?.level ?? 0,
    cameraStream,
    subscribeMascotCue,
  };
}
