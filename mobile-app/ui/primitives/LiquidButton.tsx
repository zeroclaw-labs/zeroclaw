import React, { useMemo } from "react";
import { Pressable, View } from "react-native";
import { LinearGradient } from "expo-linear-gradient";

import { theme } from "../theme";
import { Text } from "./Text";

export function LiquidButton({
  label,
  onPress,
  disabled,
  variant = "primary",
  testID
}: {
  label: string;
  onPress: () => void;
  disabled?: boolean;
  variant?: "primary" | "secondary";
  testID?: string;
}) {
  const colors = useMemo(() => {
    if (variant === "secondary") {
      return [theme.colors.alpha.buttonSecondaryTop, theme.colors.alpha.buttonSecondaryBottom] as const;
    }
    return [theme.colors.base.secondary, theme.colors.base.accent, theme.colors.base.primary] as const;
  }, [variant]);

  return (
    <Pressable
      testID={testID}
      onPress={onPress}
      disabled={disabled}
      style={({ pressed }) => [
        {
          opacity: disabled ? 0.5 : pressed ? 0.9 : 1,
          transform: [{ scale: pressed ? 0.98 : 1 }]
        }
      ]}
    >
      <View
        style={{
          borderRadius: theme.radii.xl,
          overflow: "hidden",
          borderWidth: 1,
          borderColor: variant === "secondary" ? theme.colors.stroke.subtle : theme.colors.alpha.borderFaint,
          shadowColor: theme.colors.shadow.soft,
          shadowOpacity: 0.7,
          shadowRadius: 18,
          shadowOffset: { width: 0, height: 12 }
        }}
      >
        <LinearGradient
          colors={colors as unknown as readonly [string, string, ...string[]]}
          start={{ x: 0, y: 0 }}
          end={{ x: 1, y: 1 }}
          style={{ paddingVertical: 14, paddingHorizontal: 16 }}
        >
          <Text variant="bodyMedium" style={{ textAlign: "center", color: theme.colors.base.text }}>
            {label}
          </Text>
        </LinearGradient>
      </View>
    </Pressable>
  );
}
