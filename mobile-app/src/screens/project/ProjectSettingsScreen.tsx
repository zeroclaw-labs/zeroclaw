import React, { useEffect, useState } from "react";
import { Linking, ScrollView, View } from "react-native";
import { useRoute } from "@react-navigation/native";

import {
  exportAppSourceUrl,
  exportRuntimeSourceUrl,
  getProject,
  setProjectSubscriptionTier,
  type Project
} from "../../api/platform";
import { Screen } from "../../../ui/primitives/Screen";
import { Text } from "../../../ui/primitives/Text";
import { theme } from "../../../ui/theme";
import { useToast } from "../../state/toast";
import { ProjectTopBar } from "../../../ui/navigation/ProjectTopBar";
import { LiquidButton } from "../../../ui/primitives/LiquidButton";

export function ProjectSettingsScreen() {
  const toast = useToast();
  const route = useRoute<any>();
  const projectId: string = String(route.params?.projectId);
  const dockPad = 96;

  const [project, setProject] = useState<Project | null>(null);
  const [updatingTier, setUpdatingTier] = useState<"free" | "starter" | "premium" | null>(null);

  const loadProject = async () => {
    const p = await getProject(projectId);
    setProject(p);
  };

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const p = await getProject(projectId);
        if (!cancelled) {
          setProject(p);
        }
      } catch {
        if (!cancelled) toast.show("Couldn't load project.");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [projectId, toast]);

  const openExport = async (kind: "app" | "runtime") => {
    const url = kind === "app" ? exportAppSourceUrl(projectId) : exportRuntimeSourceUrl(projectId);
    try {
      await Linking.openURL(url);
    } catch {
      toast.show("Couldn't start export download.");
    }
  };

  const updateTier = async (tier: "free" | "starter" | "premium") => {
    try {
      setUpdatingTier(tier);
      await setProjectSubscriptionTier(projectId, tier);
      await loadProject();
      toast.show(`Plan set to ${tier}.`);
    } catch {
      toast.show("Couldn't update subscription tier.");
    } finally {
      setUpdatingTier(null);
    }
  };

  const currentTier = project?.subscription_tier ?? "free";

  return (
    <Screen>
      <View style={{ flex: 1 }}>
        <ProjectTopBar title="Settings" />

        <ScrollView
          style={{ flex: 1 }}
          contentContainerStyle={{ paddingHorizontal: theme.spacing.lg, paddingBottom: dockPad, gap: theme.spacing.lg }}
          showsVerticalScrollIndicator={false}
        >
          <View style={{ padding: theme.spacing.lg, borderRadius: theme.radii.xl, backgroundColor: theme.colors.surface.raised, borderWidth: 1, borderColor: theme.colors.stroke.subtle, gap: 10 }}>
            <Text variant="title">Project</Text>
            <Row label="ID" value={project?.id ?? projectId} />
            <Row label="Name" value={project?.name ?? "-"} />
            <Row label="Visibility" value={project?.visibility ?? "-"} />
            <Row label="Template" value={project?.template ?? "-"} />
            <Row label="Theme" value={project?.theme ?? "-"} />
            <Row label="Latest snapshot" value={project?.latest_snapshot_id ?? "-"} mono />
            <Row label="Runtime" value={project?.runtime_status ?? "not_provisioned"} />
            <Row label="Runtime mode" value={project?.runtime_mode ?? "local"} />
            <Row label="Deployment" value={project?.runtime_deployment ?? "shared"} />
            <Row label="Project ref" value={project?.runtime_project_ref ?? "-"} mono />
            <Row label="Supabase URL" value={project?.runtime_supabase_url ?? "-"} mono />
            <Row label="Subscription" value={currentTier} />
            <Row label="Source export" value={project?.export_source_enabled ? "enabled" : "paid plan required"} />
            <Row label="Agent input tokens" value={String(project?.agent_total_input_tokens ?? 0)} mono />
            <Row label="Agent output tokens" value={String(project?.agent_total_output_tokens ?? 0)} mono />
            <Row label="Agent total cost" value={`$${(project?.agent_total_cost_usd ?? 0).toFixed(4)}`} mono />
          </View>

          <View style={{ padding: theme.spacing.lg, borderRadius: theme.radii.xl, backgroundColor: theme.colors.surface.raised, borderWidth: 1, borderColor: theme.colors.stroke.subtle, gap: 10 }}>
            <Text variant="title">Subscription Tier</Text>
            <LiquidButton
              label={currentTier === "free" ? "Free (current)" : "Switch to Free"}
              onPress={() => updateTier("free")}
              disabled={updatingTier !== null}
              variant={currentTier === "free" ? "primary" : "secondary"}
            />
            <LiquidButton
              label={currentTier === "starter" ? "Starter (current)" : "Switch to Starter"}
              onPress={() => updateTier("starter")}
              disabled={updatingTier !== null}
              variant={currentTier === "starter" ? "primary" : "secondary"}
            />
            <LiquidButton
              label={currentTier === "premium" ? "Premium (current)" : "Switch to Premium"}
              onPress={() => updateTier("premium")}
              disabled={updatingTier !== null}
              variant={currentTier === "premium" ? "primary" : "secondary"}
            />
          </View>

          <View style={{ padding: theme.spacing.lg, borderRadius: theme.radii.xl, backgroundColor: theme.colors.surface.raised, borderWidth: 1, borderColor: theme.colors.stroke.subtle, gap: 10 }}>
            <Text variant="title">Downloads</Text>
            <LiquidButton label="Download app source" onPress={() => openExport("app")} />
            <LiquidButton label="Download runtime backend" onPress={() => openExport("runtime")} variant="secondary" />
          </View>
        </ScrollView>
      </View>
    </Screen>
  );
}

function Row({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <View style={{ flexDirection: "row", justifyContent: "space-between", gap: 10 }}>
      <Text variant="muted">{label}</Text>
      <Text variant={mono ? "mono" : "body"} style={{ color: theme.colors.base.text }}>
        {value}
      </Text>
    </View>
  );
}
