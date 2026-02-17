import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Animated, Easing, Pressable, View, type ViewStyle } from "react-native";
import Svg, { Circle, Defs, Path, RadialGradient, Stop } from "react-native-svg";
import * as Haptics from "expo-haptics";
import { Ionicons } from "@expo/vector-icons";

import { theme } from "../theme";

type Wave = { id: number; intensity: number };

export function VoiceRecordButton({
  onPress,
  onVoiceResult,
  disabled,
  mode = "voice",
  size = 128,
  style,
  onRecordStart,
  onRecordEnd,
  volume: externalVolume,
  testID,
}: {
  onPress?: () => void;
  onVoiceResult?: (text: string) => void;
  disabled?: boolean;
  mode?: "voice" | "send";
  size?: number;
  style?: ViewStyle;
  /** Called when voice mode starts recording. */
  onRecordStart?: () => void | boolean | Promise<void | boolean>;
  /** Called when voice mode stops recording. Should return the transcript. */
  onRecordEnd?: () => Promise<string | undefined>;
  /** Real audio metering volume 0..1.  Falls back to simulated if omitted. */
  volume?: number;
  testID?: string;
}) {
  const [isRecording, setIsRecording] = useState(false);
  const [waves, setWaves] = useState<Wave[]>([]);
  const [isProcessingTap, setIsProcessingTap] = useState(false);

  // Use external volume when provided, otherwise simulate.
  const hasRealVolume = externalVolume !== undefined;
  const [simVolume, setSimVolume] = useState(0);
  const smoothVolume = hasRealVolume ? externalVolume : simVolume;

  const timeRef = useRef(0);
  const lastWaveSpawnRef = useRef(0);
  const animationFrameRef = useRef<number | null>(null);

  const buttonScale = useRef(new Animated.Value(0.96)).current;
  const coreOpacity = useRef(new Animated.Value(0)).current;

  const glow = theme.colors.base.secondary;
  const glowDim = theme.colors.alpha.textFaint;
  const accent = theme.colors.base.accent;
  const highlight = theme.colors.base.primary;

  useEffect(() => {
    let last = Date.now();
    const tick = () => {
      const now = Date.now();
      timeRef.current += now - last;
      last = now;
      animationFrameRef.current = requestAnimationFrame(tick);
    };
    animationFrameRef.current = requestAnimationFrame(tick);
    return () => {
      if (animationFrameRef.current) cancelAnimationFrame(animationFrameRef.current);
    };
  }, []);

  // Simulated volume for visuals (only used when real volume is not provided).
  useEffect(() => {
    if (hasRealVolume) return;
    if (!isRecording) {
      setSimVolume(0);
      return;
    }
    const id = setInterval(() => {
      setSimVolume((prev) => {
        const target = 0.25 + Math.random() * 0.7;
        return prev + (target - prev) * 0.12;
      });
    }, 16);
    return () => clearInterval(id);
  }, [isRecording, hasRealVolume]);

  useEffect(() => {
    const v = Math.max(0, Math.min(1, smoothVolume));
    if (!isRecording || v <= 0.1) return;

    const now = Date.now();
    const minInterval = 90;
    const sinceLast = now - lastWaveSpawnRef.current;
    const spawnChance = v * 0.55;

    if (sinceLast > minInterval && Math.random() < spawnChance) {
      lastWaveSpawnRef.current = now;
      setWaves((prev) => [...prev.slice(-8), { id: now + Math.random(), intensity: v }]);
    }
  }, [smoothVolume, isRecording]);

  useEffect(() => {
    const cleanup = setInterval(() => {
      const now = Date.now();
      setWaves((prev) => prev.filter((w) => now - w.id < 2400));
    }, 150);
    return () => clearInterval(cleanup);
  }, []);

  useEffect(() => {
    Animated.spring(buttonScale, {
      toValue: isRecording ? 1.02 : 0.96,
      useNativeDriver: true,
      friction: 8,
      tension: 40
    }).start();
  }, [isRecording, buttonScale]);

  useEffect(() => {
    Animated.timing(coreOpacity, {
      toValue: isRecording ? 1 : 0,
      duration: 260,
      useNativeDriver: true,
      easing: Easing.inOut(Easing.ease)
    }).start();
  }, [isRecording, coreOpacity]);

  const onVoiceTap = useCallback(async () => {
    if (mode !== "voice") return;
    if (disabled || isProcessingTap) return;
    setIsProcessingTap(true);
    try {
      if (!isRecording) {
        try {
          await Haptics.impactAsync(Haptics.ImpactFeedbackStyle.Light);
        } catch {
          // ignore
        }
        const started = onRecordStart ? await onRecordStart() : true;
        setIsRecording(started !== false);
        return;
      }

      setIsRecording(false);
      try {
        await Haptics.impactAsync(Haptics.ImpactFeedbackStyle.Medium);
      } catch {
        // ignore
      }
      if (!onVoiceResult) return;
      try {
        const text = onRecordEnd ? await onRecordEnd() : undefined;
        if (text) onVoiceResult(text);
      } catch {
        // ignore
      }
    } finally {
      setIsProcessingTap(false);
    }
  }, [disabled, isProcessingTap, isRecording, mode, onRecordEnd, onRecordStart, onVoiceResult]);

  const time = timeRef.current;
  const voicePresence = Math.min(1, Math.max(0, smoothVolume * 1.8));

  const renderWaves = useCallback(() => {
    return waves.map((wave) => {
      const life = (Date.now() - wave.id) / 2400;
      const age = Math.min(1, Math.max(0, life));
      const baseRadius = 64 + voicePresence * 56 + age * 10;
      const opacity = (1 - age * 0.9) * (0.32 + wave.intensity * 0.8);
      const wobbleIntensity = Math.pow(wave.intensity || 0.1, 1.2) * 18 * (1 - age * 0.25);

      const points = 72;
      let pathData = "";

      for (let i = 0; i <= points; i++) {
        const angle = (i / points) * Math.PI * 2;
        const wobble =
          Math.sin(angle * 4 + wave.id * 0.01 + time * 0.002) * wobbleIntensity +
          Math.cos(angle * 7 - wave.id * 0.008 + time * 0.0015) * wobbleIntensity * 0.55 +
          Math.sin(angle * 11 - time * 0.003) * wobbleIntensity * 0.2;

        const r = baseRadius + wobble;
        const x = 150 + Math.cos(angle) * r;
        const y = 150 + Math.sin(angle) * r;
        pathData += (i === 0 ? "M" : "L") + `${x},${y}`;
      }
      pathData += "Z";

      return <Path key={wave.id} d={pathData} fill={"none"} stroke={glow} strokeWidth={3 - age * 2.2} opacity={opacity} />;
    });
  }, [glow, time, voicePresence, waves]);

  const svgScale = size / 128;
  const canvasSize = 300;

  const ringOpacity = useMemo(() => 0.18 + voicePresence * 0.25, [voicePresence]);

  return (
    <View style={style}>
      <View style={{ width: size, height: size, alignItems: "center", justifyContent: "center" }}>
        <View style={{ position: "absolute", width: canvasSize * svgScale, height: canvasSize * svgScale }}>
          <Svg width={canvasSize * svgScale} height={canvasSize * svgScale} viewBox="0 0 300 300">
            <Defs>
              <RadialGradient id="waveGradient" cx="50%" cy="50%">
                <Stop offset="0%" stopColor={glow} stopOpacity={0} />
                <Stop offset="45%" stopColor={glow} stopOpacity={0.55} />
                <Stop offset="100%" stopColor={glow} stopOpacity={0} />
              </RadialGradient>
            </Defs>

            {renderWaves()}

            <Circle
              cx="150"
              cy="150"
              r={92}
              fill="none"
              stroke={glowDim}
              strokeWidth={0.8}
              opacity={0.14}
              strokeDasharray="1 14"
              rotation={time * 0.008}
              origin="150, 150"
            />

            <Circle
              cx="150"
              cy="150"
              r={78 + voicePresence * 5}
              fill="none"
              stroke={accent}
              strokeWidth={1.0}
              opacity={ringOpacity}
              strokeDasharray={`${40 + voicePresence * 70} 260`}
              strokeLinecap="round"
              rotation={-time * 0.018}
              origin="150, 150"
            />
          </Svg>
        </View>

        <Animated.View style={{ transform: [{ scale: buttonScale }] }}>
          <Pressable
            onPress={mode === "voice" ? onVoiceTap : onPress}
            disabled={disabled}
            testID={testID}
            style={({ pressed }) => [
              {
                width: size,
                height: size,
                borderRadius: size / 2,
                backgroundColor: mode === "send" ? highlight : theme.colors.alpha.transparent,
                borderWidth: 1,
                borderColor: mode === "send" ? highlight : isRecording ? highlight : theme.colors.alpha.borderFaint,
                alignItems: "center",
                justifyContent: "center",
                opacity: disabled ? 0.5 : pressed ? 0.92 : 1,
                shadowColor: glow,
                shadowOpacity: mode === "send" ? 0.55 : isRecording ? 0.8 : 0.2,
                shadowRadius: mode === "send" ? 20 : isRecording ? 30 : 12,
                shadowOffset: { width: 0, height: 0 }
              }
            ]}
          >
            {mode === "send" ? (
              <Ionicons name="arrow-up" size={Math.round(size * 0.34)} color="#FFFFFF" />
            ) : !isRecording ? (
              <View style={{ width: size * 0.2, height: size * 0.28, borderRadius: size, backgroundColor: glow }} />
            ) : (
              <Animated.View
                style={{
                  opacity: coreOpacity,
                  width: size * (0.22 + smoothVolume * 0.08),
                  height: size * (0.22 + smoothVolume * 0.08),
                  borderRadius: size,
                  backgroundColor: highlight,
                  shadowColor: highlight,
                  shadowOpacity: 0.85,
                  shadowRadius: 18,
                  shadowOffset: { width: 0, height: 0 }
                }}
              />
            )}
          </Pressable>
        </Animated.View>
      </View>
    </View>
  );
}
