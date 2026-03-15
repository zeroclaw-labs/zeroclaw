import { useState, useRef, useCallback, useEffect } from "react";
import { t, type Locale } from "../lib/i18n";
import { apiClient } from "../lib/api";

interface InterpreterProps {
  locale: Locale;
  onBack: () => void;
  onToggleSidebar: () => void;
  sidebarOpen: boolean;
}

interface Transcript {
  id: string;
  type: "input" | "output" | "system";
  text: string;
  timestamp: number;
}

type ConnectionStatus = "idle" | "connecting" | "ready" | "listening" | "stopping" | "error";
type VoiceProvider = "gemini" | "openai";

// "auto" = auto-detect language from speech input
const LANGUAGES = [
  { code: "auto", name: "Auto-detect / 자동 감지", flag: "🌐" },
  { code: "ko", name: "한국어", flag: "🇰🇷" },
  { code: "en", name: "English", flag: "🇺🇸" },
  { code: "ja", name: "日本語", flag: "🇯🇵" },
  { code: "zh", name: "中文", flag: "🇨🇳" },
  { code: "zh-TW", name: "中文 (繁體)", flag: "🇹🇼" },
  { code: "es", name: "Español", flag: "🇪🇸" },
  { code: "fr", name: "Français", flag: "🇫🇷" },
  { code: "de", name: "Deutsch", flag: "🇩🇪" },
  { code: "th", name: "ไทย", flag: "🇹🇭" },
  { code: "vi", name: "Tiếng Việt", flag: "🇻🇳" },
  { code: "ru", name: "Русский", flag: "🇷🇺" },
  { code: "ar", name: "العربية", flag: "🇸🇦" },
  { code: "pt", name: "Português", flag: "🇧🇷" },
  { code: "it", name: "Italiano", flag: "🇮🇹" },
  { code: "hi", name: "हिन्दी", flag: "🇮🇳" },
  { code: "id", name: "Bahasa Indonesia", flag: "🇮🇩" },
  { code: "ms", name: "Bahasa Melayu", flag: "🇲🇾" },
  { code: "tl", name: "Filipino", flag: "🇵🇭" },
  { code: "nl", name: "Nederlands", flag: "🇳🇱" },
  { code: "pl", name: "Polski", flag: "🇵🇱" },
  { code: "sv", name: "Svenska", flag: "🇸🇪" },
  { code: "da", name: "Dansk", flag: "🇩🇰" },
  { code: "cs", name: "Čeština", flag: "🇨🇿" },
  { code: "uk", name: "Українська", flag: "🇺🇦" },
  { code: "tr", name: "Türkçe", flag: "🇹🇷" },
];

// Interpretation direction modes
type InterpDirection = "bidirectional" | "unidirectional";


