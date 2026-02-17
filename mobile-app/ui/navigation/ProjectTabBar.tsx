import React, { useMemo } from "react";
import { View, Pressable } from "react-native";
import { BlurView } from "expo-blur";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import type { BottomTabBarProps } from "@react-navigation/bottom-tabs";
import { Ionicons } from "@expo/vector-icons";

import { theme } from "../theme";

const ICONS: Record<string, keyof typeof Ionicons.glyphMap> = {
  chat: "chatbubble-ellipses",
  preview: "play",
  share: "share",
  settings: "options"
};

export function ProjectTabBar({ state, descriptors, navigation }: BottomTabBarProps) {
  const insets = useSafeAreaInsets();
  const bottom = Math.max(12, insets.bottom + 10);
  const dockHeight = 56;

  const routes = useMemo(() => state.routes, [state.routes]);

  return (
    <View style={{ position: "absolute", left: 16, right: 16, bottom }}>
      <BlurView
        intensity={26}
        tint="dark"
        style={{
          borderRadius: 22,
          overflow: "hidden",
          backgroundColor: theme.colors.surface.dock,
          borderWidth: 1,
          borderColor: theme.colors.stroke.subtle,
          height: dockHeight
        }}
      >
        <View style={{ height: dockHeight, flexDirection: "row", justifyContent: "space-between", paddingHorizontal: 12 }}>
          {routes.map((route, index) => {
            const isFocused = state.index === index;
            const options = descriptors[route.key].options;
            const label = options.title ?? route.name;

            const onPress = () => {
              const event = navigation.emit({ type: "tabPress", target: route.key, canPreventDefault: true });
              if (!isFocused && !event.defaultPrevented) {
                navigation.navigate(route.name);
              }
            };

            const iconName = ICONS[route.name] ?? "ellipse";
            return (
              <Pressable
                key={route.key}
                onPress={onPress}
                testID={`project-tab-${route.name}`}
                style={({ pressed }) => [
                  {
                    flex: 1,
                    alignItems: "center",
                    justifyContent: "center",
                    height: dockHeight,
                    opacity: pressed ? 0.75 : 1
                  }
                ]}
                accessibilityRole="button"
                accessibilityLabel={typeof label === "string" ? label : route.name}
              >
                <Ionicons name={iconName} size={20} color={isFocused ? theme.colors.base.primary : theme.colors.overlay.dockIconIdle} />
              </Pressable>
            );
          })}
        </View>
      </BlurView>
    </View>
  );
}
