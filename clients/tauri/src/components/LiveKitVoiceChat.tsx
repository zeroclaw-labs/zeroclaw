/**
 * LiveKit Voice Chat Component
 *
 * Connects to a LiveKit room for real-time voice conversation with
 * the MoA agent. Audio flows through LiveKit's WebRTC transport:
 *
 *   User Mic → [Audio Track A] → LiveKit Server → Agent (Deepgram STT → LLM → Typecast TTS)
 *   User Speaker ← [Audio Track B] ← LiveKit Server ← Agent
 *
 * Echo loop is structurally prevented because STT only receives
 * Track A (user mic), never Track B (agent speaker output).
 *
 * Stack:
 *   - STT: Deepgram Nova-3 (server-side via LiveKit Agent)
 *   - LLM: User-selected / default Gemini 3.1 Flash
 *   - TTS: Typecast (server-side via LiveKit Agent)
 *   - VAD: Silero (server-side via LiveKit Agent)
 *   - Transport: LiveKit WebRTC
 */

import { useState, useRef, useCallback, useEffect } from "react";
import {
  Room,
  RoomEvent,
  Track,
  RemoteTrackPublication,
  RemoteParticipant,
  ConnectionState,
  DisconnectReason,
} from "livekit-client";
import type { Locale } from "../lib/i18n";
import { apiClient } from "../lib/api";

interface LiveKitVoiceChatProps {
  locale: Locale;
  onClose: () => void;
  initialMode?: "pipeline" | "s2s";
}

type VoiceChatStatus =
  | "idle"
  | "connecting"
  | "connected"
  | "speaking"
  | "agent_speaking"
  | "error"
  | "disconnecting";

interface ChatTranscript {
  id: string;
  role: "user" | "agent" | "system";
  text: string;
  timestamp: number;
  isFinal: boolean;
}

type VoiceMode = "pipeline" | "s2s";

