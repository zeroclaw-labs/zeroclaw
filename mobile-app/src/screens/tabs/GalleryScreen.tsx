import React, { useCallback, useEffect, useState } from "react";
import { View, FlatList, useWindowDimensions, Pressable } from "react-native";
import { useFocusEffect, useNavigation } from "@react-navigation/native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { WebView } from "react-native-webview";
import { Ionicons } from "@expo/vector-icons";

import { forkProject, getGallery, type GalleryItem } from "../../api/platform";
import { config } from "../../config";
import { Text } from "../../../ui/primitives/Text";
import { useToast } from "../../state/toast";
import { theme } from "../../../ui/theme";

export function GalleryScreen() {
  const navigation = useNavigation<any>();
  const insets = useSafeAreaInsets();
  const { width: screenW, height: screenH } = useWindowDimensions();
  const toast = useToast();
  const [items, setItems] = useState<GalleryItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [hintShown, setHintShown] = useState(false);

  const releasePreviewUrl = useCallback((projectId: string, releaseId?: string | null) => {
    const base = `${config.platformUrl}/projects/${encodeURIComponent(projectId)}/preview-web`;
    const params = new URLSearchParams();
    params.set("mode", "gallery_preview");
    if (releaseId) params.set("release_id", releaseId);
    return `${base}?${params.toString()}`;
  }, []);

  const load = useCallback(async () => {
    const list = await getGallery();
    setItems(list);
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        await load();
      } catch {
        if (!cancelled) toast.show("Couldn't load the gallery.");
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [load, toast]);

  useFocusEffect(
    useCallback(() => {
      void load();
    }, [load])
  );

  useEffect(() => {
    if (hintShown) return;
    const id = setTimeout(() => {
      toast.show("Swipe left / right to browse");
      setHintShown(true);
    }, 350);
    return () => clearTimeout(id);
  }, [hintShown, toast]);

  const onFork = useCallback(
    async (projectId: string) => {
      try {
        const res = await forkProject(projectId);
        toast.show("Forked.\nMake it yours.");
        navigation.push("Project", { projectId: res.project_id });
      } catch {
        toast.show("Couldn't fork this project.");
      }
    },
    [navigation, toast]
  );

  return (
    <View style={{ flex: 1, backgroundColor: theme.colors.base.background }}>
      <FlatList
        data={items}
        keyExtractor={(it) => it.project_id}
        horizontal
        pagingEnabled
        showsHorizontalScrollIndicator={false}
        bounces={false}
        ListEmptyComponent={() => {
          if (loading) return null;
          return (
            <View style={{ width: screenW, height: screenH, alignItems: "center", justifyContent: "center", padding: theme.spacing.lg }}>
              <Text variant="title" style={{ textAlign: "center" }}>
                No gallery items yet
              </Text>
              <Text variant="muted" style={{ textAlign: "center", marginTop: theme.spacing.sm, maxWidth: 320 }}>
                Turn on demo mode or publish a project to Gallery.
              </Text>
            </View>
          );
        }}
        renderItem={({ item }) => {
          return (
            <View style={{ width: screenW, height: screenH }}>
              <View style={{ flex: 1, paddingTop: insets.top + 8, paddingBottom: insets.bottom, paddingHorizontal: 0 }}>
                <View style={{ flexDirection: "row", alignItems: "center", justifyContent: "space-between", gap: 12, paddingHorizontal: 12, marginBottom: 8 }}>
                  <Pressable
                    onPress={() => navigation.navigate("projects")}
                    style={({ pressed }) => [{ width: 36, height: 36, borderRadius: 18, alignItems: "center", justifyContent: "center", borderWidth: 1, borderColor: theme.colors.stroke.subtle, backgroundColor: theme.colors.surface.raised, opacity: pressed ? 0.8 : 1 }]}
                    accessibilityRole="button"
                    accessibilityLabel="Back"
                  >
                    <Ionicons name="chevron-back" size={20} color={theme.colors.base.text} />
                  </Pressable>

                  <View style={{ flex: 1 }}>
                    <Text variant="title" numberOfLines={1}>{item.name}</Text>
                    {loading ? <Text variant="muted">Loading...</Text> : null}
                  </View>

                  <Pressable
                    onPress={() => onFork(item.project_id)}
                    style={({ pressed }) => [
                      {
                        paddingVertical: 10,
                        paddingHorizontal: 14,
                        borderRadius: 14,
                        backgroundColor: theme.colors.surface.raised,
                        borderWidth: 1,
                        borderColor: theme.colors.stroke.subtle,
                        opacity: pressed ? 0.8 : 1
                      }
                    ]}
                    accessibilityRole="button"
                    accessibilityLabel="Fork"
                  >
                    <Text variant="mono" style={{ color: theme.colors.base.primary }}>Fork</Text>
                  </Pressable>
                </View>

                <View style={{ flex: 1 }}>
                  <WebView
                    source={{ uri: releasePreviewUrl(item.project_id, item.release_id) }}
                    originWhitelist={["*"]}
                    style={{ flex: 1, backgroundColor: "transparent" }}
                    cacheEnabled={false}
                    incognito
                    scrollEnabled
                    nestedScrollEnabled
                    bounces
                    testID="gallery-active-webview"
                  />
                </View>
              </View>
            </View>
          );
        }}
      />
    </View>
  );
}
