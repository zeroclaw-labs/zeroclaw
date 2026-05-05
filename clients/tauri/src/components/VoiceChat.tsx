/**
 * Voice Chat Component (Rust-native, replaces LiveKitVoiceChat).
 *
 * Connects to the gateway's `/ws/voice_chat` WebSocket for real-time
 * voice conversation with the MoA agent. The new transport is a
 * simple JSON-over-WebSocket protocol (see
 * `src/voice/events_chat.rs` server-side):
 *
 *   User Mic ─▸ AudioWorklet (16 kHz PCM16) ─▸ audio_chunk frames ─▸ Server
 *                                                                       │
 *                                                                       ▼
 *                                                         Gemma 4 ASR + self-validation
 *                                                                       │
 *                                                                       ▼
 *                                                         LLM (Gemini Flash) reply
 *                                                                       │
 *                                                                       ▼
 *   User Speaker ◀── audio_out frames (Typecast or local TTS) ◀────────┘
 *
 * No LiveKit, no WebRTC. Same browser-native AudioContext +
 * AudioWorklet pipeline the Interpreter component already uses for
 * mic capture, just pointed at a different WebSocket endpoint.
 *
 * The server handles offline TTS fallback automatically: when
 * Typecast is unreachable (mountain Wi-Fi off, etc.) it falls
 * through to local Kokoro/CosyVoice and continues talking.
 */

import { useState, useRef, useCallback, useEffect } from "react";
import type { Locale } from "../lib/i18n";
import { apiClient } from "../lib/api";

interface VoiceChatProps {
  locale: Locale;
  onClose: () => void;
  /** Carried for backwards compatibility with the LiveKit version's
   *  Mode A/B distinction. The Rust-native path is functionally
   *  equivalent to "Mode A: Pipeline" — it pipelines Gemma STT +
   *  LLM + TTS — so we ignore the value but accept the prop so the
   *  Chat.tsx call site does not need to change. */
  initialMode?: "pipeline" | "s2s";
}

type VoiceChatStatus =
  | "idle"
  | "connecting"
  | "connected"
  | "listening"
  | "thinking"
  | "speaking"
  | "error"
  | "disconnecting";

interface ChatTranscript {
  id: string;
  role: "user" | "assistant" | "system" | "reask";
  text: string;
  timestamp: number;
}

// 16 kHz mono PCM16 — matches what GemmaAsrSession expects on the
// server side. The worklet downsamples from the device's native
// sample rate.
const WORKLET_CODE = `
class PcmCaptureProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    this._buffer = [];
    this._sourceRate = options.processorOptions?.sourceRate || sampleRate;
    this._targetRate = options.processorOptions?.targetRate || 16000;
    this._ratio = this._sourceRate / this._targetRate;
    this._targetSamples = Math.round(this._targetRate * 0.1);
    this._resamplePos = 0;
  }
  process(inputs) {
    const input = inputs[0];
    if (!input || !input[0]) return true;
    const samples = input[0];
    for (let i = 0; i < samples.length; i++) {
      this._resamplePos += 1;
      if (this._resamplePos >= this._ratio) {
        this._resamplePos -= this._ratio;
        this._buffer.push(samples[i]);
      }
    }
    while (this._buffer.length >= this._targetSamples) {
      const chunk = this._buffer.splice(0, this._targetSamples);
      const pcm16 = new Int16Array(chunk.length);
      for (let i = 0; i < chunk.length; i++) {
        const s = Math.max(-1, Math.min(1, chunk[i]));
        pcm16[i] = s < 0 ? s * 0x8000 : s * 0x7FFF;
      }
      this.port.postMessage(pcm16.buffer, [pcm16.buffer]);
    }
    return true;
  }
}
registerProcessor('pcm-capture-processor', PcmCaptureProcessor);
`;

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunkSize = 0x8000;
  for (let i = 0; i < bytes.length; i += chunkSize) {
    binary += String.fromCharCode(
      ...bytes.subarray(i, Math.min(i + chunkSize, bytes.length)),
    );
  }
  return btoa(binary);
}

function base64ToPcm16(b64: string): Int16Array {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return new Int16Array(bytes.buffer);
}

