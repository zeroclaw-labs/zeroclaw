import { useCallback, useRef, useState } from "react";
import { Audio } from "expo-av";

import { log } from "../logger";
import { transcribeWithDeepgram } from "../api/mobileclaw";

export type VoiceState = "idle" | "recording" | "transcribing";

function meteringTo01(metering?: number | null): number {
  // expo-av metering is typically in dBFS: 0 = max, negative = quieter.
  if (typeof metering !== "number" || Number.isNaN(metering)) return 0;
  const clamped = Math.max(-60, Math.min(0, metering));
  const t = (clamped + 60) / 60; // 0..1
  return Math.max(0, Math.min(1, Math.pow(t, 1.6)));
}

export function useVoiceRecording(deepgramApiKey?: string) {
  const [state, setState] = useState<VoiceState>("idle");
  const [volume, setVolume] = useState(0);
  const [transcript, setTranscript] = useState("");
  const [interimText, setInterimText] = useState("");

  const recordingRef = useRef<Audio.Recording | null>(null);
  const startedRef = useRef(false);

  const start = useCallback(async (): Promise<boolean> => {
    setTranscript("");
    setInterimText("");
    setVolume(0);
    startedRef.current = true;

    try {
      const perm = await Audio.requestPermissionsAsync();
      if (!perm.granted) throw new Error("Microphone permission denied");

      await Audio.setAudioModeAsync({
        allowsRecordingIOS: true,
        playsInSilentModeIOS: true,
        staysActiveInBackground: false
      });
    } catch (err) {
      log("error", "Microphone permission error", { error: String(err) });
      setInterimText("Microphone permission is required");
      setState("idle");
      recordingRef.current = null;
      startedRef.current = false;
      return false;
    }

    try {
      setInterimText("Listening…");
      const rec = new Audio.Recording();
      recordingRef.current = rec;

      rec.setOnRecordingStatusUpdate((anyStatus: any) => {
        if (!startedRef.current) return;
        if (anyStatus?.isRecording) {
          const next = meteringTo01(anyStatus.metering);
          setVolume(next);
        }
      });

      await rec.prepareToRecordAsync({
        ...Audio.RecordingOptionsPresets.HIGH_QUALITY,
        isMeteringEnabled: true,
      } as any);
      await rec.startAsync();
      setState("recording");
      log("debug", "Voice recording started");
      return true;
    } catch (err) {
      log("error", "Failed to start voice recording", { error: String(err) });
      setInterimText("Mic failed");
      setState("idle");
      startedRef.current = false;
      recordingRef.current = null;
      return false;
    }
  }, [deepgramApiKey]);

  const stop = useCallback(async (): Promise<string> => {
    setState("transcribing");
    startedRef.current = false;

    const rec = recordingRef.current;
    recordingRef.current = null;
    if (!rec) {
      setInterimText("");
      setVolume(0);
      setState("idle");
      return "";
    }

    try {
      setInterimText("Transcribing…");
      await rec.stopAndUnloadAsync();
      const uri = rec.getURI();
      log("debug", "Voice recording stopped", { uri });
      if (!uri) throw new Error("Missing recording URI");
      const text = await transcribeWithDeepgram(uri, deepgramApiKey || "");
      const finalText = String(text || "").trim();
      if (!finalText) {
        setTranscript("");
        setInterimText("Didn't catch that. Try again.");
        return "";
      }
      setTranscript(finalText);
      setInterimText(finalText);
      return finalText;
    } catch (err) {
      log("error", "Voice transcription failed", { error: String(err) });
      setInterimText("Transcription failed");
      return "";
    } finally {
      setVolume(0);
      setState("idle");
    }
  }, []);

  const cancel = useCallback(async () => {
    startedRef.current = false;
    try {
      await recordingRef.current?.stopAndUnloadAsync();
    } catch {
      // ignore
    }
    recordingRef.current = null;
    setState("idle");
    setVolume(0);
    setInterimText("");
    setTranscript("");
  }, []);

  return { state, volume, transcript, interimText, start, stop, cancel } as const;
}
