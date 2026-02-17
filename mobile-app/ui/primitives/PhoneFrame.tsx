import React from "react";
import { View, type ViewStyle } from "react-native";

import { theme } from "../theme";

export function PhoneFrame({ children, style }: { children: React.ReactNode; style?: ViewStyle }) {
  const safeTop = 38;
  const safeBottom = 26;
  return (
    <View
      style={[
        {
          borderRadius: 48,
          backgroundColor: "#05060A",
          borderWidth: 1.5,
          borderColor: "rgba(255,255,255,0.22)",
          shadowColor: "#000",
          shadowOpacity: 0.7,
          shadowRadius: 28,
          shadowOffset: { width: 0, height: 20 },
          overflow: "visible",
        },
        style,
      ]}
    >
      <View
        style={{
          position: "absolute",
          top: 10,
          left: "50%",
          marginLeft: -62,
          width: 124,
          height: 26,
          borderRadius: 16,
          backgroundColor: "#05060A",
          borderWidth: 1,
          borderColor: "rgba(255,255,255,0.16)",
          zIndex: 2,
        }}
      />

      <View
        style={{
          margin: 10,
          borderRadius: 40,
          overflow: "hidden",
          backgroundColor: theme.colors.base.background,
          borderWidth: 1,
          borderColor: "rgba(255,255,255,0.14)",
          flex: 1,
        }}
      >
        <View style={{ flex: 1, paddingTop: safeTop, paddingBottom: safeBottom }}>
          {children}
        </View>

        <View
          pointerEvents="none"
          style={{
            position: "absolute",
            bottom: 10,
            left: "50%",
            marginLeft: -56,
            width: 112,
            height: 4,
            borderRadius: 4,
            backgroundColor: "rgba(255,255,255,0.20)",
          }}
        />
      </View>
    </View>
  );
}
