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

const sdkInt = typeof Platform.Version === "number" ? Platform.Version : Number(Platform.Version || 0);
const UI_AUTOMATION_TOOL_IDS = [
  "android_device.ui.automation_enable",
  "android_device.ui.automation_status",
  "android_device.ui.tap",
  "android_device.ui.swipe",
  "android_device.ui.click_text",
  "android_device.ui.back",
  "android_device.ui.home",
  "android_device.ui.recents",
];

function unique(values: Array<string | undefined>): string[] {
  return Array.from(new Set(values.filter((value): value is string => Boolean(value))));
}

export function DeviceScreen() {
  const [tools, setTools] = useState<MobileToolCapability[]>(DEFAULT_DEVICE_TOOLS);
  const [saveStatus, setSaveStatus] = useState("Loading...");
  const [probeStatus, setProbeStatus] = useState("");
  const [accessibilityServiceEnabled, setAccessibilityServiceEnabled] = useState(false);
  const [accessibilityConnected, setAccessibilityConnected] = useState(false);
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

  const refreshUiAutomationStatus = async () => {
    try {
      const output = (await executeAndroidToolAction("ui_automation_status", {})) as {
        enabled?: boolean;
        connected?: boolean;
      } | null;
      setAccessibilityServiceEnabled(Boolean(output?.enabled));
      setAccessibilityConnected(Boolean(output?.connected));
    } catch {
      setAccessibilityServiceEnabled(false);
      setAccessibilityConnected(false);
    }
  };

  useEffect(() => {
    void refreshUiAutomationStatus();
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
    const modernMedia = sdkInt >= 33;
    const bluetoothRuntime = sdkInt >= 31;
    const notificationsRuntime = sdkInt >= 33;

    const storagePermissions = modernMedia
      ? [
          PermissionsAndroid.PERMISSIONS.READ_MEDIA_IMAGES,
          PermissionsAndroid.PERMISSIONS.READ_MEDIA_VIDEO,
          PermissionsAndroid.PERMISSIONS.READ_MEDIA_AUDIO,
        ]
      : [
          PermissionsAndroid.PERMISSIONS.READ_EXTERNAL_STORAGE,
          PermissionsAndroid.PERMISSIONS.WRITE_EXTERNAL_STORAGE,
        ];

    const byTool: Record<string, string[]> = {
      "android_device.storage.files": storagePermissions,
      "android_device.storage.documents": storagePermissions,
      "android_device.userdata.photos": [
        ...(modernMedia
          ? [PermissionsAndroid.PERMISSIONS.READ_MEDIA_IMAGES, PermissionsAndroid.PERMISSIONS.READ_MEDIA_VIDEO]
          : [PermissionsAndroid.PERMISSIONS.READ_EXTERNAL_STORAGE]),
      ],
      "android_device.microphone.record": [PermissionsAndroid.PERMISSIONS.RECORD_AUDIO],
      "android_device.camera.capture": [PermissionsAndroid.PERMISSIONS.CAMERA],
      "android_device.camera.scan_qr": [PermissionsAndroid.PERMISSIONS.CAMERA],
      "android_device.location.read": [
        PermissionsAndroid.PERMISSIONS.ACCESS_COARSE_LOCATION,
        PermissionsAndroid.PERMISSIONS.ACCESS_FINE_LOCATION,
      ],
      "android_device.location.geofence": [
        PermissionsAndroid.PERMISSIONS.ACCESS_COARSE_LOCATION,
        PermissionsAndroid.PERMISSIONS.ACCESS_FINE_LOCATION,
      ],
      "android_device.notifications.post": notificationsRuntime ? [PermissionsAndroid.PERMISSIONS.POST_NOTIFICATIONS] : [],
      "android_device.notifications.read": notificationsRuntime ? [PermissionsAndroid.PERMISSIONS.POST_NOTIFICATIONS] : [],
      "android_device.notifications.hook": notificationsRuntime ? [PermissionsAndroid.PERMISSIONS.POST_NOTIFICATIONS] : [],
      "android_device.calls.start": [PermissionsAndroid.PERMISSIONS.CALL_PHONE],
      "android_device.calls.incoming_hook": [PermissionsAndroid.PERMISSIONS.READ_PHONE_STATE],
      "android_device.sms.send": [PermissionsAndroid.PERMISSIONS.SEND_SMS],
      "android_device.sms.incoming_hook": [PermissionsAndroid.PERMISSIONS.RECEIVE_SMS],
      "android_device.contacts.read": [PermissionsAndroid.PERMISSIONS.READ_CONTACTS],
      "android_device.calendar.read_write": [
        PermissionsAndroid.PERMISSIONS.READ_CALENDAR,
        PermissionsAndroid.PERMISSIONS.WRITE_CALENDAR,
      ],
      "android_device.bluetooth.scan": bluetoothRuntime ? [PermissionsAndroid.PERMISSIONS.BLUETOOTH_SCAN] : [],
      "android_device.bluetooth.connect": bluetoothRuntime ? [PermissionsAndroid.PERMISSIONS.BLUETOOTH_CONNECT] : [],
      "android_device.userdata.call_log": [PermissionsAndroid.PERMISSIONS.READ_CALL_LOG],
      "android_device.userdata.sms_inbox": [PermissionsAndroid.PERMISSIONS.READ_SMS],
    };
    return unique(byTool[id] || []);
  };

  const requestPermissionsForTools = async (ids: string[]): Promise<boolean> => {
    if (Platform.OS !== "android") return true;

    const requestedPermissions = unique(ids.flatMap((id) => permissionsForTool(id)));
    if (requestedPermissions.length === 0) return true;

    const result = await PermissionsAndroid.requestMultiple(requestedPermissions as any);
    return requestedPermissions.every((permission) => result[permission] === PermissionsAndroid.RESULTS.GRANTED);
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
        setSaveStatus("Enabled with limited permissions");
        await addActivity({
          kind: "action",
          source: "device",
          title: "Permission denied",
          detail: `Enabled ${id}, but one or more Android permissions were denied`,
        });
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
        setSaveStatus("Enabled with limited permissions");
        await addActivity({
          kind: "action",
          source: "device",
          title: "Permission denied",
          detail: "Enabled all capabilities, but one or more Android permissions were denied",
        });
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

  const setUiAutomationBundle = async (enabled: boolean) => {
    setTools((prev) =>
      prev.map((tool) =>
        UI_AUTOMATION_TOOL_IDS.includes(tool.id)
          ? {
              ...tool,
              enabled,
            }
          : tool,
      ),
    );

    if (enabled) {
      try {
        await executeAndroidToolAction("ui_automation_enable", {});
      } catch {
        setSaveStatus("Could not open accessibility settings");
      }
      await addActivity({
        kind: "action",
        source: "device",
        title: "UI automation requested",
        detail: "Open Android Accessibility settings and enable MobileClaw accessibility service",
      });
    } else {
      await addActivity({
        kind: "action",
        source: "device",
        title: "UI automation toggled off",
        detail: "MobileClaw UI automation capabilities disabled in app policy",
      });
    }

    setTimeout(() => {
      void refreshUiAutomationStatus();
    }, 500);
  };

  const uiAutomationToggleOn = useMemo(
    () => tools.some((tool) => UI_AUTOMATION_TOOL_IDS.includes(tool.id) && tool.enabled),
    [tools],
  );

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
          <View style={{ flexDirection: "row", alignItems: "center", justifyContent: "space-between", gap: theme.spacing.md }}>
            <View style={{ flex: 1 }}>
              <Text variant="title">Accessibility Automation</Text>
              <Text variant="muted" style={{ marginTop: 4 }}>
                Required for full OS UI automation (tap/swipe/click/back/home/recents).
              </Text>
            </View>
            <Switch
              testID="device-accessibility-toggle"
              value={uiAutomationToggleOn}
              onValueChange={(value) => {
                void setUiAutomationBundle(value);
              }}
            />
          </View>

          <Text variant="mono" style={{ color: theme.colors.base.textMuted }}>
            {`service_enabled=${accessibilityServiceEnabled}, connected=${accessibilityConnected}`}
          </Text>

          <Pressable
            testID="device-open-accessibility-settings"
            onPress={() => {
              void executeAndroidToolAction("ui_automation_enable", {});
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
            <Text variant="bodyMedium">Open Accessibility Settings</Text>
          </Pressable>

          <Text variant="muted">How to enable:</Text>
          <Text variant="muted">1) Tap "Open Accessibility Settings".</Text>
          <Text variant="muted">2) Select "MobileClaw" service.</Text>
          <Text variant="muted">3) Turn on accessibility permission and confirm prompt.</Text>
          <Text variant="muted">4) Return here and verify `service_enabled=true`.</Text>
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