// AudioWorklet processor code (inline, runs in audio thread)
// Receives audio at native sample rate (e.g. 48000) and downsamples to 16kHz PCM16.
// IMPORTANT: AudioContext must be created at native rate, NOT 16000,
// because forcing sampleRate:16000 kills mic input on macOS WebKit (Tauri).
const WORKLET_CODE = `
class PcmCaptureProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    this._buffer = [];
    this._sourceRate = options.processorOptions?.sourceRate || sampleRate;
    this._targetRate = options.processorOptions?.targetRate || 16000;
    this._ratio = this._sourceRate / this._targetRate;
    // 100ms of output at target rate
    this._targetSamples = Math.round(this._targetRate * 0.1);
    this._resamplePos = 0;

    // RMS-based VAD state
    this._rmsThreshold = 0.015;       // silence threshold (tunable)
    this._silenceChunks = 0;          // consecutive silent chunks
    this._silenceLimit = 12;          // 12 chunks × 100ms = 1.2s silence → activityEnd
    this._isSpeaking = false;         // current speech state
    this._speechStartChunks = 2;      // 2 chunks of speech to confirm start (200ms)
    this._speechChunks = 0;           // consecutive speech chunks
  }
  process(inputs) {
    const input = inputs[0];
    if (!input || !input[0]) return true;
    const samples = input[0];

    // Simple linear-interpolation downsample from native rate to 16kHz
    for (let i = 0; i < samples.length; i++) {
      this._resamplePos += 1;
      if (this._resamplePos >= this._ratio) {
        this._resamplePos -= this._ratio;
        this._buffer.push(samples[i]);
      }
    }

    while (this._buffer.length >= this._targetSamples) {
      const chunk = this._buffer.splice(0, this._targetSamples);

      // Calculate RMS energy for VAD
      let sumSq = 0;
      for (let i = 0; i < chunk.length; i++) {
        sumSq += chunk[i] * chunk[i];
      }
      const rms = Math.sqrt(sumSq / chunk.length);

      // VAD state machine
      if (rms >= this._rmsThreshold) {
        this._silenceChunks = 0;
        this._speechChunks++;
        if (!this._isSpeaking && this._speechChunks >= this._speechStartChunks) {
          this._isSpeaking = true;
          this.port.postMessage({ type: 'vad', speaking: true });
        }
      } else {
        this._speechChunks = 0;
        if (this._isSpeaking) {
          this._silenceChunks++;
          if (this._silenceChunks >= this._silenceLimit) {
            this._isSpeaking = false;
            this.port.postMessage({ type: 'vad', speaking: false });
          }
        }
      }

      // Convert to PCM16 and send audio
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

export function Interpreter({
  locale,
  onBack,
  onToggleSidebar,
  sidebarOpen,
}: InterpreterProps) {
  void onBack; // available for future navigation
  const [status, setStatus] = useState<ConnectionStatus>("idle");
  const [provider, setProvider] = useState<VoiceProvider>("gemini");
  // Default: auto-detect source language, translate to English, bidirectional
  const [sourceLang, setSourceLang] = useState("auto");
  const [targetLang, setTargetLang] = useState("en");
  const [bidirectional, setBidirectional] = useState(true);
  const [interpDirection, setInterpDirection] = useState<InterpDirection>("bidirectional");
  const [speakerMode, setSpeakerMode] = useState(true); // true=speaker(mute mic during playback), false=earphone(simultaneous)
  const [echoCancellation, setEchoCancellation] = useState(true);
  const [autoGainControl, setAutoGainControl] = useState(true);
  const [noiseSuppression, setNoiseSuppression] = useState(true);
  const [transcripts, setTranscripts] = useState<Transcript[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [micPermissionDenied, setMicPermissionDenied] = useState(false);

  const wsRef = useRef<WebSocket | null>(null);
  const audioContextRef = useRef<AudioContext | null>(null);
  const workletNodeRef = useRef<AudioWorkletNode | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const playbackCtxRef = useRef<AudioContext | null>(null);
  const micMutedRef = useRef(false); // mute mic while audio plays back
  const speakerModeRef = useRef(true); // ref mirror for use in callbacks
  const activeProviderRef = useRef<VoiceProvider>("gemini");
  const inputSampleRateRef = useRef(16000); // from server ready message
  const transcriptEndRef = useRef<HTMLDivElement>(null);

  // Keep ref in sync with state for use inside callbacks
  useEffect(() => { speakerModeRef.current = speakerMode; }, [speakerMode]);

  // Auto-scroll transcripts
  useEffect(() => {
    transcriptEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [transcripts]);

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      endSession();
    };
  }, []);

  const sessionStartRef = useRef<number>(0);

  const addTranscript = useCallback((type: "input" | "output" | "system", text: string, append = false) => {
    const now = Date.now();
    const elapsed = sessionStartRef.current ? `+${((now - sessionStartRef.current) / 1000).toFixed(1)}s` : "0.0s";
    const prefix = type === "system" ? `[${elapsed}] ` : "";
    setTranscripts((prev) => {
      // Append mode: merge with the last transcript of same type
      if (append && prev.length > 0) {
        const last = prev[prev.length - 1];
        if (last.type === type) {
          return [
            ...prev.slice(0, -1),
            { ...last, text: last.text + text, timestamp: now },
          ];
        }
      }
      return [
        ...prev,
        { id: crypto.randomUUID(), type, text: `${prefix}${text}`, timestamp: now },
      ];
    });
  }, []);

  const playbackChunkCountRef = useRef(0);

  // Sequential audio playback scheduling.
  // Each chunk is scheduled to play right after the previous one ends,
  // preventing overlapping playback that sounds like fast-forward noise.
  const nextPlayTimeRef = useRef(0);
  const muteTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const playAudioChunk = useCallback(async (pcmData: ArrayBuffer) => {
    if (!playbackCtxRef.current) {
      playbackCtxRef.current = new AudioContext({ sampleRate: 24000 });
      addTranscript("system", `Playback AudioContext: requested 24000Hz, actual ${playbackCtxRef.current.sampleRate}Hz`);
    }
    const ctx = playbackCtxRef.current;

    // Resume if suspended (browser autoplay policy)
    if (ctx.state === "suspended") {
      await ctx.resume();
    }

    const int16 = new Int16Array(pcmData);
    const float32 = new Float32Array(int16.length);
    for (let i = 0; i < int16.length; i++) {
      float32[i] = int16[i] / 32768;
    }
    const buffer = ctx.createBuffer(1, float32.length, 24000);
    buffer.getChannelData(0).set(float32);
    const source = ctx.createBufferSource();
    source.buffer = buffer;
    source.connect(ctx.destination);

    // Schedule this chunk to play after the previous one finishes.
    const now = ctx.currentTime;
    // Reset schedule if it fell behind (e.g. after a gap between turns)
    // No max cap — interrupted event handles turn boundaries
    if (nextPlayTimeRef.current < now) {
      nextPlayTimeRef.current = now;
    }
    const startTime = nextPlayTimeRef.current;
    const endTime = startTime + buffer.duration;
    nextPlayTimeRef.current = endTime;

    // Mute mic during playback (Gemini + speaker mode only).
    // OpenAI semantic VAD handles echo automatically — no muting needed.
    if (speakerMode && activeProviderRef.current === "gemini") {
      micMutedRef.current = true;
      if (muteTimerRef.current) clearTimeout(muteTimerRef.current);
      const muteMs = (endTime - now) * 1000 + 50; // +50ms safety margin
      muteTimerRef.current = setTimeout(() => {
        micMutedRef.current = false;
        muteTimerRef.current = null;
      }, muteMs);
    }

    source.start(startTime);
    playbackChunkCountRef.current++;
    if (playbackChunkCountRef.current === 1 || playbackChunkCountRef.current % 20 === 0) {
      console.log(`[audio] chunk #${playbackChunkCountRef.current}, ctx.state=${ctx.state}, scheduled at ${startTime.toFixed(2)}, now=${now.toFixed(2)}, samples=${int16.length}`);
    }
  }, [speakerMode]);

  const startSession = useCallback(async () => {
    setError(null);
    setStatus("connecting");
    setTranscripts([]);
    sessionStartRef.current = Date.now();
    addTranscript("system", `Connecting to server... (${apiClient.getServerUrl()})`);

    try {
      // 1. Create voice session via local MoA gateway
      //    Voice interpretation runs locally — no Railway server involved.
      //    Gemini Live API is called directly from the local agent.
      const serverUrl = apiClient.getServerUrl();
      const token = apiClient.getToken();
      if (!token) {
        throw new Error("Not authenticated — please log in first");
      }

      const res = await fetch(`${serverUrl}/api/voice/sessions`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${token}`,
        },
        body: JSON.stringify({
          source_language: sourceLang,
          target_language: targetLang,
          bidirectional,
          provider,
        }),
      });

      if (!res.ok) {
        const data = await res.json().catch(() => ({ error: "Failed to create session" }));
        throw new Error(data.error || `Session creation failed (${res.status})`);
      }

      const session = await res.json();
      const sessionId = session.session_id;
      addTranscript("system", `Session created: ${sessionId}`);

      // 2. Connect WebSocket (pass token as query param since WS can't use headers)
      const wsUrl = serverUrl.replace(/^http/, "ws") + `/api/voice/interpret?session_id=${sessionId}&token=${token}`;
      addTranscript("system", "Opening WebSocket...");
      const ws = new WebSocket(wsUrl);
      wsRef.current = ws;

      ws.binaryType = "arraybuffer";

      ws.onopen = () => {
        setStatus("ready");
        addTranscript("system", "WebSocket connected — waiting for Gemini setup...");
      };

      let audioChunksReceived = 0;

      ws.onmessage = (event) => {
        if (event.data instanceof ArrayBuffer) {
          // Binary: translated audio PCM
          audioChunksReceived++;
          if (audioChunksReceived === 1) {
            addTranscript("system", `First audio response received (${event.data.byteLength} bytes)`);
          }
          playAudioChunk(event.data);
        } else {
          // Text: JSON event
          try {
            const msg = JSON.parse(event.data);
            switch (msg.type) {
              case "ready": {
                const serverProvider = msg.provider || "gemini";
                const sampleRate = msg.input_sample_rate || 16000;
                activeProviderRef.current = serverProvider;
                inputSampleRateRef.current = sampleRate;
                setStatus("listening");
                const vadMode = serverProvider === "openai" ? "semantic VAD (server)" : "RMS VAD (client)";
                addTranscript("system", `${serverProvider === "openai" ? "OpenAI Realtime" : "Gemini Live"} ready — ${vadMode}, input ${sampleRate}Hz`);
                startMicrophone();
                break;
              }
              case "input_transcript":
                if (msg.text) addTranscript("input", msg.text);
                break;
              case "output_transcript":
                if (msg.text) addTranscript("output", msg.text, true);
                break;
              case "turn_complete":
                addTranscript("system", "Turn complete");
                setStatus((prev) => {
                  if (prev === "stopping") {
                    // Wait for ALL scheduled audio to finish before closing.
                    // Audio chunks arrive in bursts and are scheduled sequentially
                    // via nextPlayTimeRef, so remaining can be several seconds.
                    const ctx = playbackCtxRef.current;
                    const remaining = ctx
                      ? Math.max(0, (nextPlayTimeRef.current - ctx.currentTime) * 1000)
                      : 0;
                    const delayMs = remaining + 1000; // +1s safety margin
                    addTranscript("system", `Drain: ${(remaining / 1000).toFixed(1)}s audio remaining — closing in ${(delayMs / 1000).toFixed(1)}s`);
                    setTimeout(() => endSession(), delayMs);
                    return prev;
                  }
                  return prev;
                });
                break;
              case "interrupted": {
                addTranscript("system", "Interrupted — new speech detected");
                // Cancel all scheduled audio by closing current playback context
                const oldCtx = playbackCtxRef.current;
                if (oldCtx) {
                  oldCtx.close();
                  playbackCtxRef.current = null;
                }
                nextPlayTimeRef.current = 0;
                playbackChunkCountRef.current = 0;
                micMutedRef.current = false;
                if (muteTimerRef.current) {
                  clearTimeout(muteTimerRef.current);
                  muteTimerRef.current = null;
                }
                break;
              }
              case "error":
                addTranscript("system", `Error: ${msg.message}`);
                setError(msg.message);
                break;
              default:
                addTranscript("system", `Event: ${msg.type}`);
                break;
            }
          } catch {
            // ignore parse errors
          }
        }
      };

      ws.onerror = () => {
        // Ignore errors during intentional close (stop button or draining)
        if (wsRef.current === null) return;
        setStatus((prev) => {
          if (prev === "stopping") return prev; // ignore during drain
          addTranscript("system", "WebSocket error");
          setError("WebSocket connection error");
          return "error";
        });
      };

      ws.onclose = (event) => {
        addTranscript("system", `WebSocket closed (code: ${event.code}, reason: "${event.reason || "none"}", clean: ${event.wasClean})`);
        addTranscript("system", `Stats: ${audioChunksReceived} audio chunks received`);
        // Only reset if this ws is still the active one (avoid stale closure killing new session)
        if (wsRef.current === null || wsRef.current === ws) {
          setStatus((prev) => prev !== "idle" ? "idle" : prev);
        }
      };
    } catch (e) {
      const msg = e instanceof Error ? e.message : "Connection failed";
      addTranscript("system", `Connection failed: ${msg}`);
      setError(msg);
      setStatus("error");
    }
  }, [sourceLang, targetLang, bidirectional, provider, addTranscript, playAudioChunk, status]);

  const startMicrophone = useCallback(async () => {
    try {
      const stream = await navigator.mediaDevices.getUserMedia({
        audio: {
          channelCount: 1,
          echoCancellation,
          autoGainControl,
          noiseSuppression,
        },
      });
      streamRef.current = stream;

      // Use native sample rate — forcing 16kHz kills mic on macOS WebKit (Tauri).
      // Downsampling happens inside the AudioWorklet processor.
      const ctx = new AudioContext();
      audioContextRef.current = ctx;
      const targetRate = inputSampleRateRef.current;
      addTranscript("system", `Audio: native ${ctx.sampleRate}Hz → downsample to ${targetRate}Hz`);

      // Register worklet
      const blob = new Blob([WORKLET_CODE], { type: "application/javascript" });
      const url = URL.createObjectURL(blob);
      await ctx.audioWorklet.addModule(url);
      URL.revokeObjectURL(url);

      const source = ctx.createMediaStreamSource(stream);
      const workletNode = new AudioWorkletNode(ctx, "pcm-capture-processor", {
        processorOptions: { sourceRate: ctx.sampleRate, targetRate },
      });
      workletNodeRef.current = workletNode;

      let sentChunks = 0;
      workletNode.port.onmessage = (e) => {
        const ws = wsRef.current;
        if (!ws || ws.readyState !== WebSocket.OPEN) return;

        // Distinguish VAD messages from audio data
        if (e.data && typeof e.data === "object" && !(e.data instanceof ArrayBuffer) && e.data.type === "vad") {
          // Only send activityStart/End for Gemini (manual/RMS VAD).
          // OpenAI uses server-side semantic VAD — no client signals needed.
          if (activeProviderRef.current === "gemini") {
            if (e.data.speaking) {
              ws.send(JSON.stringify({ type: "activity_start" }));
              addTranscript("system", "VAD: speech detected → activityStart");
            } else {
              ws.send(JSON.stringify({ type: "activity_end" }));
              addTranscript("system", "VAD: silence detected → activityEnd");
            }
          }
          return;
        }

        // Audio data (ArrayBuffer) — always send
        ws.send(e.data);
        sentChunks++;
        if (sentChunks === 1) {
          addTranscript("system", `First audio chunk sent (${e.data.byteLength} bytes)`);
        } else if (sentChunks % 50 === 0) {
          addTranscript("system", `Audio chunks sent: ${sentChunks}`);
        }
      };

      source.connect(workletNode);
      workletNode.connect(ctx.destination); // needed to keep processor alive

      // RMS VAD in worklet sends activityStart when speech is detected,
      // ensuring it arrives before audio chunks (VAD needs 200ms of speech to trigger).

      addTranscript("system", "Microphone active — listening...");
      setStatus("listening");
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      const name = e instanceof DOMException ? e.name : "";
      console.error("Microphone error:", e);
      addTranscript("system", `Microphone error: ${msg}`);

      if (name === "NotAllowedError" || name === "NotFoundError" || msg.includes("Permission denied")) {
        setMicPermissionDenied(true);
      } else {
        setError(`Microphone error: ${msg}`);
      }
      setStatus("error");
    }
  }, [addTranscript, echoCancellation, autoGainControl, noiseSuppression]);

  // Stop microphone only — keep WS and playback alive for remaining translation
  const stopMicrophone = useCallback(() => {
    if (workletNodeRef.current) {
      workletNodeRef.current.disconnect();
      workletNodeRef.current = null;
    }
    if (audioContextRef.current) {
      audioContextRef.current.close();
      audioContextRef.current = null;
    }
    if (streamRef.current) {
      streamRef.current.getTracks().forEach((t) => t.stop());
      streamRef.current = null;
    }
  }, []);

  // Full cleanup — close WS, defer playback ctx close until audio finishes
  const endSession = useCallback(() => {
    stopMicrophone();

    if (wsRef.current) {
      wsRef.current.close();
      wsRef.current = null;
    }

    // Don't close playbackCtx immediately — scheduled audio is still playing.
    // Wait until all scheduled audio finishes, then close.
    // Capture ref so delayed close won't affect a new session's ctx.
    const ctx = playbackCtxRef.current;
    playbackCtxRef.current = null; // detach immediately so new session gets fresh ctx
    if (ctx) {
      const remaining = Math.max(0, (nextPlayTimeRef.current - ctx.currentTime) * 1000);
      const closeDelay = remaining + 500; // +500ms safety
      setTimeout(() => {
        ctx.close();
      }, closeDelay);
    }

    // Reset all playback state for next session
    nextPlayTimeRef.current = 0;
    micMutedRef.current = false;
    playbackChunkCountRef.current = 0;
    if (muteTimerRef.current) {
      clearTimeout(muteTimerRef.current);
      muteTimerRef.current = null;
    }

    setStatus("idle");
  }, [stopMicrophone]);

  // Graceful stop: activityEnd (if still speaking) → mic off → notify server → wait for turn_complete → end
  const handleStop = useCallback(() => {
    addTranscript("system", "Stopping — mic off...");

    // For Gemini: force activityEnd in case VAD hasn't sent it yet (user pressed Stop mid-speech)
    // For OpenAI: semantic VAD handles this server-side, no signal needed
    if (activeProviderRef.current === "gemini" && wsRef.current && wsRef.current.readyState === WebSocket.OPEN) {
      try {
        wsRef.current.send(JSON.stringify({ type: "activity_end" }));
        addTranscript("system", "Stop: activityEnd sent (safety)");
      } catch {
        // ignore send errors
      }
    }

    stopMicrophone();

    if (wsRef.current && wsRef.current.readyState === WebSocket.OPEN) {
      try {
        wsRef.current.send(JSON.stringify({ type: "stop" }));
      } catch {
        // ignore send errors
      }
    }

    addTranscript("system", "Waiting for remaining translation...");
    setStatus("stopping");

    // Safety timeout: if turn_complete never arrives, force end after 10s
    // Capture current ws ref so timeout won't kill a new session
    const currentWs = wsRef.current;
    setTimeout(() => {
      if (wsRef.current !== currentWs) return; // new session started, skip
      setStatus((prev) => {
        if (prev === "stopping") {
          endSession();
        }
        return prev === "stopping" ? "idle" : prev;
      });
    }, 10000);
  }, [stopMicrophone, endSession]);

  const swapLanguages = () => {
    setSourceLang(targetLang);
    setTargetLang(sourceLang);
  };

  const isActive = status === "listening" || status === "ready" || status === "connecting" || status === "stopping";

  return (
    <div className="interpreter-page">
      {/* Header */}
      <div className="chat-header">
        <button className="chat-header-toggle" onClick={onToggleSidebar}>
          {sidebarOpen ? "\u2715" : "\u2630"}
        </button>
        <div className="header-title">{t("interpreter", locale)}</div>
        <div className={`connection-badge ${status === "listening" ? "connected" : ""}`}>
          {status === "idle"
            ? t("interpreter_idle", locale)
            : status === "connecting"
            ? t("interpreter_connecting", locale)
            : status === "listening"
            ? t("interpreter_listening", locale)
            : status === "stopping"
            ? t("interpreter_stopping", locale)
            : status === "ready"
            ? t("interpreter_ready", locale)
            : t("interpreter_error", locale)}
        </div>
      </div>

      {/* Language selector */}
      <div className="interpreter-controls">
        <div className="lang-selector">
          <div className="lang-label">
            {locale === "ko" ? "입력 언어" : "Input"}
          </div>
          <select
            value={sourceLang}
            onChange={(e) => setSourceLang(e.target.value)}
            disabled={isActive}
            className="lang-select"
          >
            {LANGUAGES.map((l) => (
              <option key={l.code} value={l.code}>
                {l.flag} {l.name}
              </option>
            ))}
          </select>

          <button
            className="lang-swap-btn"
            onClick={swapLanguages}
            disabled={isActive || sourceLang === "auto"}
            title={locale === "ko" ? "언어 교환" : "Swap languages"}
          >
            ⇄
          </button>

          <div className="lang-label">
            {locale === "ko" ? "출력 언어" : "Output"}
          </div>
          <select
            value={targetLang}
            onChange={(e) => setTargetLang(e.target.value)}
            disabled={isActive}
            className="lang-select"
          >
            {LANGUAGES.filter((l) => l.code !== "auto").map((l) => (
              <option key={l.code} value={l.code}>
                {l.flag} {l.name}
              </option>
            ))}
          </select>
        </div>

        {/* Interpretation direction: unidirectional or bidirectional */}
        <div className="interp-direction-toggle">
          <button
            className={`direction-btn ${interpDirection === "unidirectional" ? "active" : ""}`}
            onClick={() => {
              setInterpDirection("unidirectional");
              setBidirectional(false);
            }}
            disabled={isActive}
            title={locale === "ko" ? "일방향 통역: 입력→출력 방향만 통역" : "One-way: input→output only"}
          >
            {locale === "ko" ? "일방향 →" : "One-way →"}
          </button>
          <button
            className={`direction-btn ${interpDirection === "bidirectional" ? "active" : ""}`}
            onClick={() => {
              setInterpDirection("bidirectional");
              setBidirectional(true);
            }}
            disabled={isActive}
            title={locale === "ko" ? "쌍방향 통역: 양쪽 언어 모두 자동 감지하여 통역" : "Two-way: auto-detect and interpret both languages"}
          >
            {locale === "ko" ? "쌍방향 ⇄" : "Two-way ⇄"}
          </button>
        </div>

        <div className="interpreter-row">
          <select
            value={provider}
            onChange={(e) => setProvider(e.target.value as VoiceProvider)}
            disabled={isActive}
            className="provider-select"
            title={locale === "ko" ? "음성 AI 공급자 (본인 API 키 필요)" : "Voice AI provider (requires your own API key)"}
          >
            <option value="gemini">🤖 Gemini</option>
            <option value="openai">🧠 GPT</option>
          </select>

          <button
            className={`audio-mode-btn ${speakerMode ? "speaker" : "earphone"}`}
            onClick={() => setSpeakerMode((v) => !v)}
            title={speakerMode
              ? (locale === "ko" ? "스피커 모드 (순차 통역)" : "Speaker mode (sequential)")
              : (locale === "ko" ? "이어폰 모드 (동시 통역)" : "Earphone mode (simultaneous)")}
          >
            {speakerMode ? "🔊" : "🎧"}{" "}
            {speakerMode
              ? (locale === "ko" ? "스피커" : "Speaker")
              : (locale === "ko" ? "이어폰" : "Earphone")}
          </button>
        </div>

        {/* User API key notice */}
        <div className="api-key-notice">
          {locale === "ko"
            ? "⚠️ 동시통역은 본인의 API 키가 필요합니다 (설정에서 입력)"
            : "⚠️ Voice interpretation requires your own API key (enter in Settings)"}
        </div>
      </div>

      {/* Audio processing toggles */}
      <div className="audio-processing-toggles">
        <label className="audio-toggle" title="Echo Cancellation — removes speaker feedback from mic input">
          <input type="checkbox" checked={echoCancellation} onChange={(e) => setEchoCancellation(e.target.checked)} disabled={isActive} />
          <span>AEC</span>
        </label>
        <label className="audio-toggle" title="Auto Gain Control — normalizes mic volume automatically">
          <input type="checkbox" checked={autoGainControl} onChange={(e) => setAutoGainControl(e.target.checked)} disabled={isActive} />
          <span>AGC</span>
        </label>
        <label className="audio-toggle" title="Noise Suppression — filters background noise">
          <input type="checkbox" checked={noiseSuppression} onChange={(e) => setNoiseSuppression(e.target.checked)} disabled={isActive} />
          <span>NS</span>
        </label>
      </div>

      {/* Subtitle / transcript area */}
      <div className="interpreter-transcripts">
        {transcripts.length === 0 && status === "idle" && (
          <div className="interpreter-empty">
            <div className="interpreter-empty-icon">🎙️</div>
            <p>{t("interpreter_hint", locale)}</p>
            <p className="interpreter-subtitle-hint">
              {locale === "ko"
                ? "통역 중 화면에 자막이 실시간으로 표시됩니다"
                : "Subtitles will appear on screen during interpretation"}
            </p>
          </div>
        )}
        {transcripts.map((tr) => (
          <div key={tr.id} className={`transcript-item transcript-${tr.type}`}>
            <span className="transcript-badge">
              {tr.type === "input" ? "🗣️" : tr.type === "output" ? "🔊" : "⚙️"}
            </span>
            <div className="transcript-content">
              {tr.type !== "system" && (
                <span className="transcript-lang-label">
                  {tr.type === "input"
                    ? (sourceLang === "auto"
                      ? (locale === "ko" ? "원문 (자동감지)" : "Source (auto)")
                      : LANGUAGES.find((l) => l.code === sourceLang)?.name || sourceLang)
                    : LANGUAGES.find((l) => l.code === targetLang)?.name || targetLang}
                </span>
              )}
              <span className={`transcript-text ${tr.type !== "system" ? "subtitle-text" : ""}`}>
                {tr.text}
              </span>
            </div>
          </div>
        ))}
        {status === "listening" && (
          <div className="transcript-item transcript-listening">
            <span className="listening-pulse" />
            <span className="transcript-text">{t("interpreter_listening_hint", locale)}</span>
          </div>
        )}
        <div ref={transcriptEndRef} />
      </div>

      {/* Error display */}
      {error && (
        <div className="interpreter-error">
          {error}
        </div>
      )}

      {/* Microphone permission denied guide */}
      {micPermissionDenied && (
        <div className="mic-permission-overlay" onClick={() => setMicPermissionDenied(false)}>
          <div className="mic-permission-modal" onClick={(e) => e.stopPropagation()}>
            <div className="mic-permission-icon">🎙️</div>
            <h3 className="mic-permission-title">
              {locale === "ko" ? "마이크 접근이 차단되었습니다" : "Microphone Access Blocked"}
            </h3>
            <p className="mic-permission-desc">
              {locale === "ko"
                ? "음성 통역을 사용하려면 마이크 권한이 필요합니다. 아래 안내에 따라 설정해주세요."
                : "Microphone permission is required for voice interpretation. Follow the steps below."}
            </p>

            <div className="mic-permission-steps">
              {/* iOS */}
              <details className="mic-permission-platform">
                <summary>iPhone / iPad</summary>
                <ol>
                  <li>{locale === "ko" ? "설정 앱을 엽니다" : "Open Settings"}</li>
                  <li>{locale === "ko" ? "MoA (또는 브라우저 앱)를 찾습니다" : "Find MoA (or your browser app)"}</li>
                  <li>{locale === "ko" ? "마이크를 켭니다" : "Turn on Microphone"}</li>
                  <li>{locale === "ko" ? "앱을 다시 열어주세요" : "Reopen the app"}</li>
                </ol>
              </details>

              {/* Android */}
              <details className="mic-permission-platform">
                <summary>Android</summary>
                <ol>
                  <li>{locale === "ko" ? "설정 → 앱 → MoA" : "Settings → Apps → MoA"}</li>
                  <li>{locale === "ko" ? "권한 → 마이크 → 허용" : "Permissions → Microphone → Allow"}</li>
                  <li>{locale === "ko" ? "앱을 다시 열어주세요" : "Reopen the app"}</li>
                </ol>
              </details>

              {/* Desktop */}
              <details className="mic-permission-platform">
                <summary>macOS / Windows</summary>
                <ol>
                  <li>{locale === "ko"
                    ? "macOS: 시스템 설정 → 개인정보 보호 → 마이크 → MoA 허용"
                    : "macOS: System Settings → Privacy → Microphone → Allow MoA"}</li>
                  <li>{locale === "ko"
                    ? "Windows: 설정 → 개인정보 → 마이크 → MoA 허용"
                    : "Windows: Settings → Privacy → Microphone → Allow MoA"}</li>
                </ol>
              </details>
            </div>

            <button
              className="mic-permission-retry"
              onClick={() => {
                setMicPermissionDenied(false);
                setError(null);
                setStatus("idle");
              }}
            >
              {locale === "ko" ? "닫기" : "Close"}
            </button>
          </div>
        </div>
      )}

      {/* Action button */}
      <div className="interpreter-actions">
        {!isActive ? (
          <button className="interpreter-start-btn" onClick={startSession}>
            🎙️ {t("interpreter_start", locale)}
          </button>
        ) : (
          <button
            className="interpreter-stop-btn"
            onClick={handleStop}
            disabled={status === "stopping"}
          >
            {status === "stopping"
              ? `⏳ ${t("interpreter_stopping", locale)}`
              : `⏹ ${t("interpreter_stop", locale)}`}
          </button>
        )}
      </div>
    </div>
  );
}
