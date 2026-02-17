import React, { useMemo, useState } from "react";
import { ScrollView, Switch, View } from "react-native";

import { Screen } from "../../../ui/primitives/Screen";
import { Text } from "../../../ui/primitives/Text";
import { theme } from "../../../ui/theme";
import { addActivity } from "../../state/activity";

type ToolToggle = {
  id: string;
  title: string;
  detail: string;
  enabled: boolean;
};

const DEFAULT_TOOLS: ToolToggle[] = [
  { id: "sensors.accelerometer", title: "Accelerometer", detail: "Read movement and tilt", enabled: true },
  { id: "sensors.gyroscope", title: "Gyroscope", detail: "Read rotation deltas", enabled: true },
  { id: "sensors.magnetometer", title: "Magnetometer", detail: "Compass and field readings", enabled: true },
  { id: "sensors.gps", title: "GPS Location", detail: "Precise location and geofencing", enabled: true },
  { id: "camera.capture", title: "Camera Capture", detail: "Take photos and video", enabled: true },
  { id: "camera.scan_qr", title: "QR/Barcode Scan", detail: "Scan visual codes", enabled: true },
  { id: "microphone.record", title: "Microphone", detail: "Voice capture for STT", enabled: true },
  { id: "storage.files", title: "File Storage", detail: "Read and write local files", enabled: true },
  { id: "apps.launch", title: "Launch Installed Apps", detail: "Open external app packages", enabled: true },
  { id: "network.wifi_info", title: "Network Info", detail: "Read Wi-Fi and connectivity", enabled: true },
  { id: "telephony.calls", title: "Calls", detail: "Initiate phone calls", enabled: true },
  { id: "telephony.sms", title: "SMS", detail: "Send SMS messages", enabled: true },
  { id: "contacts.read", title: "Contacts", detail: "Read contacts and identities", enabled: true },
  { id: "calendar.read_write", title: "Calendar", detail: "Read and update events", enabled: true },
  { id: "notifications.post", title: "Notifications", detail: "Post local notifications", enabled: true },
  { id: "userdata.clipboard", title: "Clipboard", detail: "Read/write clipboard content", enabled: true },
  { id: "userdata.photos", title: "Photo Library", detail: "Read media files", enabled: true },
  { id: "userdata.call_log", title: "Call Log", detail: "Read recent calls", enabled: true },
  { id: "userdata.sms_read", title: "SMS Inbox", detail: "Read incoming SMS", enabled: true },
];

export function DeviceScreen() {
  const [tools, setTools] = useState<ToolToggle[]>(DEFAULT_TOOLS);

  const enabledCount = useMemo(() => tools.filter((t) => t.enabled).length, [tools]);

  const setToggle = async (id: string, enabled: boolean) => {
    setTools((prev) => prev.map((t) => (t.id === id ? { ...t, enabled } : t)));
    const tool = tools.find((t) => t.id === id);
    await addActivity({
      kind: "action",
      source: "device",
      title: enabled ? "Capability enabled" : "Capability disabled",
      detail: tool ? `${tool.title} (${tool.id})` : id,
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
            {`${enabledCount}/${tools.length} enabled`}
          </Text>
        </View>

        <View style={{ padding: theme.spacing.lg, borderRadius: theme.radii.xl, backgroundColor: theme.colors.surface.raised, borderWidth: 1, borderColor: theme.colors.stroke.subtle, gap: theme.spacing.md }}>
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
