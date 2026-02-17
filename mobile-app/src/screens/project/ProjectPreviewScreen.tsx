import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { View, Pressable, ActivityIndicator } from "react-native";
import * as Linking from "expo-linking";
import { useRoute } from "@react-navigation/native";
import { WebView } from "react-native-webview";

import { getPreview, publishPreview, setProjectExecutionMode } from "../../api/platform";
import { config } from "../../config";
import { Screen } from "../../../ui/primitives/Screen";
import { Text } from "../../../ui/primitives/Text";
import { useToast } from "../../state/toast";
import { theme } from "../../../ui/theme";
import { ProjectTopBar } from "../../../ui/navigation/ProjectTopBar";
import { MiniApp } from "../../../ui/miniapps/MiniApps";
import { miniAppKindForId } from "../../dev/demoMiniapps";

function isMockPreviewProjectId(projectId: string) {
  // Only show rich mock previews for seeded demo content.
  // User-created demo projects use id prefix "demo_" and should be blank.
  return projectId.startsWith("demo_proj_") || projectId.startsWith("gal_demo_");
}

function BlankPreview() {
  return (
    <View style={{ flex: 1, backgroundColor: "#0B1220" }}>
      <View style={{ height: 56, paddingHorizontal: 16, flexDirection: "row", alignItems: "center", justifyContent: "space-between", borderBottomWidth: 1, borderBottomColor: "#1F2937" }}>
        <Text variant="bodyMedium" style={{ color: "#E5E7EB" }}>New App</Text>
        <Text variant="muted" style={{ color: "#94A3B8" }}>Preview</Text>
      </View>
      <View style={{ flex: 1, alignItems: "center", justifyContent: "center", padding: 24 }}>
        <Text variant="title" style={{ textAlign: "center", color: "#E5E7EB" }}>Empty template</Text>
        <Text variant="muted" style={{ textAlign: "center", marginTop: 10, color: "#94A3B8" }}>
          Generate your project to see a live preview.
        </Text>
      </View>
    </View>
  );
}

