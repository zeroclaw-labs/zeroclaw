import React from "react";
import { View, type ViewProps } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";

import { theme } from "../theme";
import { HoloBackdrop } from "./HoloBackdrop";

export function Screen({ children, style, ...props }: ViewProps) {
  return (
    <SafeAreaView style={{ flex: 1, backgroundColor: theme.colors.base.background }}>
      <View style={{ flex: 1 }}>
        <HoloBackdrop subtle />
        <View {...props} style={[{ flex: 1 }, style]}>
          {children}
        </View>
      </View>
    </SafeAreaView>
  );
}
