import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { View, Pressable, Animated } from "react-native";
import { LinearGradient } from "expo-linear-gradient";
import { BlurView } from "expo-blur";
import * as Haptics from "expo-haptics";

import { theme } from "../theme";
import { Text } from "../primitives/Text";

type Mode = "idle" | "recording" | "thinking";

export function Nucleus({
  size,
  hint,
  compact,
  disabled,
  onVoiceResult,
  onPress,
  onRecordStart,
  onRecordEnd,
  volume: externalVolume
}: {
  size: number;
  hint?: string;
  compact?: boolean;
  disabled?: boolean;
  onVoiceResult?: (text: string) => void;
  onPress?: () => void;
  /** Called when the user begins holding the button (start recording). */
  onRecordStart?: () => void;
  /** Called when the user releases the button. Should return the transcript. */
  onRecordEnd?: () => Promise<string | undefined>;
  /** Real audio metering volume 0..1. Drives bar heights when provided. */
  volume?: number;
}) {
  const [mode, setMode] = useState<Mode>("idle");

  const breathe = useRef(new Animated.Value(0)).current;
  const pulse = useRef(new Animated.Value(0)).current;
  const bars = useRef(Array.from({ length: compact ? 7 : 11 }, () => new Animated.Value(0.35))).current;

  useEffect(() => {
    const loop = Animated.loop(
      Animated.sequence([
        Animated.timing(breathe, { toValue: 1, duration: 2400, useNativeDriver: true }),
        Animated.timing(breathe, { toValue: 0, duration: 2400, useNativeDriver: true })
      ])
    );
    loop.start();
    return () => loop.stop();
  }, [breathe]);

  // Drive bar heights from real volume when provided, otherwise random.
  useEffect(() => {
    if (mode !== "recording") return;

    if (externalVolume !== undefined) {
      bars.forEach((b, i) => {
        const variation = 0.5 + Math.sin(Date.now() * 0.01 + i * 0.8) * 0.5;
        const target = 0.1 + externalVolume * 0.9 * variation;
        Animated.timing(b, { toValue: target, duration: 100, useNativeDriver: false }).start();
      });
      return;
    }

    const id = setInterval(() => {
      bars.forEach((b) => {
        Animated.timing(b, { toValue: 0.15 + Math.random() * 0.85, duration: 160, useNativeDriver: false }).start();
      });
    }, 170);
    return () => clearInterval(id);
  }, [bars, mode, externalVolume]);

  useEffect(() => {
    if (mode !== "thinking") return;
    const loop = Animated.loop(
      Animated.sequence([
        Animated.timing(pulse, { toValue: 1, duration: 520, useNativeDriver: true }),
        Animated.timing(pulse, { toValue: 0, duration: 520, useNativeDriver: true })
      ])
    );
    loop.start();
    return () => loop.stop();
  }, [mode, pulse]);

  const scale = useMemo(() => {
    const base = breathe.interpolate({ inputRange: [0, 1], outputRange: [1, 1.04] });
    if (mode === "thinking") {
      return Animated.add(base as any, pulse.interpolate({ inputRange: [0, 1], outputRange: [0, 0.02] }) as any);
    }
    return base;
  }, [breathe, mode, pulse]);

  const onLongPress = useCallback(async () => {
    if (disabled) return;
    setMode("recording");
    try {
      await Haptics.impactAsync(Haptics.ImpactFeedbackStyle.Light);
    } catch {
      // ignore
    }
    onRecordStart?.();
  }, [disabled, onRecordStart]);

  const onPressOut = useCallback(async () => {
    if (disabled) return;
    if (mode !== "recording") return;
    setMode("thinking");
    try {
      await Haptics.impactAsync(Haptics.ImpactFeedbackStyle.Medium);
    } catch {
      // ignore
    }

    try {
      const text = onRecordEnd ? await onRecordEnd() : undefined;
      if (text) onVoiceResult?.(text);
    } finally {
      setMode("idle");
    }
  }, [disabled, mode, onVoiceResult, onRecordEnd]);

  const label = hint ?? (mode === "recording" ? "Listening" : mode === "thinking" ? "Thinking" : "Hold");

  return (
    <Pressable
      onPress={onPress}
      onLongPress={onLongPress}
      onPressOut={onPressOut}
      delayLongPress={220}
      disabled={disabled}
      style={({ pressed }) => [
        {
          opacity: disabled ? 0.5 : pressed ? 0.95 : 1
        }
      ]}
    >
      <Animated.View style={{ width: size, height: size, transform: [{ scale }] }}>
        <LinearGradient
          colors={[theme.colors.base.secondary, theme.colors.base.accent, theme.colors.base.primary]}
          start={{ x: 0.1, y: 0.1 }}
          end={{ x: 0.9, y: 0.9 }}
          style={{
            flex: 1,
            borderRadius: size,
            padding: 1.2,
            shadowColor: theme.colors.shadow.glowViolet,
            shadowOpacity: 0.9,
            shadowRadius: 22,
            shadowOffset: { width: 0, height: 14 }
          }}
        >
          <BlurView
            intensity={compact ? 20 : 26}
            tint="dark"
            style={{ flex: 1, borderRadius: size, overflow: "hidden", backgroundColor: theme.colors.surface.raised }}
          >
            <View style={{ flex: 1, alignItems: "center", justifyContent: "center" }}>
              {mode === "recording" ? (
                <View style={{ flexDirection: "row", alignItems: "flex-end", gap: 4 }}>
                  {bars.map((b, idx) => (
                    <Animated.View
                      key={idx}
                      style={{
                        width: compact ? 3 : 4,
                        height: b.interpolate({ inputRange: [0, 1], outputRange: [10, compact ? 28 : 42] }),
                        borderRadius: 999,
                        backgroundColor: idx % 2 === 0 ? theme.colors.base.secondary : theme.colors.base.primary
                      }}
                    />
                  ))}
                </View>
              ) : mode === "thinking" ? (
                <View style={{ alignItems: "center" }}>
                  <Animated.View
                    style={{
                      width: compact ? 14 : 18,
                      height: compact ? 14 : 18,
                      borderRadius: 999,
                      backgroundColor: theme.colors.base.secondary,
                      opacity: pulse.interpolate({ inputRange: [0, 1], outputRange: [0.35, 0.9] })
                    }}
                  />
                </View>
              ) : (
                <View style={{ alignItems: "center" }}>
                  <View
                    style={{
                      width: compact ? 14 : 18,
                      height: compact ? 14 : 18,
                      borderRadius: 999,
                      backgroundColor: theme.colors.base.primary
                    }}
                  />
                </View>
              )}

              {compact ? null : (
                <Text variant="label" style={{ marginTop: 10, color: theme.colors.alpha.textSubtle }}>
                  {label}
                </Text>
              )}
            </View>
          </BlurView>
        </LinearGradient>
      </Animated.View>
    </Pressable>
  );
}
