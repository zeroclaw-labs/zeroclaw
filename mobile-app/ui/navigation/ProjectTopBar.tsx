import React from "react";
import { View, Pressable } from "react-native";
import { useNavigation } from "@react-navigation/native";
import { Ionicons } from "@expo/vector-icons";

import { theme } from "../theme";
import { Text } from "../primitives/Text";

export function ProjectTopBar({ title, right }: { title: string; right?: React.ReactNode }) {
  const navigation = useNavigation<any>();

  return (
    <View
      style={{
        paddingTop: 10,
        paddingHorizontal: theme.spacing.lg,
        paddingBottom: 10,
        flexDirection: "row",
        alignItems: "center",
        justifyContent: "space-between"
      }}
    >
      <View style={{ width: 44, alignItems: "flex-start" }}>
        <Pressable
          onPress={() => navigation.goBack()}
          style={({ pressed }) => [
            {
              width: 40,
              height: 40,
              borderRadius: 14,
              alignItems: "center",
              justifyContent: "center",
              backgroundColor: theme.colors.surface.raised,
              borderWidth: 1,
              borderColor: theme.colors.stroke.subtle,
              opacity: pressed ? 0.75 : 1
            }
          ]}
          accessibilityRole="button"
          accessibilityLabel="Back"
        >
          <Ionicons name="chevron-back" size={20} color={theme.colors.base.text} />
        </Pressable>
      </View>

      <View pointerEvents="none" style={{ position: "absolute", left: theme.spacing.lg + 52, right: theme.spacing.lg + 52, alignItems: "center" }}>
        <Text variant="title" numberOfLines={1} style={{ textAlign: "center" }}>
          {title}
        </Text>
      </View>

      <View style={{ minWidth: 44, alignItems: "flex-end" }}>{right ?? <View style={{ width: 40, height: 40 }} />}</View>
    </View>
  );
}
