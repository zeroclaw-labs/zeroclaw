import React, { useEffect, useMemo, useRef } from "react";
import { View, Animated, type ViewStyle } from "react-native";
import { LinearGradient } from "expo-linear-gradient";

import { theme } from "../theme";

export function SkeletonBlock({ style }: { style?: ViewStyle }) {
  const shimmer = useRef(new Animated.Value(0)).current;

  useEffect(() => {
    const loop = Animated.loop(
      Animated.timing(shimmer, {
        toValue: 1,
        duration: 1100,
        useNativeDriver: true
      })
    );
    loop.start();
    return () => loop.stop();
  }, [shimmer]);

  const colors = useMemo(
    () => [theme.colors.alpha.surfaceFaint, theme.colors.alpha.borderFaint, theme.colors.alpha.surfaceFaint] as const,
    []
  );

  return (
    <View
      style={[
        {
          backgroundColor: theme.colors.surface.raised,
          overflow: "hidden"
        },
        style
      ]}
    >
      <Animated.View
        style={{
          position: "absolute",
          top: 0,
          bottom: 0,
          width: "60%",
          transform: [
            {
              translateX: shimmer.interpolate({
                inputRange: [0, 1],
                outputRange: [-220, 420]
              })
            },
            { rotateZ: "12deg" }
          ]
        }}
      >
        <LinearGradient
          colors={colors as unknown as [string, string, ...string[]]}
          start={{ x: 0, y: 0 }}
          end={{ x: 1, y: 0 }}
          style={{ flex: 1 }}
        />
      </Animated.View>
    </View>
  );
}
