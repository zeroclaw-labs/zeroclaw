import React, { useEffect, useMemo, useRef, useState } from "react";
import { View, Pressable, Animated, PanResponder, type LayoutChangeEvent } from "react-native";
import { BlurView } from "expo-blur";
import { LinearGradient } from "expo-linear-gradient";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import type { BottomTabBarProps } from "@react-navigation/bottom-tabs";
import { Ionicons } from "@expo/vector-icons";

import { theme } from "../theme";

const ICONS: Record<string, keyof typeof Ionicons.glyphMap> = {
  chat: "chatbubbles",
  activity: "notifications",
  settings: "hardware-chip",
  integrations: "link",
  device: "phone-portrait",
  security: "shield-checkmark"
};

export function FloatingDock(props: BottomTabBarProps) {
  const { state, descriptors, navigation } = props;
  const insets = useSafeAreaInsets();
  const anim = useRef(new Animated.Value(1)).current;

  const dockX = useRef(new Animated.Value(0)).current;
  const dockXValue = useRef(0);
  const [dockWidth, setDockWidth] = useState(0);

  const bottom = Math.max(12, insets.bottom + 10);
  const inset = 16;
  const radius = 22;
  const dockHeight = 56;
  const chatSize = 68;

  const routes = useMemo(() => state.routes, [state.routes]);
  const isCollapsed = false;

  useEffect(() => {
    const id = dockX.addListener(({ value }) => {
      dockXValue.current = value;
    });
    return () => dockX.removeListener(id);
  }, [dockX]);

  const hiddenX = Math.max(0, dockWidth - 70);
  useEffect(() => {
    Animated.spring(dockX, {
      toValue: isCollapsed ? hiddenX : 0,
      useNativeDriver: true,
      damping: 18,
      stiffness: 180,
      mass: 1
    }).start();
  }, [dockX, hiddenX, isCollapsed]);

  const panResponder = useMemo(() => {
    return PanResponder.create({
      onMoveShouldSetPanResponder: (_evt, g) => {
        if (!isCollapsed) return false;
        return Math.abs(g.dx) > 6 && Math.abs(g.dy) < 12;
      },
      onPanResponderMove: (_evt, g) => {
        if (!isCollapsed) return;
        const next = Math.max(0, Math.min(hiddenX, hiddenX + g.dx));
        dockX.setValue(next);
      },
      onPanResponderRelease: () => {
        if (!isCollapsed) return;
        const shouldOpen = dockXValue.current < hiddenX * 0.5;
        Animated.spring(dockX, {
          toValue: shouldOpen ? 0 : hiddenX,
          useNativeDriver: true,
          damping: 18,
          stiffness: 180,
          mass: 1
        }).start();
      }
    });
  }, [dockX, hiddenX, isCollapsed]);

  const onDockLayout = (e: LayoutChangeEvent) => {
    setDockWidth(e.nativeEvent.layout.width);
  };

  return (
    <Animated.View
      style={{
        position: "absolute",
        left: inset,
        right: inset,
        bottom,
        transform: [{ translateX: dockX }, { scale: anim }],
        opacity: anim
      }}
      onLayout={onDockLayout}
      {...(isCollapsed ? panResponder.panHandlers : {})}
    >
      <LinearGradient
        colors={theme.colors.overlay.dockStroke as any}
        start={{ x: 0, y: 0 }}
        end={{ x: 1, y: 1 }}
        style={{ borderRadius: radius, padding: 1 }}
      >
        <BlurView
          intensity={28}
          tint="dark"
          style={{
            borderRadius: radius,
            backgroundColor: theme.colors.surface.dock,
            height: dockHeight
          }}
        >
          <View style={{ height: dockHeight, flexDirection: "row", alignItems: "center", justifyContent: "space-between", paddingHorizontal: 12 }}>
            {routes.map((route, index) => {
              const isFocused = state.index === index;
              const options = descriptors[route.key].options;

              const label =
                options.tabBarLabel !== undefined
                  ? options.tabBarLabel
                  : options.title !== undefined
                    ? options.title
                    : route.name;

              const onPress = () => {
                if (!isFocused) {
                  navigation.navigate(route.name);
                }
              };

              const onLongPress = () => {
                navigation.emit({ type: "tabLongPress", target: route.key });
              };

              const iconName = ICONS[route.name] ?? "ellipse";
              const isChat = route.name === "chat";

              return (
                <Pressable
                  key={route.key}
                  onPress={onPress}
                  onLongPress={onLongPress}
                  testID={`dock-tab-${route.name}`}
                  accessibilityRole="button"
                  accessibilityState={isFocused ? { selected: true } : {}}
                  accessibilityLabel={typeof label === "string" ? label : route.name}
                  style={({ pressed }) => [
                    {
                      flex: 1,
                      alignItems: "center",
                      justifyContent: "center",
                      height: dockHeight,
                      opacity: pressed ? 0.8 : 1,
                    }
                  ]}
                >
                  {isChat ? (
                    <View
                      style={{
                        width: chatSize,
                        height: chatSize,
                        borderRadius: chatSize,
                        marginTop: -14,
                        alignItems: "center",
                        justifyContent: "center",
                        backgroundColor: isFocused ? theme.colors.base.primary : theme.colors.surface.glass,
                        borderWidth: 1,
                        borderColor: isFocused ? theme.colors.base.primary : theme.colors.stroke.subtle,
                        shadowColor: theme.colors.shadow.glowViolet,
                        shadowOpacity: isFocused ? 0.85 : 0.35,
                        shadowRadius: isFocused ? 20 : 10,
                        shadowOffset: { width: 0, height: 8 },
                      }}
                    >
                      <Ionicons
                        name={iconName}
                        size={28}
                        color={isFocused ? theme.colors.base.background : theme.colors.base.text}
                      />
                    </View>
                  ) : (
                    <Ionicons name={iconName} size={22} color={isFocused ? theme.colors.base.primary : theme.colors.overlay.dockIconIdle} />
                  )}
                </Pressable>
              );
            })}
          </View>
        </BlurView>
      </LinearGradient>
    </Animated.View>
  );
}
