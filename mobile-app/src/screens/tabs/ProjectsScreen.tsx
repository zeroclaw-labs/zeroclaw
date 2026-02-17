import React, { useCallback, useEffect, useMemo, useState } from "react";
import { View, ScrollView, RefreshControl } from "react-native";
import { useFocusEffect, useNavigation } from "@react-navigation/native";

import { getProjects, type Project } from "../../api/platform";
import { Screen } from "../../../ui/primitives/Screen";
import { Text } from "../../../ui/primitives/Text";
import { ProjectCard, ProjectCardSkeleton } from "../../../ui/projects/ProjectCard";
import { LiquidButton } from "../../../ui/primitives/LiquidButton";
import { useToast } from "../../state/toast";
import { theme } from "../../../ui/theme";

export function ProjectsScreen() {
  const navigation = useNavigation<any>();
  const toast = useToast();

  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [projects, setProjects] = useState<Project[]>([]);

  const load = useCallback(async () => {
    const list = await getProjects();
    setProjects(list);
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        await load();
      } catch {
        if (!cancelled) toast.show("Couldn't load projects.");
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

  const mostRecent = useMemo(() => projects[0] ?? null, [projects]);
  const rest = useMemo(() => projects.slice(1), [projects]);

  const onRefresh = useCallback(async () => {
    setRefreshing(true);
    try {
      await load();
    } catch {
      toast.show("Couldn't refresh.");
    } finally {
      setRefreshing(false);
    }
  }, [load, toast]);

  const openProject = useCallback(
    (projectId: string) => {
      navigation.push("Project", { projectId });
    },
    [navigation]
  );

  const onCreate = useCallback(() => {
    navigation.navigate("create");
  }, [navigation]);

  return (
    <Screen>
      <ScrollView
        contentContainerStyle={{ paddingBottom: 160 }}
        refreshControl={<RefreshControl refreshing={refreshing} onRefresh={onRefresh} tintColor={theme.colors.base.secondary} />}
      >
        <View style={{ paddingHorizontal: theme.spacing.lg, paddingTop: theme.spacing.xl }}>
          <Text variant="display" style={{ marginBottom: theme.spacing.xs }}>
            My Projects
          </Text>
          <Text variant="muted" style={{ marginBottom: theme.spacing.lg }}>
            A calm place to iterate, remix, and ship.
          </Text>

          <LiquidButton label="New project" onPress={onCreate} />
        </View>

        <View style={{ paddingHorizontal: theme.spacing.lg, marginTop: theme.spacing.lg }}>
          {loading ? (
            <ProjectCardSkeleton prominent />
          ) : mostRecent ? (
            <ProjectCard testID="projects-most-recent" prominent project={mostRecent} onPress={() => openProject(mostRecent.id)} />
          ) : (
            <View style={{ padding: theme.spacing.lg, borderRadius: theme.radii.lg, backgroundColor: theme.colors.surface.raised }}>
              <Text variant="body">No projects yet.</Text>
              <Text variant="muted" style={{ marginTop: theme.spacing.xs }}>
                Start by creating one.
              </Text>
            </View>
          )}
        </View>

        <View style={{ paddingHorizontal: theme.spacing.lg, marginTop: theme.spacing.lg }}>
          <View style={{ flexDirection: "row", justifyContent: "space-between", alignItems: "baseline", marginBottom: theme.spacing.sm }}>
            <Text variant="title">Recent</Text>
            <Text variant="muted">{projects.length} total</Text>
          </View>
          <View style={{ flexDirection: "row", gap: theme.spacing.sm }}>
            <View style={{ flex: 1, gap: theme.spacing.sm }}>
              {rest.filter((_, idx) => idx % 2 === 0).map((p) => (
                <ProjectCard key={p.id} project={p} onPress={() => openProject(p.id)} />
              ))}
            </View>
            <View style={{ flex: 1, gap: theme.spacing.sm }}>
              {rest.filter((_, idx) => idx % 2 === 1).map((p) => (
                <ProjectCard key={p.id} project={p} onPress={() => openProject(p.id)} />
              ))}
            </View>
          </View>
        </View>
      </ScrollView>
    </Screen>
  );
}
