import React, { useEffect, useMemo } from "react";
import { View } from "react-native";
import { LinearGradient } from "expo-linear-gradient";
import Animated, {
  useSharedValue,
  useAnimatedStyle,
  withRepeat,
  withSequence,
  withTiming,
  Easing,
} from "react-native-reanimated";

import { theme } from "../theme";

// ---------------------------------------------------------------------------
// Ambient drift loop durations (ms)
// ---------------------------------------------------------------------------

const DRIFT_A = 9000;
const DRIFT_B = 11000;

export function HoloBackdrop({ subtle }: { subtle?: boolean }) {
  const opacity = subtle ? 0.35 : 0.6;

  const stops = useMemo(() => {
    const [v, p, l] = theme.colors.gradient.holographic;
    return [v, p, l] as const;
  }, []);

  // -- Ambient liquid drift: two shared values drive orb positions --
  const driftA = useSharedValue(0);
  const driftB = useSharedValue(0);

  useEffect(() => {
    // Orb A: slow horizontal drift + scale pulse
    driftA.value = withRepeat(
      withSequence(
        withTiming(1, { duration: DRIFT_A, easing: Easing.inOut(Easing.sin) }),
        withTiming(0, { duration: DRIFT_A, easing: Easing.inOut(Easing.sin) }),
      ),
      -1,
      false,
    );
    // Orb B: slower drift, offset phase
    driftB.value = withRepeat(
      withSequence(
        withTiming(1, { duration: DRIFT_B, easing: Easing.inOut(Easing.sin) }),
        withTiming(0, { duration: DRIFT_B, easing: Easing.inOut(Easing.sin) }),
      ),
      -1,
      false,
    );
  }, [driftA, driftB]);

  const orbAStyle = useAnimatedStyle(() => ({
    transform: [
      { translateX: -20 + driftA.value * 40 } as const,
      { translateY: -10 + driftA.value * 20 } as const,
      { scale: 1 + driftA.value * 0.08 } as const,
    ] as const,
  }));

  const orbBStyle = useAnimatedStyle(() => ({
    transform: [
      { translateX: 15 - driftB.value * 30 } as const,
      { translateY: 10 - driftB.value * 25 } as const,
      { scale: 1.02 - driftB.value * 0.06 } as const,
    ] as const,
  }));

  return (
    <View pointerEvents="none" style={{ position: "absolute", top: 0, right: 0, bottom: 0, left: 0 }}>
      <LinearGradient
        colors={[theme.colors.alpha.transparent, theme.colors.alpha.scrim]}
        start={{ x: 0.5, y: 0 }}
        end={{ x: 0.5, y: 1 }}
        style={{ position: "absolute", top: 0, right: 0, bottom: 0, left: 0 }}
      />

      {/* Orb A: top-right, drifts slowly */}
      <Animated.View style={[{ position: "absolute", top: -140, right: -120 }, orbAStyle]}>
        <LinearGradient
          colors={[stops[0], stops[1], stops[2]]}
          start={{ x: 0.1, y: 0.2 }}
          end={{ x: 0.9, y: 0.8 }}
          style={{
            width: 360,
            height: 360,
            borderRadius: 360,
            opacity,
          }}
        />
      </Animated.View>

      {/* Orb B: bottom-left, drifts opposite */}
      <Animated.View style={[{ position: "absolute", bottom: -160, left: -120 }, orbBStyle]}>
        <LinearGradient
          colors={[stops[2], theme.colors.alpha.transparent]}
          start={{ x: 0.2, y: 0.2 }}
          end={{ x: 0.8, y: 0.8 }}
          style={{
            width: 420,
            height: 420,
            borderRadius: 420,
            opacity: opacity * 0.9,
          }}
        />
      </Animated.View>

      <View
        style={{
          position: "absolute",
          top: 0,
          right: 0,
          bottom: 0,
          left: 0,
          backgroundColor: theme.colors.base.background,
          opacity: 0.35,
        }}
      />
    </View>
  );
}