export function ProjectPreviewScreen() {
  const toast = useToast();
  const route = useRoute<any>();
  const projectId: string = String(route.params?.projectId);

  const [deeplink, setDeeplink] = useState<string>("");
  const [reloadKey, setReloadKey] = useState(0);
  const [quickReadyUrl, setQuickReadyUrl] = useState<string>("");
  const [activeSnapshotId, setActiveSnapshotId] = useState<string>("");
  const [liveStatus, setLiveStatus] = useState<string>("");
  const [webError, setWebError] = useState<string>("");
  const [webLoading, setWebLoading] = useState<boolean>(true);
  const [previewMode, setPreviewMode] = useState<"quick_preview" | "full_preview">("quick_preview");
  const [fullReady, setFullReady] = useState(false);
  const [lastReadyDeeplink, setLastReadyDeeplink] = useState("");
  const [isGenerating, setIsGenerating] = useState(false);
  const [fullBuildState, setFullBuildState] = useState<"locked" | "building" | "ready" | "failed">("locked");
  const [pendingFullBuildSnapshot, setPendingFullBuildSnapshot] = useState("");
  const [autoOpenFullWhenReady, setAutoOpenFullWhenReady] = useState(false);
  const [wsConnected, setWsConnected] = useState(false);
  const ws = useRef<WebSocket | null>(null);
  const refreshPending = useRef(false);
  const fullBuildSnapshotRef = useRef("");
  const hasSeenSnapshotRef = useRef(false);

  const dockPad = 96;

  const refreshPreviewMeta = useCallback(async () => {
      try {
        const res = await getPreview(projectId);
        setDeeplink(res.deeplink);
        const responseSnapshotId = String(res.snapshot_id || "").trim();
        const currentSnapshotId = String(activeSnapshotId || "").trim();
        if (responseSnapshotId && responseSnapshotId !== currentSnapshotId) {
          setQuickReadyUrl("");
          setActiveSnapshotId(responseSnapshotId);
          if (hasSeenSnapshotRef.current) {
            setFullReady(false);
            setLastReadyDeeplink("");
            setFullBuildState("locked");
            setPendingFullBuildSnapshot(responseSnapshotId);
          }
          hasSeenSnapshotRef.current = true;
        }
        const activeOrResponseSnapshotId = String(activeSnapshotId || res.snapshot_id || "").trim();
        const snapshotMatches = !activeOrResponseSnapshotId || responseSnapshotId === activeOrResponseSnapshotId;
        const ready =
          snapshotMatches &&
          res.ready === true &&
          !!String(res.release_id || "").trim() &&
          String(res.deeplink || "").startsWith("guappa-preview://");
        setFullReady(ready);
        if (ready) {
          setLastReadyDeeplink(String(res.deeplink || ""));
          setFullBuildState("ready");
        }
      } catch {
        // keep previous values
      }
  }, [activeSnapshotId, projectId]);

  const waitForPreviewReady = useCallback(
    async (snapshotId?: string): Promise<string | null> => {
      for (let i = 0; i < 45; i += 1) {
        try {
          const params = new URLSearchParams();
          params.set("mode", "quick_preview");
          if (snapshotId) params.set("snapshot_id", snapshotId);
          const statusUrl = `${config.platformUrl}/projects/${encodeURIComponent(projectId)}/preview-web-status?${params.toString()}`;
          const res = await fetch(statusUrl);
          const status = await res.json();
          const state = String(status?.state || "");
          if (state === "done") {
            const rel = String(status?.url || "").trim();
            if (rel) {
              return `${config.platformUrl}${rel}`;
            }
            const fallbackParams = new URLSearchParams();
            fallbackParams.set("mode", "quick_preview");
            if (snapshotId) fallbackParams.set("snapshot_id", snapshotId);
            return `${config.platformUrl}/projects/${encodeURIComponent(projectId)}/preview-web?${fallbackParams.toString()}`;
          }
          if (state === "failed") {
            return null;
          }
        } catch {
          // continue polling
        }
        await new Promise((resolve) => setTimeout(resolve, 900));
      }
      return null;
    },
    [projectId]
  );

  const refreshWhenReady = useCallback(
    async (snapshotId?: string) => {
      if (refreshPending.current) {
        return;
      }
      refreshPending.current = true;
      setLiveStatus("Refreshing quick preview…");
      const effectiveSnapshotId = String(snapshotId || activeSnapshotId || "").trim() || undefined;
      try {
        const params = new URLSearchParams();
        params.set("mode", "quick_preview");
        if (effectiveSnapshotId) params.set("snapshot_id", effectiveSnapshotId);
        await fetch(`${config.platformUrl}/projects/${encodeURIComponent(projectId)}/preview-web?${params.toString()}`);
      } catch {
        // best-effort trigger for web bundle build
      }
      const readyUrl = await waitForPreviewReady(effectiveSnapshotId);
      if (readyUrl) {
        setQuickReadyUrl(readyUrl);
        setWebError("");
        setReloadKey((k) => k + 1);
        setLiveStatus("Quick preview updated");
        setIsGenerating(false);
        await refreshPreviewMeta();
      } else {
        setLiveStatus("Preview is still preparing");
      }
      refreshPending.current = false;
    },
    [activeSnapshotId, refreshPreviewMeta, waitForPreviewReady]
  );

  useEffect(() => {
    let cancelled = false;
    setDeeplink("");
    setFullReady(false);
    setActiveSnapshotId("");
    setQuickReadyUrl("");
    setReloadKey((k) => k + 1);
    setWebError("");
    setLiveStatus("");
    setWebLoading(true);
    setPreviewMode("quick_preview");
    setLastReadyDeeplink("");
    setIsGenerating(false);
    setFullBuildState("locked");
    setPendingFullBuildSnapshot("");
    setAutoOpenFullWhenReady(false);
    fullBuildSnapshotRef.current = "";
    hasSeenSnapshotRef.current = false;
    (async () => {
      try {
        const res = await getPreview(projectId);
        if (cancelled) return;
        setDeeplink(res.deeplink);
        setActiveSnapshotId(String(res.snapshot_id || ""));
        hasSeenSnapshotRef.current = true;
        const ready =
          res.ready === true &&
          !!String(res.release_id || "").trim() &&
          String(res.deeplink || "").startsWith("guappa-preview://");
        setFullReady(ready);
        if (ready) {
          setLastReadyDeeplink(String(res.deeplink || ""));
          setFullBuildState("ready");
        }
      } catch {
        if (!cancelled) toast.show("No preview yet.\nGenerate first.");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [projectId, toast]);

  useEffect(() => {
    void refreshWhenReady();
  }, [refreshWhenReady]);

  useEffect(() => {
    const timer = setInterval(() => {
      void refreshPreviewMeta();
    }, 5000);
    return () => clearInterval(timer);
  }, [refreshPreviewMeta]);

  useEffect(() => {
    // Listen for live preview updates.
    const url = `${config.wsUrl}/projects/${projectId}/agent`;
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
    let reconnectAttempt = 0;
    let closedByEffect = false;

    const connect = () => {
      if (closedByEffect) return;
      const socket = new WebSocket(url);
      ws.current = socket;

      socket.onopen = () => {
        setWsConnected(true);
        reconnectAttempt = 0;
      };

      socket.onmessage = (e) => {
        try {
          const data = JSON.parse(e.data);
          if (data.type === "preview_update" || data.type === "quick_preview") {
            const nextSnapshotId = String(data.snapshot_id || "").trim();
            if (nextSnapshotId) {
              setQuickReadyUrl("");
              setActiveSnapshotId(nextSnapshotId);
              setFullReady(false);
              setLastReadyDeeplink("");
              setFullBuildState("locked");
              setPendingFullBuildSnapshot(nextSnapshotId);
              setLiveStatus("Updating quick preview… Full preview will unlock when native build is ready.");
            }
            void refreshWhenReady(nextSnapshotId || undefined);
          } else if (data.type === "stage") {
            const label = String(data.label ?? "").trim();
            setIsGenerating(true);
            if (label) setLiveStatus(label);
          } else if (data.type === "chat") {
            const text = String(data.text ?? "").trim();
            setIsGenerating(true);
            if (text) setLiveStatus(text);
          } else if (data.type === "done") {
            setIsGenerating(false);
            const nextSnapshotId = String(data.snapshot_id || "").trim();
            if (nextSnapshotId) setActiveSnapshotId(nextSnapshotId);
            setQuickReadyUrl("");
            setFullReady(false);
            setLastReadyDeeplink("");
            setFullBuildState("locked");
            setPendingFullBuildSnapshot(nextSnapshotId);
            setLiveStatus("Quick preview updated. Preparing full native preview…");
            void refreshWhenReady(nextSnapshotId || undefined);
            const summary = String(data.summary ?? "").trim();
            setLiveStatus(summary || "Generation completed");
            if (summary) toast.show(summary);
            void refreshPreviewMeta();
          } else if (data.type === "bundle_state" && String(data.state ?? "") === "done") {
            const nextSnapshotId = String(data.snapshot_id || "").trim();
            if (nextSnapshotId) setActiveSnapshotId(nextSnapshotId);
            if (nextSnapshotId) setQuickReadyUrl("");
            if (nextSnapshotId) setPendingFullBuildSnapshot(nextSnapshotId);
            void refreshWhenReady(nextSnapshotId || undefined);
            setLiveStatus("Published preview is ready");
            void refreshPreviewMeta();
          }
        } catch {
          // ignore
        }
      };

      socket.onerror = () => {
        setWsConnected(false);
      };

      socket.onclose = () => {
        setWsConnected(false);
        if (closedByEffect) return;
        reconnectAttempt += 1;
        const delayMs = Math.min(1200 * reconnectAttempt, 8000);
        setLiveStatus("Connection lost. Reconnecting…");
        reconnectTimer = setTimeout(connect, delayMs);
      };
    };

    connect();
    return () => {
      closedByEffect = true;
      if (reconnectTimer) {
        clearTimeout(reconnectTimer);
      }
      try {
        ws.current?.close();
      } catch {
        // ignore
      }
    };
  }, [projectId, refreshPreviewMeta, refreshWhenReady]);

  const releasePreviewUrl = useMemo(() => {
    if (quickReadyUrl) {
      return `${quickReadyUrl}${quickReadyUrl.includes("?") ? "&" : "?"}v=${reloadKey}`;
    }
    const base = `${config.platformUrl}/projects/${encodeURIComponent(projectId)}/preview-web`;
    const params = new URLSearchParams();
    params.set("v", String(reloadKey));
    params.set("mode", "quick_preview");
    if (activeSnapshotId) params.set("snapshot_id", activeSnapshotId);
    return `${base}?${params.toString()}`;
  }, [activeSnapshotId, projectId, quickReadyUrl, reloadKey]);

  const quickPreviewMarkerId = webError
    ? "quick-preview-error"
    : webLoading
      ? "quick-preview-not-ready"
      : "quick-preview-ready";

  const openPreviewerViaDeeplink = useCallback(async (fetchedDeeplink: string) => {
    if (!fetchedDeeplink.startsWith("guappa-preview://")) {
      toast.show("Invalid full preview link. Expected Previewer deeplink.");
      return;
    }
    const query = fetchedDeeplink.includes("?") ? fetchedDeeplink.slice(fetchedDeeplink.indexOf("?")) : "";
    const canonical = `guappa-preview://preview/${encodeURIComponent(projectId)}${query}`;
    const candidates = Array.from(new Set([fetchedDeeplink, canonical]));

    const errors: string[] = [];
    for (const candidate of candidates) {
      try {
        let supported = true;
        try {
          supported = await Linking.canOpenURL(candidate);
        } catch {
          supported = true;
        }
        if (!supported) {
          errors.push(`canOpenURL=false:${candidate}`);
        }
        await Linking.openURL(candidate);
        setLiveStatus(`Opened full preview via ${candidate}`);
        return;
      } catch (err) {
        errors.push(`${candidate} -> ${String((err as Error)?.message || err || "open-failed")}`);
      }
    }

    const detail = errors.length ? errors[errors.length - 1] : "unknown";
    setLiveStatus(`Full preview open failed: ${detail}`);
    toast.show("Couldn't open the Previewer app.");
  }, [projectId, toast]);

  const startNativeBuildForSnapshot = useCallback(async (snapshotId: string) => {
    const targetSnapshotId = String(snapshotId || "").trim();
    if (!targetSnapshotId) return;
    if (fullBuildSnapshotRef.current === targetSnapshotId && fullBuildState === "building") return;

    fullBuildSnapshotRef.current = targetSnapshotId;
    setFullBuildState("building");
    setFullReady(false);
    setLastReadyDeeplink("");
    setLiveStatus("Building full native preview…");

    try {
      await setProjectExecutionMode(projectId, "full_preview");
    } catch {
      // ignore
    }

    const appendQueryParam = (url: string, key: string, value: string) => {
      if (!value) return url;
      const marker = `${encodeURIComponent(key)}=`;
      if (url.includes(marker)) return url;
      return `${url}${url.includes("?") ? "&" : "?"}${marker}${encodeURIComponent(value)}`;
    };

    try {
      const published = await publishPreview(projectId);
      const publishedSnapshotId = String(published?.snapshot_id || "").trim();
      if (publishedSnapshotId && publishedSnapshotId !== targetSnapshotId) {
        setFullBuildState("locked");
        return;
      }
      let nextDeeplink = String(published?.deeplink || "").trim();
      const publishedChannel = String(published?.channel || "").trim();
      const publishedRelease = String(published?.release_id || "").trim();
      if (nextDeeplink) {
        nextDeeplink = appendQueryParam(nextDeeplink, "channel", publishedChannel);
        nextDeeplink = appendQueryParam(nextDeeplink, "release", publishedRelease);
      }
      if (!nextDeeplink) {
        setFullBuildState("failed");
        setLiveStatus("Native preview build failed");
        return;
      }
      setDeeplink(nextDeeplink);
      setLastReadyDeeplink(nextDeeplink);
      setFullReady(true);
      setFullBuildState("ready");
      setLiveStatus("Full preview is ready");
      if (autoOpenFullWhenReady) {
        setAutoOpenFullWhenReady(false);
        await openPreviewerViaDeeplink(nextDeeplink);
      }
    } catch {
      setFullBuildState("failed");
      setLiveStatus("Native preview build failed");
    }
  }, [autoOpenFullWhenReady, fullBuildState, openPreviewerViaDeeplink, projectId]);

  const openPreviewer = useCallback(async () => {
    const stableDeeplink = (lastReadyDeeplink || deeplink || "").trim();
    setPreviewMode("full_preview");
    if (fullReady && stableDeeplink) {
      await openPreviewerViaDeeplink(stableDeeplink);
      return;
    }
    if (fullBuildState === "building") {
      setAutoOpenFullWhenReady(true);
      setLiveStatus("Native build in progress. Opening automatically when ready…");
      return;
    }
    const snapshotForBuild = String(activeSnapshotId || "").trim();
    if (!snapshotForBuild) {
      toast.show("Full preview is locked while native build is preparing.");
      return;
    }
    setAutoOpenFullWhenReady(true);
    void startNativeBuildForSnapshot(snapshotForBuild);
  }, [activeSnapshotId, deeplink, fullBuildState, fullReady, lastReadyDeeplink, openPreviewerViaDeeplink, startNativeBuildForSnapshot, toast]);

  useEffect(() => {
    const pendingSnapshot = String(pendingFullBuildSnapshot || "").trim();
    const currentSnapshot = String(activeSnapshotId || "").trim();
    if (!pendingSnapshot || !currentSnapshot || pendingSnapshot !== currentSnapshot) return;
    if (webLoading || !!webError) return;
    setPendingFullBuildSnapshot("");
    void startNativeBuildForSnapshot(currentSnapshot);
  }, [activeSnapshotId, pendingFullBuildSnapshot, startNativeBuildForSnapshot, webError, webLoading]);

  return (
    <Screen>
      <View style={{ flex: 1 }}>
        <ProjectTopBar
          title="Preview"
          right={
            <View
              style={{
                flexDirection: "row",
                borderRadius: 14,
                overflow: "hidden",
                borderWidth: 1,
                borderColor: theme.colors.stroke.subtle,
                backgroundColor: theme.colors.surface.raised
              }}
            >
              <Pressable
                testID="preview-mode-quick"
                onPress={async () => {
                  setPreviewMode("quick_preview");
                  try {
                    await setProjectExecutionMode(projectId, "quick_preview");
                  } catch {
                    // ignore
                  }
                  void refreshWhenReady();
                }}
                style={({ pressed }) => [{ opacity: pressed ? 0.8 : 1, paddingVertical: 8, paddingHorizontal: 10, backgroundColor: previewMode === "quick_preview" ? theme.colors.alpha.userBubbleBg : "transparent" }]}
              >
                <Text variant="mono" style={previewMode === "quick_preview" ? { color: theme.colors.base.primary } : undefined}>
                  Quick
                </Text>
              </Pressable>
              <Pressable
                testID="preview-mode-full"
                onPress={openPreviewer}
                disabled={fullBuildState === "locked" || fullBuildState === "failed"}
                style={({ pressed }) => [
                  {
                    opacity: (fullBuildState === "locked" || fullBuildState === "failed") ? 0.45 : pressed ? 0.8 : 1,
                    paddingVertical: 8,
                    paddingHorizontal: 10,
                    backgroundColor: previewMode === "full_preview" ? theme.colors.alpha.userBubbleBg : "transparent",
                    flexDirection: "row",
                    alignItems: "center",
                    gap: 6,
                  }
                ]}
                >
                {fullBuildState === "building" ? (
                  <ActivityIndicator size="small" color={theme.colors.base.primary} />
                ) : null}
                <Text variant="mono" style={previewMode === "full_preview" ? { color: theme.colors.base.primary } : undefined}>Full</Text>
              </Pressable>
            </View>
          }
        />

        <View style={{ flex: 1, paddingBottom: dockPad }}>
          {!!liveStatus && (
            <View
              style={{
                marginHorizontal: theme.spacing.md,
                marginTop: theme.spacing.sm,
                marginBottom: theme.spacing.sm,
                borderWidth: 1,
                borderColor: theme.colors.stroke.subtle,
                borderRadius: 10,
                paddingHorizontal: 10,
                paddingVertical: 8,
                backgroundColor: theme.colors.surface.raised,
                flexDirection: "row",
                alignItems: "center",
                gap: 8,
              }}
            >
              {(fullBuildState === "building" || !wsConnected) ? (
                <ActivityIndicator size="small" color={theme.colors.base.primary} />
              ) : null}
              <Text variant="bodyMedium" style={{ flex: 1 }}>{liveStatus}</Text>
            </View>
          )}

          {isMockPreviewProjectId(projectId) ? (
            <MiniApp kind={miniAppKindForId(projectId)} variant="fill" />
          ) : (
            <View style={{ flex: 1, borderRadius: 24, overflow: "hidden", borderWidth: 1, borderColor: theme.colors.stroke.subtle, backgroundColor: theme.colors.base.background }}>
              <WebView
                key={`${projectId}:${activeSnapshotId || "none"}:${reloadKey}`}
                source={{ uri: releasePreviewUrl }}
                style={{ flex: 1, backgroundColor: "transparent" }}
                testID="project-preview-webview"
                originWhitelist={["*"]}
                cacheEnabled={false}
                incognito
                scrollEnabled
                nestedScrollEnabled
                bounces
                showsVerticalScrollIndicator={false}
                onLoadStart={() => {
                  setWebLoading(true);
                  setWebError("");
                }}
                onLoadEnd={() => {
                  setWebLoading(false);
                }}
                onHttpError={(e) => {
                  const status = e.nativeEvent.statusCode;
                  const message = `Preview returned HTTP ${status}`;
                  setWebError(message);
                  setLiveStatus(message);
                  setWebLoading(false);
                }}
                onError={() => {
                  const message = "Quick preview failed to load";
                  setWebError(message);
                  setLiveStatus(message);
                  setWebLoading(false);
                }}
              />
              {!!webError && (
                <View
                  style={{
                    position: "absolute",
                    left: theme.spacing.lg,
                    right: theme.spacing.lg,
                    top: theme.spacing.lg,
                    borderWidth: 1,
                    borderColor: theme.colors.stroke.subtle,
                    borderRadius: 12,
                    padding: 12,
                    backgroundColor: theme.colors.surface.raised,
                    gap: 10
                  }}
                >
                  <Text variant="bodyMedium">{webError}</Text>
                  <Pressable
                    onPress={() => {
                      setLiveStatus("Retrying quick preview…");
                      setWebError("");
                      setReloadKey((k) => k + 1);
                      void refreshWhenReady();
                    }}
                    style={({ pressed }) => [{ opacity: pressed ? 0.8 : 1 }]}
                  >
                    <Text variant="mono" style={{ color: theme.colors.base.primary }}>Retry quick preview</Text>
                  </Pressable>
                </View>
              )}

            </View>
          )}

          <View
            testID={fullBuildState === "building" ? "preview-mode-full-building" : fullReady ? "preview-mode-full-ready" : "preview-mode-full-not-ready"}
            accessible={false}
            pointerEvents="none"
            style={{
              position: "absolute",
              right: 2,
              top: 2,
              width: 2,
              height: 2,
              borderRadius: 1,
              backgroundColor:
                fullBuildState === "building"
                  ? theme.colors.alpha.userBubbleBg
                  : fullReady
                    ? theme.colors.base.primary
                    : theme.colors.surface.raised
            }}
          />

          <View
            testID={quickPreviewMarkerId}
            accessible={false}
            pointerEvents="none"
            style={{
              position: "absolute",
              right: 6,
              top: 6,
              width: 2,
              height: 2,
              borderRadius: 1,
              backgroundColor: webError
                ? theme.colors.base.secondary
                : webLoading
                  ? theme.colors.stroke.subtle
                  : theme.colors.base.primary
            }}
          />

        </View>
      </View>
    </Screen>
  );
}
