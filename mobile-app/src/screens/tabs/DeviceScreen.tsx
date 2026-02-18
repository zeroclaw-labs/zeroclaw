import React, { useEffect, useMemo, useRef, useState } from "react";
import { PermissionsAndroid, Platform, Pressable, ScrollView, Switch, View } from "react-native";

import { Screen } from "../../../ui/primitives/Screen";
import { Text } from "../../../ui/primitives/Text";
import { theme } from "../../../ui/theme";
import { addActivity } from "../../state/activity";
import {
  DEFAULT_DEVICE_TOOLS,
  type MobileToolCapability,
  loadDeviceToolsConfig,
  saveDeviceToolsConfig,
} from "../../state/mobileclaw";
import { runToolExecutionProbe } from "../../runtime/session";
import { executeAndroidToolAction } from "../../native/androidAgentBridge";

export function DeviceScreen() {
  const [tools, setTools] = useState<MobileToolCapability[]>(DEFAULT_DEVICE_TOOLS);
  const [saveStatus, setSaveStatus] = useState("Loading...");
  const [probeStatus, setProbeStatus] = useState("");
  const hydratedRef = useRef(false);

  const enabledCount = useMemo(() => tools.filter((t) => t.enabled).length, [tools]);
  const allEnabled = tools.length > 0 && enabledCount === tools.length;

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const loaded = await loadDeviceToolsConfig();
      if (cancelled) return;
      setTools(loaded);
      hydratedRef.current = true;
      setSaveStatus("Autosave enabled");
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!hydratedRef.current) return;
    const timer = setTimeout(() => {
      void saveDeviceToolsConfig(tools);
      setSaveStatus("Saved locally");
    }, 300);
    return () => clearTimeout(timer);
  }, [tools]);

  const permissionsForTool = (id: string): string[] => {
    const byTool: Record<string, string[]> = {
      "android_device.storage.files": [
        PermissionsAndroid.PERMISSIONS.READ_EXTERNAL_STORAGE,
        PermissionsAndroid.PERMISSIONS.WRITE_EXTERNAL_STORAGE,
        PermissionsAndroid.PERMISSIONS.READ_MEDIA_IMAGES,
        PermissionsAndroid.PERMISSIONS.READ_MEDIA_VIDEO,
        PermissionsAndroid.PERMISSIONS.READ_MEDIA_AUDIO,
      ],
      "android_device.storage.documents": [
        PermissionsAndroid.PERMISSIONS.READ_EXTERNAL_STORAGE,
        PermissionsAndroid.PERMISSIONS.WRITE_EXTERNAL_STORAGE,
        PermissionsAndroid.PERMISSIONS.READ_MEDIA_IMAGES,
        PermissionsAndroid.PERMISSIONS.READ_MEDIA_VIDEO,
        PermissionsAndroid.PERMISSIONS.READ_MEDIA_AUDIO,
      ],
      "android_device.userdata.photos": [
        PermissionsAndroid.PERMISSIONS.READ_MEDIA_IMAGES,
        PermissionsAndroid.PERMISSIONS.READ_MEDIA_VIDEO,
      ],
      "android_device.microphone.record": [PermissionsAndroid.PERMISSIONS.RECORD_AUDIO],
      "android_device.location.read": [
        PermissionsAndroid.PERMISSIONS.ACCESS_COARSE_LOCATION,
        PermissionsAndroid.PERMISSIONS.ACCESS_FINE_LOCATION,
      ],
      "android_device.location.geofence": [
        PermissionsAndroid.PERMISSIONS.ACCESS_COARSE_LOCATION,
        PermissionsAndroid.PERMISSIONS.ACCESS_FINE_LOCATION,
      ],
      "android_device.notifications.post": [PermissionsAndroid.PERMISSIONS.POST_NOTIFICATIONS],
      "android_device.notifications.read": [PermissionsAndroid.PERMISSIONS.POST_NOTIFICATIONS],
      "android_device.notifications.hook": [PermissionsAndroid.PERMISSIONS.POST_NOTIFICATIONS],
      "android_device.calls.start": [PermissionsAndroid.PERMISSIONS.CALL_PHONE],
      "android_device.calls.incoming_hook": [PermissionsAndroid.PERMISSIONS.READ_PHONE_STATE],
      "android_device.sms.send": [PermissionsAndroid.PERMISSIONS.SEND_SMS],
      "android_device.contacts.read": [PermissionsAndroid.PERMISSIONS.READ_CONTACTS],
      "android_device.calendar.read_write": [
        PermissionsAndroid.PERMISSIONS.READ_CALENDAR,
        PermissionsAndroid.PERMISSIONS.WRITE_CALENDAR,
      ],
      "android_device.bluetooth.scan": [PermissionsAndroid.PERMISSIONS.BLUETOOTH_SCAN],
      "android_device.bluetooth.connect": [PermissionsAndroid.PERMISSIONS.BLUETOOTH_CONNECT],
      "android_device.userdata.call_log": [PermissionsAndroid.PERMISSIONS.READ_CALL_LOG],
      "android_device.userdata.sms_inbox": [PermissionsAndroid.PERMISSIONS.READ_SMS],
    };
    return byTool[id] || [];
  };

  const requestPermissionsForTools = async (ids: string[]): Promise<boolean> => {
    if (Platform.OS !== "android") return true;

    const unique = Array.from(new Set(ids.flatMap((id) => permissionsForTool(id)).filter(Boolean)));
    if (unique.length === 0) return true;

    const result = await PermissionsAndroid.requestMultiple(unique as any);
    return unique.every((permission) => result[permission] === PermissionsAndroid.RESULTS.GRANTED);
  };

  const setToggle = async (id: string, enabled: boolean) => {
    if (enabled) {
      if (id === "android_device.storage.files_full_access") {
        try {
          await executeAndroidToolAction("request_all_files_access", {});
        } catch {
          setSaveStatus("Could not open all-files access settings");
        }
      }

      const granted = await requestPermissionsForTools([id]);
      if (!granted) {
        setSaveStatus("Permission denied");
        await addActivity({
          kind: "action",
          source: "device",
          title: "Permission denied",
          detail: `Could not enable ${id}`,
        });
        return;
      }
    }

    setTools((prev) => prev.map((t) => (t.id === id ? { ...t, enabled } : t)));
    const tool = tools.find((t) => t.id === id);
    await addActivity({
      kind: "action",
      source: "device",
      title: enabled ? "Capability enabled" : "Capability disabled",
      detail: tool ? `${tool.title} (${tool.id})` : id,
    });
  };

  const setAllToggles = async (enabled: boolean) => {
    if (enabled) {
      const granted = await requestPermissionsForTools(tools.map((tool) => tool.id));
      if (!granted) {
        setSaveStatus("Permission denied");
        await addActivity({
          kind: "action",
          source: "device",
          title: "Permission denied",
          detail: "Could not enable all capabilities",
        });
        return;
      }
    }

    setTools((prev) => prev.map((tool) => ({ ...tool, enabled })));
    await addActivity({
      kind: "action",
      source: "device",
      title: enabled ? "All capabilities enabled" : "All capabilities disabled",
      detail: `${tools.length} capabilities updated`,
    });
  };

  const runFileProbe = async () => {
    setProbeStatus("Running tool probe...");
    const result = await runToolExecutionProbe(
      JSON.stringify({
        type: "tool_call",
        tool: "android_device.storage.files",
        arguments: {
          scope: "user",
          path: "",
          limit: 120,
        },
      }),
    );

    const firstEvent = result.toolEvents[0];
    if (!firstEvent) {
      setProbeStatus("Probe failed: no tool event.");
      return;
    }

    const payload = (firstEvent.output || {}) as {
      entry_count?: number;
      scope?: string;
      path?: string;
    };
    const outputText = firstEvent.output ? JSON.stringify(firstEvent.output).slice(0, 320) : "no output";
    const count = typeof payload.entry_count === "number" ? payload.entry_count : 0;
    const scope = typeof payload.scope === "string" ? payload.scope : "unknown";
    const status = `${firstEvent.status}: ${firstEvent.detail} (files=${count}, scope=${scope})`;
    setProbeStatus(status);
    await addActivity({
      kind: "action",
      source: "device",
      title: "Tool probe: list files",
      detail: `${status} | ${outputText}`,
    });
  };

  return (
    <Screen>
      <ScrollView contentContainerStyle={{ paddingHorizontal: theme.spacing.lg, paddingTop: theme.spacing.xl, paddingBottom: 140, gap: theme.spacing.lg }}>
        <View>
          <Text testID="screen-device" variant="display">Device</Text>
          <Text variant="muted" style={{ marginTop: theme.spacing.xs }}>
            Hardware, sensor, camera, user-data, calls, and SMS tool controls.
          </Text>
          <Text variant="mono" style={{ marginTop: theme.spacing.sm, color: theme.colors.base.textMuted }}>
            {saveStatus}
          </Text>
          <Text variant="mono" style={{ marginTop: theme.spacing.sm, color: theme.colors.base.textMuted }}>
            {`${enabledCount}/${tools.length} enabled`}
          </Text>
        </View>

        <View style={{ padding: theme.spacing.lg, borderRadius: theme.radii.xl, backgroundColor: theme.colors.surface.raised, borderWidth: 1, borderColor: theme.colors.stroke.subtle, gap: theme.spacing.md }}>
          <Text variant="title">Runtime Tool Probe</Text>
          <Text variant="muted">Runs a synthetic agent tool_call for `android_device.storage.files`.</Text>
          <Pressable
            testID="device-run-file-probe"
            onPress={() => {
              void runFileProbe();
            }}
            style={{
              paddingVertical: 12,
              paddingHorizontal: 14,
              borderRadius: theme.radii.lg,
              borderWidth: 1,
              borderColor: theme.colors.stroke.subtle,
              backgroundColor: theme.colors.surface.panel,
            }}
          >
            <Text variant="bodyMedium">Run agent tool probe: list files</Text>
          </Pressable>
          {!!probeStatus && (
            <Text testID="device-probe-status" variant="mono" style={{ color: theme.colors.base.textMuted }}>
              {probeStatus}
            </Text>
          )}
        </View>

        <View style={{ padding: theme.spacing.lg, borderRadius: theme.radii.xl, backgroundColor: theme.colors.surface.raised, borderWidth: 1, borderColor: theme.colors.stroke.subtle, gap: theme.spacing.md }}>
          <View
            style={{
              paddingVertical: 8,
              borderBottomWidth: 1,
              borderBottomColor: theme.colors.alpha.borderFaint,
              flexDirection: "row",
              alignItems: "center",
              justifyContent: "space-between",
              gap: theme.spacing.md,
            }}
          >
            <View style={{ flex: 1 }}>
              <Text variant="bodyMedium">All capabilities</Text>
              <Text variant="muted" style={{ marginTop: 2 }}>
                Toggle every device capability at once.
              </Text>
            </View>
            <Switch
              testID="device-toggle-all"
              value={allEnabled}
              onValueChange={(value) => {
                void setAllToggles(value);
              }}
            />
          </View>

          {tools.map((tool) => (
            <View
              key={tool.id}
              style={{
                paddingVertical: 8,
                borderBottomWidth: 1,
                borderBottomColor: theme.colors.alpha.borderFaint,
                flexDirection: "row",
                alignItems: "center",
                justifyContent: "space-between",
                gap: theme.spacing.md,
              }}
            >
              <View style={{ flex: 1 }}>
                <Text variant="bodyMedium">{tool.title}</Text>
                <Text variant="muted" style={{ marginTop: 2 }}>
                  {tool.detail}
                </Text>
                <Text variant="mono" style={{ marginTop: 4, color: theme.colors.base.textMuted }}>
                  {tool.id}
                </Text>
              </View>
              <Switch
                testID={`tool-toggle-${tool.id.replace(/[^a-zA-Z0-9]/g, "-")}`}
                value={tool.enabled}
                onValueChange={(value) => {
                  void setToggle(tool.id, value);
                }}
              />
            </View>
          ))}
        </View>
      </ScrollView>
    </Screen>
  );
}
