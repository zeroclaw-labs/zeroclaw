import React, { createContext, useCallback, useContext, useMemo, useRef, useState } from "react";
import { View, Pressable } from "react-native";
import { BlurView } from "expo-blur";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import Animated, {
  useSharedValue,
  useAnimatedStyle,
  withSpring,
  withTiming,
  withSequence,
  Easing,
  runOnJS,
} from "react-native-reanimated";

import { theme } from "../../ui/theme";
import { Text } from "../../ui/primitives/Text";

type ToastApi = {
  show: (message: string) => void;
};

const ToastContext = createContext<ToastApi | null>(null);

// ---------------------------------------------------------------------------
// Glass condensation spring config
// ---------------------------------------------------------------------------

const ENTER_SPRING = { damping: 14, stiffness: 200, mass: 0.8 };

export function ToastProvider({ children }: { children: React.ReactNode }) {
  const insets = useSafeAreaInsets();
  const [message, setMessage] = useState<string | null>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // -- Glass condensation animation values --
  const opacity = useSharedValue(0);
  const scale = useSharedValue(0.3);
  const translateY = useSharedValue(-20);
  const blurProgress = useSharedValue(0);

  const clearMessage = useCallback(() => {
    setMessage(null);
  }, []);

  const hide = useCallback(() => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    // Glass dissolve out
    opacity.value = withTiming(0, { duration: 180, easing: Easing.in(Easing.ease) });
    scale.value = withTiming(0.85, { duration: 200, easing: Easing.in(Easing.cubic) });
    translateY.value = withTiming(-14, { duration: 200 }, () => {
      runOnJS(clearMessage)();
    });
    blurProgress.value = withTiming(0, { duration: 200 });
  }, [opacity, scale, translateY, blurProgress, clearMessage]);

  const show = useCallback(
    (msg: string) => {
      const normalized = String(msg || "").replace(/\s+/g, " ").trim();
      const compact = normalized.length > 140 ? `${normalized.slice(0, 137)}...` : normalized;
      setMessage(compact);
      if (timerRef.current) clearTimeout(timerRef.current);

      // Glass condensation entrance: expand from disc + overshoot + settle
      opacity.value = 0;
      scale.value = 0.3;
      translateY.value = -20;
      blurProgress.value = 0;

      opacity.value = withTiming(1, { duration: 160, easing: Easing.out(Easing.ease) });
      scale.value = withSpring(1, ENTER_SPRING);
      translateY.value = withSpring(8, ENTER_SPRING);
      blurProgress.value = withSequence(
        withTiming(1.2, { duration: 200 }),
        withTiming(1, { duration: 300, easing: Easing.out(Easing.ease) }),
      );

      timerRef.current = setTimeout(() => hide(), 2200);
    },
    [opacity, scale, translateY, blurProgress, hide],
  );

  const api = useMemo(() => ({ show }), [show]);

  const animatedStyle = useAnimatedStyle(() => ({
    opacity: opacity.value,
    transform: [
      { translateY: translateY.value } as const,
      { scale: scale.value } as const,
    ] as const,
  }));

  return (
    <ToastContext.Provider value={api}>
      {children}
      {message ? (
        <Animated.View
          pointerEvents="box-none"
          style={[
            {
              position: "absolute",
              left: 0,
              right: 0,
              top: Math.max(0, insets.top + 8),
            },
            animatedStyle,
          ]}
        >
          <View style={{ alignItems: "center" }}>
            <Pressable onPress={hide}>
              <BlurView
                intensity={28}
                tint="dark"
                style={{
                  borderRadius: 999,
                  overflow: "hidden",
                  backgroundColor: theme.colors.surface.raised,
                  borderWidth: 1,
                  borderColor: theme.colors.stroke.subtle,
                }}
              >
                <View style={{ paddingVertical: 10, paddingHorizontal: 14, minWidth: 180, maxWidth: 320 }}>
                  <Text variant="bodyMedium" style={{ textAlign: "center" }}>
                    {message}
                  </Text>
                </View>
              </BlurView>
            </Pressable>
          </View>
        </Animated.View>
      ) : null}
    </ToastContext.Provider>
  );
}

export function useToast(): ToastApi {
  const ctx = useContext(ToastContext);
  if (!ctx) {
    throw new Error("useToast must be used within ToastProvider");
  }
  return ctx;
}