export function VoiceChat({ locale, onClose, initialMode: _initialMode }: VoiceChatProps) {
  const [status, setStatus] = useState<VoiceChatStatus>("idle");
  const [transcripts, setTranscripts] = useState<ChatTranscript[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [isMuted, setIsMuted] = useState(false);

  const wsRef = useRef<WebSocket | null>(null);
  const sessionIdRef = useRef<string>("");
  const audioContextRef = useRef<AudioContext | null>(null);
  const workletNodeRef = useRef<AudioWorkletNode | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const playbackCtxRef = useRef<AudioContext | null>(null);
  const playbackQueueRef = useRef<{ pcm: Int16Array; sampleRate: number }[]>([]);
  const playbackBusyRef = useRef(false);
  const transcriptEndRef = useRef<HTMLDivElement>(null);
  const audioSeqRef = useRef(0);

  useEffect(() => {
    transcriptEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [transcripts]);

  useEffect(() => {
    return () => {
      // best-effort cleanup on unmount
      const ws = wsRef.current;
      if (ws && ws.readyState === WebSocket.OPEN) {
        try {
          ws.close();
        } catch { /* ignore */ }
      }
      const stream = streamRef.current;
      if (stream) stream.getTracks().forEach((t) => t.stop());
      const ctx = audioContextRef.current;
      if (ctx && ctx.state !== "closed") ctx.close();
      const pb = playbackCtxRef.current;
      if (pb && pb.state !== "closed") pb.close();
    };
  }, []);

  const addTranscript = useCallback(
    (role: ChatTranscript["role"], text: string) => {
      setTranscripts((prev) => [
        ...prev,
        {
          id: crypto.randomUUID(),
          role,
          text,
          timestamp: Date.now(),
        },
      ]);
    },
    [],
  );

  // ── Playback queue: stitch incoming audio_out chunks into one
  //    smooth stream per assistant reply.
  const drainPlayback = useCallback(async () => {
    if (playbackBusyRef.current) return;
    playbackBusyRef.current = true;
    try {
      while (playbackQueueRef.current.length > 0) {
        const item = playbackQueueRef.current.shift();
        if (!item) break;
        let ctx = playbackCtxRef.current;
        if (!ctx || ctx.state === "closed") {
          ctx = new AudioContext({ sampleRate: item.sampleRate });
          playbackCtxRef.current = ctx;
        }
        if (ctx.state === "suspended") {
          try { await ctx.resume(); } catch { /* ignore */ }
        }
        const float = new Float32Array(item.pcm.length);
        for (let i = 0; i < item.pcm.length; i++) float[i] = item.pcm[i] / 0x8000;
        const buf = ctx.createBuffer(1, float.length, item.sampleRate);
        buf.getChannelData(0).set(float);
        const src = ctx.createBufferSource();
        src.buffer = buf;
        src.connect(ctx.destination);
        await new Promise<void>((resolve) => {
          src.onended = () => resolve();
          src.start();
        });
      }
    } finally {
      playbackBusyRef.current = false;
    }
  }, []);

  // ── Microphone capture: start AudioWorklet, send PCM as
  //    `audio_chunk` frames over the WebSocket.
  const startMicrophone = useCallback(async () => {
    try {
      const stream = await navigator.mediaDevices.getUserMedia({
        audio: {
          echoCancellation: true,
          autoGainControl: true,
          noiseSuppression: true,
        },
      });
      streamRef.current = stream;

      const ctx = new AudioContext();
      audioContextRef.current = ctx;
      const blob = new Blob([WORKLET_CODE], { type: "application/javascript" });
      const url = URL.createObjectURL(blob);
      await ctx.audioWorklet.addModule(url);
      URL.revokeObjectURL(url);

      const source = ctx.createMediaStreamSource(stream);
      const node = new AudioWorkletNode(ctx, "pcm-capture-processor", {
        processorOptions: { sourceRate: ctx.sampleRate, targetRate: 16000 },
      });
      node.port.onmessage = (e: MessageEvent<ArrayBuffer>) => {
        const ws = wsRef.current;
        if (!ws || ws.readyState !== WebSocket.OPEN) return;
        const b64 = bytesToBase64(new Uint8Array(e.data));
        ws.send(
          JSON.stringify({
            type: "audio_chunk",
            sessionId: sessionIdRef.current,
            seq: audioSeqRef.current++,
            ts: Date.now(),
            pcm16le: b64,
          }),
        );
      };
      source.connect(node);
      workletNodeRef.current = node;
      setStatus("listening");
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      console.error("Microphone error:", e);
      addTranscript("system", `Microphone error: ${msg}`);
      setError(msg);
      setStatus("error");
    }
  }, [addTranscript]);

  const stopMicrophone = useCallback(() => {
    const node = workletNodeRef.current;
    if (node) {
      try { node.disconnect(); } catch { /* ignore */ }
      workletNodeRef.current = null;
    }
    const ctx = audioContextRef.current;
    if (ctx && ctx.state !== "closed") {
      try { ctx.close(); } catch { /* ignore */ }
      audioContextRef.current = null;
    }
    const stream = streamRef.current;
    if (stream) {
      stream.getTracks().forEach((t) => t.stop());
      streamRef.current = null;
    }
  }, []);

  const connect = useCallback(async () => {
    setError(null);
    setStatus("connecting");
    setTranscripts([]);
    audioSeqRef.current = 0;
    sessionIdRef.current = crypto.randomUUID();

    const token = apiClient.getToken();
    if (!token) {
      setError(
        locale === "ko"
          ? "로그인이 필요합니다."
          : "Please log in first.",
      );
      setStatus("error");
      return;
    }

    const url =
      apiClient.getServerUrl().replace(/^http/, "ws") +
      `/ws/voice_chat?token=${encodeURIComponent(token)}`;
    const ws = new WebSocket(url);
    wsRef.current = ws;

    ws.onopen = () => {
      ws.send(
        JSON.stringify({
          type: "chat_session_start",
          sessionId: sessionIdRef.current,
          // Optional language hint — falls back to detection when absent.
          sourceLang: navigator.language?.split("-")[0] || "en",
          deviceId: "tauri-app",
        }),
      );
    };

    ws.onmessage = (event) => {
      try {
        const msg = JSON.parse(event.data);
        switch (msg.type) {
          case "chat_session_ready":
            setStatus("connected");
            startMicrophone();
            break;
          case "user_transcript":
            addTranscript("user", msg.text);
            setStatus("thinking");
            break;
          case "re_ask":
            // Server is asking the user to repeat or confirm — show
            // it in the thread AND queue the audio_out frames that
            // follow as TTS playback.
            addTranscript("reask", msg.message);
            setStatus("speaking");
            break;
          case "assistant_text":
            addTranscript("assistant", msg.text);
            setStatus("speaking");
            break;
          case "audio_out": {
            const pcm = base64ToPcm16(msg.pcm16le);
            playbackQueueRef.current.push({
              pcm,
              sampleRate: msg.sampleRate || 24000,
            });
            drainPlayback();
            break;
          }
          case "turn_complete":
            setStatus("listening");
            break;
          case "error":
            addTranscript("system", `[${msg.code}] ${msg.message}`);
            setError(msg.message);
            break;
          case "chat_session_ended":
            setStatus("idle");
            break;
          default:
            // Unknown message — log but don't crash.
            console.warn("voice-chat: unknown message type", msg);
            break;
        }
      } catch (e) {
        console.error("voice-chat: parse error", e);
      }
    };

    ws.onerror = (e) => {
      console.error("voice-chat WS error", e);
      addTranscript(
        "system",
        locale === "ko" ? "연결 오류가 발생했습니다." : "Connection error.",
      );
      setError("WebSocket error");
      setStatus("error");
    };

    ws.onclose = () => {
      stopMicrophone();
      setStatus("idle");
    };
  }, [addTranscript, drainPlayback, locale, startMicrophone, stopMicrophone]);

  const disconnect = useCallback(() => {
    setStatus("disconnecting");
    stopMicrophone();
    const ws = wsRef.current;
    if (ws && ws.readyState === WebSocket.OPEN) {
      try {
        ws.send(
          JSON.stringify({
            type: "chat_session_stop",
            sessionId: sessionIdRef.current,
          }),
        );
      } catch { /* ignore */ }
      try { ws.close(); } catch { /* ignore */ }
    }
    wsRef.current = null;
    setStatus("idle");
  }, [stopMicrophone]);

  const toggleMute = useCallback(() => {
    const stream = streamRef.current;
    if (!stream) return;
    const newMuted = !isMuted;
    stream.getAudioTracks().forEach((t) => {
      t.enabled = !newMuted;
    });
    setIsMuted(newMuted);
    addTranscript(
      "system",
      newMuted
        ? locale === "ko"
          ? "마이크 음소거"
          : "Mic muted"
        : locale === "ko"
        ? "마이크 활성화"
        : "Mic unmuted",
    );
  }, [isMuted, locale, addTranscript]);

  const isActive =
    status === "connecting" ||
    status === "connected" ||
    status === "listening" ||
    status === "thinking" ||
    status === "speaking" ||
    status === "disconnecting";

  return (
    <div className="livekit-voice-chat">
      {/* Header — kept the same DOM class names as LiveKitVoiceChat
          so the existing CSS in App.css continues to apply unchanged. */}
      <div className="lk-voice-header">
        <h3>{locale === "ko" ? "AI 음성 대화" : "AI Voice Chat"}</h3>
        <div className={`lk-status-badge ${status}`}>
          {status === "idle"
            ? locale === "ko" ? "대기" : "Idle"
            : status === "connecting"
            ? locale === "ko" ? "연결 중" : "Connecting"
            : status === "listening"
            ? locale === "ko" ? "듣는 중" : "Listening"
            : status === "thinking"
            ? locale === "ko" ? "생각 중" : "Thinking"
            : status === "speaking"
            ? locale === "ko" ? "말하는 중" : "Speaking"
            : status === "connected"
            ? locale === "ko" ? "대화 중" : "Connected"
            : status === "error"
            ? locale === "ko" ? "오류" : "Error"
            : locale === "ko" ? "연결 해제 중" : "Disconnecting"}
        </div>
        <button className="lk-close-btn" onClick={onClose} title="Close">
          ✕
        </button>
      </div>

      {/* Stack info — show the new Rust-native pipeline */}
      <div className="lk-stack-info">
        <span>STT: Gemma 4 (on-device)</span>
        <span>LLM: Gemini 3.1 Flash Lite</span>
        <span>TTS: Typecast → local fallback</span>
        <span>Transport: WebSocket</span>
      </div>

      <div className="lk-earphone-notice">
        {locale === "ko"
          ? "🎧 이어폰을 사용하시면 더 자연스러운 대화가 가능합니다"
          : "🎧 Using earphones provides a better voice chat experience"}
      </div>

      <div className="lk-transcripts">
        {transcripts.length === 0 && status === "idle" && (
          <div className="lk-empty">
            <div className="lk-empty-icon">🎙️</div>
            <p>
              {locale === "ko"
                ? "아래 시작 버튼을 눌러 AI와 음성으로 대화하세요"
                : "Press Start to begin a voice conversation with AI"}
            </p>
            <p className="lk-empty-sub">
              {locale === "ko"
                ? "오프라인에서도 작동합니다 (와이파이가 끊겨도 로컬 음성으로 응답)"
                : "Works offline too (responds via local voice if Wi-Fi drops)"}
            </p>
          </div>
        )}
        {transcripts.map((tr) => (
          <div
            key={tr.id}
            className={`lk-transcript lk-transcript-${tr.role}`}
          >
            <span className="lk-transcript-badge">
              {tr.role === "user"
                ? "🗣️"
                : tr.role === "assistant"
                ? "🤖"
                : tr.role === "reask"
                ? "❓"
                : "⚙️"}
            </span>
            <span className="lk-transcript-text">{tr.text}</span>
          </div>
        ))}
        {(status === "listening" || status === "connected") && (
          <div className="lk-transcript lk-listening-pulse">
            <span className="listening-pulse" />
            <span>
              {locale === "ko" ? "듣고 있습니다..." : "Listening..."}
            </span>
          </div>
        )}
        <div ref={transcriptEndRef} />
      </div>

      {error && <div className="lk-error">{error}</div>}

      <div className="lk-actions">
        {!isActive ? (
          <button className="lk-start-btn" onClick={connect}>
            🎙️ {locale === "ko" ? "음성 대화 시작" : "Start Voice Chat"}
          </button>
        ) : (
          <div className="lk-active-controls">
            <button
              className={`lk-mute-btn ${isMuted ? "muted" : ""}`}
              onClick={toggleMute}
              disabled={status === "connecting" || status === "disconnecting"}
            >
              {isMuted ? "🔇" : "🎤"}{" "}
              {isMuted
                ? locale === "ko" ? "음소거 해제" : "Unmute"
                : locale === "ko" ? "음소거" : "Mute"}
            </button>
            <button
              className="lk-end-btn"
              onClick={disconnect}
              disabled={status === "disconnecting"}
            >
              ⏹ {locale === "ko" ? "대화 종료" : "End Chat"}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
