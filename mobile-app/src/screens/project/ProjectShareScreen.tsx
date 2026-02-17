import React, { useCallback, useEffect, useState } from "react";
import { View, Share as NativeShare } from "react-native";
import { useNavigation, useRoute } from "@react-navigation/native";
import QRCode from "react-native-qrcode-svg";

import { getGallery, getPreview, publishProject } from "../../api/platform";
import { Screen } from "../../../ui/primitives/Screen";
import { Text } from "../../../ui/primitives/Text";
import { LiquidButton } from "../../../ui/primitives/LiquidButton";
import { useToast } from "../../state/toast";
import { addActivity } from "../../state/activity";
import { theme } from "../../../ui/theme";
import { ProjectTopBar } from "../../../ui/navigation/ProjectTopBar";

export function ProjectShareScreen() {
  const toast = useToast();
  const navigation = useNavigation<any>();
  const route = useRoute<any>();
  const projectId: string = String(route.params?.projectId);
  const dockPad = 96;

  const [deeplink, setDeeplink] = useState<string>("");

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const res = await getPreview(projectId);
        if (!cancelled) setDeeplink(res.deeplink);
      } catch {
        // ok
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [projectId]);

  const onShare = useCallback(async () => {
    try {
      await NativeShare.share({ message: deeplink || `Project ${projectId}` });
    } catch {
      toast.show("Couldn't open share sheet.");
    }
  }, [deeplink, projectId, toast]);

  const onPublish = useCallback(async () => {
    try {
      await publishProject(projectId);
      const items = await getGallery();
      const present = items.some((item) => item.project_id === projectId);
      if (present) {
        toast.show("Published to Gallery.");
        await addActivity({ title: "Published to gallery", detail: projectId });
        navigation.navigate("Tabs", { screen: "gallery" });
      } else {
        toast.show("Published, but gallery refresh is delayed.");
        await addActivity({ title: "Publish queued", detail: projectId });
      }
    } catch {
      toast.show("Couldn't publish.");
    }
  }, [navigation, projectId, toast]);

  return (
    <Screen>
      <View style={{ flex: 1 }}>
        <ProjectTopBar title="Share" />

        <View style={{ flex: 1, paddingHorizontal: theme.spacing.lg, paddingBottom: dockPad, gap: theme.spacing.lg }}>

          <View style={{ padding: theme.spacing.lg, borderRadius: theme.radii.xl, backgroundColor: theme.colors.surface.raised, borderWidth: 1, borderColor: theme.colors.stroke.subtle }}>
          <Text variant="title">Deep Link</Text>
          <Text variant="mono" style={{ marginTop: theme.spacing.sm, color: theme.colors.base.textMuted }}>
            {deeplink || "Generate a preview to get a share link."}
          </Text>

          {deeplink ? (
            <View style={{ marginTop: theme.spacing.lg, alignItems: "center" }}>
              <View
                style={{
                  padding: theme.spacing.md,
                  borderRadius: theme.radii.lg,
                  backgroundColor: theme.colors.base.text,
                  borderWidth: 1,
                  borderColor: theme.colors.stroke.subtle
                }}
              >
                <QRCode value={deeplink} size={180} backgroundColor={theme.colors.base.text} color={theme.colors.base.background} />
              </View>
              <Text variant="muted" style={{ marginTop: theme.spacing.sm }}>
                Scan to open in Preview Companion
              </Text>
            </View>
          ) : null}

          <View style={{ marginTop: theme.spacing.md, gap: theme.spacing.sm }}>
            <LiquidButton testID="share-project-button" label="Share" onPress={onShare} disabled={!deeplink} />
            <LiquidButton testID="publish-to-gallery-button" label="Publish to Gallery" onPress={onPublish} variant="secondary" />
          </View>
          </View>
        </View>
      </View>
    </Screen>
  );
}
