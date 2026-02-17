import React, { useMemo } from "react";
import { View, Pressable, ActionSheetIOS, Platform } from "react-native";
import { LinearGradient } from "expo-linear-gradient";
import { Ionicons } from "@expo/vector-icons";

import type { Project } from "../../src/api/platform";
import { theme } from "../theme";
import { Text } from "../primitives/Text";
import { SkeletonBlock } from "../primitives/Skeleton";
import { MiniApp } from "../miniapps/MiniApps";
import { miniAppKindForId } from "../../src/dev/demoMiniapps";
import { PhoneFrame } from "../primitives/PhoneFrame";

function isSeededDemoProjectId(projectId: string) {
  return projectId.startsWith("demo_proj_");
}

function BlankTilePreview() {
  return (
    <View style={{ flex: 1, backgroundColor: "#0B1220", alignItems: "center", justifyContent: "center", padding: 18 }}>
      <Ionicons name="image-outline" size={34} color={theme.colors.base.primary} />
      <Text variant="muted" style={{ color: "#94A3B8", marginTop: 8, textAlign: "center" }}>
        No preview yet
      </Text>
    </View>
  );
}

export function ProjectCard({
  project,
  onPress,
  onSecondaryPress,
  prominent,
  testID
}: {
  project: Project;
  onPress: () => void;
  onSecondaryPress?: () => void;
  prominent?: boolean;
  testID?: string;
}) {
  const subtitle = useMemo(() => {
    const parts = [project.template, project.theme].filter(Boolean);
    return parts.join(" · ");
  }, [project.template, project.theme]);

  const cardGradient = theme.colors.overlay.cardGradient as unknown as readonly [string, string, string];
  const miniKind = miniAppKindForId(project.id);

  return (
    <Pressable
      testID={testID}
      onPress={onPress}
      onLongPress={() => {
        if (Platform.OS !== "ios") return;
        ActionSheetIOS.showActionSheetWithOptions(
          {
            options: ["Cancel", "Edit", "Share", "Build"],
            cancelButtonIndex: 0
          },
          (buttonIndex) => {
            if (buttonIndex === 2) onSecondaryPress?.();
          }
        );
      }}
      style={({ pressed }) => [
        {
          opacity: pressed ? 0.92 : 1,
          transform: [{ scale: pressed ? 0.99 : 1 }]
        }
      ]}
    >
      <View
        style={{
          borderRadius: prominent ? theme.radii.xl : theme.radii.lg,
          overflow: "hidden",
          borderWidth: 1,
          borderColor: theme.colors.stroke.subtle,
          backgroundColor: theme.colors.surface.raised
        }}
      >
        <View style={{ height: prominent ? 220 : 98 }}>
          <LinearGradient
            colors={cardGradient as any}
            start={{ x: 0.1, y: 0.1 }}
            end={{ x: 0.9, y: 0.9 }}
            style={{ flex: 1 }}
          />
          {prominent ? (
            <View pointerEvents="none" style={{ position: "absolute", left: 12, right: 12, top: 12, bottom: 56, alignItems: "center", justifyContent: "center" }}>
              <PhoneFrame style={{ width: "74%", aspectRatio: 9 / 19.5 }}>
                {isSeededDemoProjectId(project.id) ? (
                  <MiniApp kind={miniKind} variant="fill" showTabs={false} />
                ) : (
                  <BlankTilePreview />
                )}
              </PhoneFrame>
            </View>
          ) : null}
          <View
            style={
              prominent
                ? {
                    position: "absolute",
                    left: 0,
                    right: 0,
                    bottom: 0,
                    height: 78,
                    backgroundColor: theme.colors.alpha.scrim
                  }
                : {
                    position: "absolute",
                    top: 0,
                    left: 0,
                    right: 0,
                    bottom: 0,
                    backgroundColor: theme.colors.alpha.scrim
                  }
            }
          />
          <View style={{ position: "absolute", left: 14, right: 14, bottom: 12 }}>
            <Text variant={prominent ? "title" : "bodyMedium"} numberOfLines={1}>
              {project.name}
            </Text>
            <Text variant="muted" numberOfLines={1} style={{ marginTop: 4 }}>
              {subtitle || "—"}
            </Text>
          </View>
        </View>
        {prominent ? (
          <View style={{ padding: 14, flexDirection: "row", justifyContent: "space-between", alignItems: "center" }}>
            <View>
              <Text variant="label">Latest snapshot</Text>
              <Text variant="mono" style={{ marginTop: 6, color: theme.colors.base.textMuted }}>
                {project.latest_snapshot_id ?? "-"}
              </Text>
            </View>
            <View
              style={{
                paddingVertical: 8,
                paddingHorizontal: 10,
                borderRadius: 12,
                backgroundColor: theme.colors.alpha.surfaceFaint,
                borderWidth: 1,
                borderColor: theme.colors.stroke.subtle
              }}
            >
              <Text variant="mono" style={{ color: theme.colors.base.primary }}>
                Open
              </Text>
            </View>
          </View>
        ) : null}
      </View>
    </Pressable>
  );
}

export function ProjectCardSkeleton({ prominent }: { prominent?: boolean }) {
  return (
    <SkeletonBlock
      style={{
        borderRadius: prominent ? theme.radii.xl : theme.radii.lg,
        borderWidth: 1,
        borderColor: theme.colors.stroke.subtle,
        height: prominent ? 320 : 98
      }}
    />
  );
}
