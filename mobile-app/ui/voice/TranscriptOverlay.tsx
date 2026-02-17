import React, { useEffect, useRef } from "react";
import { Animated, Easing, View } from "react-native";
import { BlurView } from "expo-blur";

import { theme } from "../theme";
import { Text } from "../primitives/Text";
import type { VoiceState } from "../../src/hooks/useVoiceRecording";

/**
 * Translucent overlay that shows the current voice-recording / transcription
 * state.  Positioned by the parent – this component only renders the card.
 *
 * - recording:     "Listening ..."   (pulsing dot)
 * - transcribing:  "Transcribing..." (static)
 * - idle + text:   final transcript  (briefly visible)
 */
export function TranscriptOverlay({
  state,
  transcript,
  interimText
}: {
  state: VoiceState;
  transcript?: string;
  interimText?: string;
}) {
  const dotOpacity = useRef(new Animated.Value(0.4)).current;

  useEffect(() => {
    if (state !== "recording") {
      dotOpacity.setValue(0.4);
      return;
    }
    const loop = Animated.loop(
      Animated.sequence([
        Animated.timing(dotOpacity, { toValue: 1, duration: 600, useNativeDriver: true, easing: Easing.inOut(Easing.ease) }),
        Animated.timing(dotOpacity, { toValue: 0.4, duration: 600, useNativeDriver: true, easing: Easing.inOut(Easing.ease) })
      ])
    );
    loop.start();
    return () => loop.stop();
  }, [state, dotOpacity]);

  if (state === "idle" && !transcript && !interimText) return null;

  return (
    <BlurView
      intensity={24}
      tint="dark"
      style={{
        borderRadius: theme.radii.lg,
        overflow: "hidden",
        borderWidth: 1,
        borderColor: theme.colors.stroke.subtle
      }}
    >
      <View
        style={{
          flexDirection: "row",
          alignItems: "center",
          paddingHorizontal: theme.spacing.md,
          paddingVertical: theme.spacing.sm,
          gap: theme.spacing.sm,
          backgroundColor: theme.colors.surface.glass
        }}
      >
        {state === "recording" ? (
          <>
            <Animated.View
              style={{
                width: 8,
                height: 8,
                borderRadius: 4,
                backgroundColor: theme.colors.base.accent,
                opacity: dotOpacity
              }}
            />
            <Text variant="body" style={{ color: theme.colors.alpha.textSubtle, flexShrink: 1 }} numberOfLines={3}>
              {interimText || "Listening..."}
            </Text>
          </>
        ) : (
          <>
            <View
              style={{
                width: 8,
                height: 8,
                borderRadius: 4,
                backgroundColor: theme.colors.base.secondary
              }}
            />
            <Text variant="body" style={{ color: theme.colors.alpha.textSubtle }}>
              {transcript || interimText || "Transcribing..."}
            </Text>
          </>
        )}
      </View>
    </BlurView>
  );
}