export function LiveKitVoiceChat({ locale, onClose, initialMode }: LiveKitVoiceChatProps) {
  const [status, setStatus] = useState<VoiceChatStatus>("idle");
  const [voiceMode, setVoiceMode] = useState<VoiceMode>(initialMode || "pipeline");
  const [transcripts, setTranscripts] = useState<ChatTranscript[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [isMuted, setIsMuted] = useState(false);

  const roomRef = useRef<Room | null>(null);
  const transcriptEndRef = useRef<HTMLDivElement>(null);

  // Auto-scroll transcripts
  useEffect(() => {
    transcriptEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [transcripts]);

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      disconnect();
    };
  }, []);

  const addTranscript = useCallback(
    (role: "user" | "agent" | "system", text: string, isFinal = true) => {
      setTranscripts((prev) => {
        // For non-final (interim) transcripts, replace the last one of same role
        if (!isFinal && prev.length > 0) {
          const last = prev[prev.length - 1];
          if (last.role === role && !last.isFinal) {
            return [
              ...prev.slice(0, -1),
              { ...last, text, timestamp: Date.now() },
            ];
          }
        }
        return [
          ...prev,
          {
            id: crypto.randomUUID(),
            role,
            text,
            timestamp: Date.now(),
            isFinal,
          },
        ];
      });
    },
    []
  );

  const connect = useCallback(async () => {
    setError(null);
    setStatus("connecting");
    setTranscripts([]);
    addTranscript("system", locale === "ko" ? "연결 중..." : "Connecting...");

    try {
      const serverUrl = apiClient.getServerUrl();
      const authToken = apiClient.getToken();
      if (!authToken) {
        throw new Error(
          locale === "ko"
            ? "로그인이 필요합니다"
            : "Not authenticated — please log in first"
        );
      }

      // 1. Get LiveKit token from our backend
      const res = await fetch(`${serverUrl}/api/livekit/token`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${authToken}`,
        },
        body: JSON.stringify({
          room: `moa-voice-${Date.now()}`,
          identity: `user-${Date.now()}`,
          metadata: JSON.stringify({
            language: locale === "ko" ? "ko" : "en",
            tier: "premium",
            voice_mode: voiceMode,
          }),
        }),
      });

      if (!res.ok) {
        const data = await res
          .json()
          .catch(() => ({ error: "Failed to get token" }));
        throw new Error(
          data.error || `Token request failed (${res.status})`
        );
      }

      const { token, url } = await res.json();
      addTranscript(
        "system",
        locale === "ko"
          ? "LiveKit 토큰 발급 완료 — 룸 연결 중..."
          : "LiveKit token received — connecting to room..."
      );

      // 2. Create and connect LiveKit Room
      const room = new Room({
        adaptiveStream: true,
        dynacast: true,
        // Audio processing for echo cancellation
        audioCaptureDefaults: {
          echoCancellation: true,
          autoGainControl: true,
          noiseSuppression: true,
          channelCount: 1,
        },
      });

      roomRef.current = room;

      // 3. Set up event handlers
      room.on(RoomEvent.Connected, () => {
        setStatus("connected");
        addTranscript(
          "system",
          locale === "ko"
            ? "연결 완료 — 말씀하세요!"
            : "Connected — start speaking!"
        );
      });

      room.on(RoomEvent.Disconnected, (reason?: DisconnectReason) => {
        setStatus("idle");
        addTranscript(
          "system",
          locale === "ko"
            ? `연결 종료 (${reason || "정상"})`
            : `Disconnected (${reason || "normal"})`
        );
      });

      // Handle agent audio track subscription (agent speaking)
      room.on(
        RoomEvent.TrackSubscribed,
        (
          track: RemoteTrackPublication["track"],
          _pub: RemoteTrackPublication,
          participant: RemoteParticipant
        ) => {
          if (track && track.kind === Track.Kind.Audio) {
            // Attach agent audio to a hidden <audio> element for playback
            const audioEl = track.attach();
            audioEl.id = `agent-audio-${participant.identity}`;
            audioEl.style.display = "none";
            document.body.appendChild(audioEl);
            addTranscript(
              "system",
              locale === "ko"
                ? "AI 에이전트 음성 트랙 연결됨"
                : "Agent audio track connected"
            );
          }
        }
      );

      room.on(
        RoomEvent.TrackUnsubscribed,
        (
          track: RemoteTrackPublication["track"],
          _publication: RemoteTrackPublication,
          _participant: RemoteParticipant
        ) => {
          if (track) {
            const elements = track.detach();
            elements.forEach((el) => el.remove());
          }
        }
      );

      // Handle transcription events from the agent
      // LiveKit Agents send transcriptions via data messages
      room.on(RoomEvent.DataReceived, (data: Uint8Array) => {
        try {
          const text = new TextDecoder().decode(data);
          const msg = JSON.parse(text);

          if (msg.type === "transcription" || msg.type === "agent_transcription") {
            const role = msg.role === "user" ? "user" : "agent";
            addTranscript(role, msg.text, msg.is_final !== false);
          }
        } catch {
          // Not a JSON message, ignore
        }
      });

      // Handle agent state changes
      room.on(
        RoomEvent.ParticipantMetadataChanged,
        () => {
          // Could track agent state (thinking, speaking, etc.)
        }
      );

      room.on(RoomEvent.ConnectionStateChanged, (state: ConnectionState) => {
        if (state === ConnectionState.Reconnecting) {
          addTranscript(
            "system",
            locale === "ko" ? "재연결 중..." : "Reconnecting..."
          );
        }
      });

      // 4. Connect to the room and enable microphone
      await room.connect(url, token);
      await room.localParticipant.setMicrophoneEnabled(true);

      addTranscript(
        "system",
        locale === "ko"
          ? "마이크 활성화 — Deepgram STT + Silero VAD + Typecast TTS 파이프라인 가동"
          : "Mic enabled — Deepgram STT + Silero VAD + Typecast TTS pipeline active"
      );
    } catch (e) {
      const msg = e instanceof Error ? e.message : "Connection failed";
      addTranscript("system", `Error: ${msg}`);
      setError(msg);
      setStatus("error");
    }
  }, [locale, voiceMode, addTranscript]);

  const disconnect = useCallback(() => {
    setStatus("disconnecting");
    const room = roomRef.current;
    if (room) {
      // Clean up leaked audio elements
      document.querySelectorAll('[id^="agent-audio-"]').forEach((el) => el.remove());
      room.disconnect();
      roomRef.current = null;
    }
    // Let RoomEvent.Disconnected handler set "idle" — don't race it.
    // Fallback: if no room was connected, set idle directly.
    if (!room) setStatus("idle");
  }, []);

  const toggleMute = useCallback(() => {
    const room = roomRef.current;
    if (room) {
      const newMuted = !isMuted;
      room.localParticipant.setMicrophoneEnabled(!newMuted);
      setIsMuted(newMuted);
      addTranscript(
        "system",
        newMuted
          ? locale === "ko"
            ? "마이크 음소거"
            : "Mic muted"
          : locale === "ko"
          ? "마이크 활성화"
          : "Mic unmuted"
      );
    }
  }, [isMuted, locale, addTranscript]);

  const isActive =
    status === "connecting" ||
    status === "connected" ||
    status === "speaking" ||
    status === "agent_speaking" ||
    status === "disconnecting";

  return (
    <div className="livekit-voice-chat">
      {/* Header */}
      <div className="lk-voice-header">
        <h3>
          {locale === "ko" ? "AI 음성 대화" : "AI Voice Chat"}
        </h3>
        <div className={`lk-status-badge ${status}`}>
          {status === "idle"
            ? locale === "ko"
              ? "대기"
              : "Idle"
            : status === "connecting"
            ? locale === "ko"
              ? "연결 중"
              : "Connecting"
            : status === "connected"
            ? locale === "ko"
              ? "대화 중"
              : "Connected"
            : status === "error"
            ? locale === "ko"
              ? "오류"
              : "Error"
            : locale === "ko"
            ? "연결 해제 중"
            : "Disconnecting"}
        </div>
        <button className="lk-close-btn" onClick={onClose} title="Close">
          ✕
        </button>
      </div>

      {/* Mode selector */}
      <div className="lk-mode-selector">
        <button
          className={`lk-mode-btn ${voiceMode === "pipeline" ? "active" : ""}`}
          onClick={() => setVoiceMode("pipeline")}
          disabled={isActive}
          title={locale === "ko"
            ? "모드 A: Deepgram STT → Gemini LLM → Typecast TTS (고품질, 개별 제어)"
            : "Mode A: Deepgram STT → Gemini LLM → Typecast TTS (high quality, modular)"}
        >
          {locale === "ko" ? "모드 A: 파이프라인" : "Mode A: Pipeline"}
        </button>
        <button
          className={`lk-mode-btn ${voiceMode === "s2s" ? "active" : ""}`}
          onClick={() => setVoiceMode("s2s")}
          disabled={isActive}
          title={locale === "ko"
            ? "모드 B: Gemini 3.1 Flash Live 올인원 (저지연, Google 음성)"
            : "Mode B: Gemini 3.1 Flash Live all-in-one (low latency, Google voice)"}
        >
          {locale === "ko" ? "모드 B: Gemini Live" : "Mode B: Gemini Live"}
        </button>
      </div>

      {/* Stack info */}
      <div className="lk-stack-info">
        {voiceMode === "pipeline" ? (
          <>
            <span>STT: Deepgram Nova-3</span>
            <span>LLM: Gemini 3.1 Flash Lite</span>
            <span>TTS: Typecast</span>
            <span>VAD: Silero</span>
          </>
        ) : (
          <>
            <span>Gemini 3.1 Flash Live (S2S)</span>
            <span>VAD: Silero</span>
          </>
        )}
        <span>Transport: LiveKit</span>
      </div>

      {/* Earphone notice */}
      <div className="lk-earphone-notice">
        {locale === "ko"
          ? "🎧 이어폰을 사용하시면 더 자연스러운 대화가 가능합니다"
          : "🎧 Using earphones provides a better voice chat experience"}
      </div>

      {/* Transcript area */}
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
                ? "에코 방지 기술이 적용되어 스피커 사용 시에도 자연스러운 대화가 가능합니다"
                : "Echo cancellation ensures natural conversation even with speakers"}
            </p>
          </div>
        )}
        {transcripts.map((tr) => (
          <div
            key={tr.id}
            className={`lk-transcript lk-transcript-${tr.role} ${
              !tr.isFinal ? "lk-transcript-interim" : ""
            }`}
          >
            <span className="lk-transcript-badge">
              {tr.role === "user"
                ? "🗣️"
                : tr.role === "agent"
                ? "🤖"
                : "⚙️"}
            </span>
            <span className="lk-transcript-text">{tr.text}</span>
          </div>
        ))}
        {status === "connected" && (
          <div className="lk-transcript lk-listening-pulse">
            <span className="listening-pulse" />
            <span>
              {locale === "ko"
                ? "듣고 있습니다..."
                : "Listening..."}
            </span>
          </div>
        )}
        <div ref={transcriptEndRef} />
      </div>

      {/* Error display */}
      {error && <div className="lk-error">{error}</div>}

      {/* Action buttons */}
      <div className="lk-actions">
        {!isActive ? (
          <button className="lk-start-btn" onClick={connect}>
            🎙️{" "}
            {locale === "ko" ? "음성 대화 시작" : "Start Voice Chat"}
          </button>
        ) : (
          <div className="lk-active-controls">
            <button
              className={`lk-mute-btn ${isMuted ? "muted" : ""}`}
              onClick={toggleMute}
              disabled={status !== "connected"}
            >
              {isMuted ? "🔇" : "🎤"}{" "}
              {isMuted
                ? locale === "ko"
                  ? "음소거 해제"
                  : "Unmute"
                : locale === "ko"
                ? "음소거"
                : "Mute"}
            </button>
            <button
              className="lk-end-btn"
              onClick={disconnect}
              disabled={status === "disconnecting"}
            >
              ⏹{" "}
              {locale === "ko" ? "대화 종료" : "End Chat"}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
