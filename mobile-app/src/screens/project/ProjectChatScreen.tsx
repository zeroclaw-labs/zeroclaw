import React, { useCallback, useEffect, useMemo, useState, useRef } from "react";
import { View, ScrollView, TextInput, TouchableOpacity } from "react-native";
import { useFocusEffect, useRoute } from "@react-navigation/native";
import Animated, { FadeIn, SlideInRight, SlideInLeft } from "react-native-reanimated";
import { Ionicons } from "@expo/vector-icons";
import * as ImagePicker from "expo-image-picker";

import { getProject, uploadProjectFile } from "../../api/platform";
import { config } from "../../config";
import { Screen } from "../../../ui/primitives/Screen";
import { Text } from "../../../ui/primitives/Text";
import { theme } from "../../../ui/theme";
import { VoiceRecordButton } from "../../../ui/voice/VoiceRecordButton";
import { TranscriptOverlay } from "../../../ui/voice/TranscriptOverlay";
import { useVoiceRecording } from "../../hooks/useVoiceRecording";
import { useToast } from "../../state/toast";
import { addActivity } from "../../state/activity";
import { loadChat, appendChat, type ChatMessage } from "../../state/chat";
import { ProjectTopBar } from "../../../ui/navigation/ProjectTopBar";
import { popPendingChatDraft } from "../../state/pendingDraft";
import { popPendingAgentStart } from "../../state/pendingAgentStart";

// ---------------------------------------------------------------------------
// Glass bubble entrance animations
// ---------------------------------------------------------------------------

const BUBBLE_USER = SlideInRight.duration(280).springify().damping(18).stiffness(180);
const BUBBLE_ASSISTANT = SlideInLeft.duration(280).springify().damping(18).stiffness(180);

export function ProjectChatScreen() {
  const toast = useToast();
  const route = useRoute<any>();
  const voice = useVoiceRecording();
  const projectId: string = String(route.params?.projectId);
  const dockPad = 96;

  const [projectName, setProjectName] = useState<string>(projectId);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [thinkingDots, setThinkingDots] = useState(".");
  const ws = useRef<WebSocket | null>(null);
  const scrollRef = useRef<ScrollView | null>(null);
  const pendingStartedRef = useRef(false);

  // Track which messages were loaded (skip entrance animation for those)
  const [loadedIds, setLoadedIds] = useState<Set<string>>(new Set());

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const p = await getProject(projectId);
        if (!cancelled) setProjectName(p.name);
      } catch {
        // non-blocking
      }
      try {
        const saved = await loadChat(projectId);
        if (!cancelled) {
          setMessages(saved);
          setLoadedIds(new Set(saved.map((m) => m.id)));
        }
      } catch {
        // non-blocking
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [projectId]);

  useEffect(() => {
    // Connect WebSocket
    const url = `${config.wsUrl}/projects/${projectId}/agent`;
    const socket = new WebSocket(url);
    ws.current = socket;

    const tryStartPending = async () => {
      if (pendingStartedRef.current) return;
      const pending = await popPendingAgentStart(projectId);
      if (!pending) return;
      if (socket.readyState !== WebSocket.OPEN) return;
      pendingStartedRef.current = true;
      setBusy(true);
      try {
        socket.send(
          JSON.stringify({
            type: "start",
            prompt: pending
          })
        );
      } catch {
        pendingStartedRef.current = false;
        setBusy(false);
      }
    };

    socket.onopen = () => {
      console.log("Agent WS connected");
      void tryStartPending();
      setTimeout(() => {
        void tryStartPending();
      }, 900);
    };

    socket.onmessage = async (e) => {
      try {
        const data = JSON.parse(e.data);

        if (data.type === "chat") {
          const msg: ChatMessage = {
            id: `a_${Date.now()}_${Math.random()}`,
            role: "assistant",
            text: data.text,
            ts: Date.now()
          };
          setMessages((prev) => [...prev, msg]);
          await appendChat(projectId, msg);
        } else if (data.type === "stage") {
          // Stage updates are shown in toasts/preview, not chat transcript.
        } else if (data.type === "agent_state") {
          setBusy(data.state === "thinking");
        } else if (data.type === "preview_update") {
          toast.show("Preview Updated!");
          await addActivity({ title: "Preview updated", detail: projectName || projectId });
          // Trigger global event or context update if needed
        } else if (data.type === "preview_build_state") {
          const state = String(data.state ?? "");
          if (state === "running") {
            // no-op for chat transcript
          }
        } else if (data.type === "bundle_state") {
          const state = String(data.state ?? "");
          if (state === "running") {
            toast.show("Getting your preview ready…");
          }
          if (state === "done") {
            toast.show("Preview ready to scan");
            await addActivity({ title: "Preview ready", detail: projectName || projectId });
          }
          if (state === "failed") toast.show("Preview couldn't be prepared.");
        } else if (data.type === "done") {
            setBusy(false);
            pendingStartedRef.current = false;
            await addActivity({ title: "Generation finished", detail: projectName || projectId });
        } else if (data.type === "error") {
            toast.show("Agent Error: " + data.message);
            setBusy(false);
            pendingStartedRef.current = false;
        }
      } catch (err) {
        console.error("WS msg error", err);
      }
    };

    socket.onclose = () => {
      console.log("Agent WS closed");
    };

    return () => {
      socket.close();
    };
  }, [projectId]);

  useEffect(() => {
    if (!scrollRef.current) return;
    scrollRef.current.scrollToEnd({ animated: true });
  }, [messages, busy]);

  useEffect(() => {
    if (!busy) return;
    const id = setInterval(() => {
      setThinkingDots((prev) => (prev.length >= 3 ? "." : `${prev}.`));
    }, 420);
    return () => clearInterval(id);
  }, [busy]);

  const renderMessageText = useCallback((text: string) => {
    const lines = text.replace(/\r\n/g, "\n").split("\n");
    const blocks: Array<{ type: "p" | "ul" | "ol"; items: string[] }> = [];
    for (const raw of lines) {
      const line = raw.trim();
      if (!line) continue;
      const ul = line.match(/^[-*•]\s+(.+)$/);
      const ol = line.match(/^\d+[.)]\s+(.+)$/);
      if (ul) {
        const last = blocks[blocks.length - 1];
        if (last && last.type === "ul") last.items.push(ul[1]);
        else blocks.push({ type: "ul", items: [ul[1]] });
        continue;
      }
      if (ol) {
        const last = blocks[blocks.length - 1];
        if (last && last.type === "ol") last.items.push(ol[1]);
        else blocks.push({ type: "ol", items: [ol[1]] });
        continue;
      }
      blocks.push({ type: "p", items: [line] });
    }

    if (!blocks.length) {
      return <Text variant="body" style={{ lineHeight: 22 }}>{text}</Text>;
    }

    return (
      <View style={{ gap: 6 }}>
        {blocks.map((b, bi) => {
          if (b.type === "p") {
            return (
              <Text key={`p_${bi}`} variant="body" style={{ lineHeight: 22 }}>
                {b.items.join(" ")}
              </Text>
            );
          }
          return (
            <View key={`${b.type}_${bi}`} style={{ gap: 4 }}>
              {b.items.map((it, ii) => (
                <View key={`${bi}_${ii}`} style={{ flexDirection: "row", gap: 8 }}>
                  <Text variant="body" style={{ lineHeight: 22, minWidth: 16 }}>
                    {b.type === "ul" ? "•" : `${ii + 1}.`}
                  </Text>
                  <Text variant="body" style={{ lineHeight: 22, flex: 1 }}>
                    {it}
                  </Text>
                </View>
              ))}
            </View>
          );
        })}
      </View>
    );
  }, []);

  useFocusEffect(
    useCallback(() => {
      let cancelled = false;
      (async () => {
        try {
          const p = await getProject(projectId);
          if (!cancelled) setProjectName(p.name);
        } catch {
          // ignore
        }
        const pending = await popPendingChatDraft(projectId);
        if (cancelled) return;
        if (pending) setDraft(pending);
      })();
      return () => {
        cancelled = true;
      };
    }, [projectId])
  );

  const send = useCallback(
    async (text: string, voiceText?: string | null) => {
      const trimmed = text.trim();
      if (!trimmed && !voiceText) return;

      const userMsg: ChatMessage = {
        id: `m_${Date.now()}`,
        role: "user",
        text: voiceText || trimmed || "(voice)",
        ts: Date.now()
      };

      setDraft("");
      setMessages((prev) => [...prev, userMsg]);
      await appendChat(projectId, userMsg);
      setBusy(true);

      if (ws.current && ws.current.readyState === WebSocket.OPEN) {
        await addActivity({ title: "Prompt sent", detail: userMsg.text.slice(0, 80) });
        ws.current.send(JSON.stringify({
            type: "start",
            prompt: userMsg.text
        }));
      } else {
        toast.show("Connection lost. Reconnecting...");
        // Reconnection logic is handled by useEffect deps, but for now just warn
        setBusy(false);
      }
    },
    [projectId, toast]
  );

  const pickImage = async () => {
    // No permissions request is necessary for launching the image library
    let result = await ImagePicker.launchImageLibraryAsync({
      mediaTypes: ImagePicker.MediaTypeOptions.Images,
      allowsEditing: true,
      quality: 0.8,
    });

    if (!result.canceled && result.assets && result.assets[0]?.uri) {
      try {
        const asset = await uploadProjectFile(projectId, result.assets[0].uri, "reference");
        toast.show("Added a picture");
        await addActivity({ title: "Uploaded", detail: asset.filename || asset.id });
        await send(`I added a picture called "${asset.filename || "a reference"}". Please use it in the design.`);
      } catch {
        toast.show("Couldn't upload that picture.");
      }
    }
  };

  const canSend = useMemo(() => !!draft.trim() && !busy, [draft, busy]);
  const hasDraft = useMemo(() => !!draft.trim(), [draft]);

  return (
    <Screen>
      <View style={{ flex: 1 }}>
        <ProjectTopBar title={projectName} />

        <View style={{ flex: 1, paddingHorizontal: theme.spacing.lg, paddingBottom: dockPad }}>
          <Text variant="muted" style={{ marginBottom: theme.spacing.md }}>
            Talk, type, iterate.
          </Text>

          <ScrollView
            ref={scrollRef}
            style={{ flex: 1 }}
            contentContainerStyle={{ paddingBottom: 18, gap: theme.spacing.sm }}
            showsVerticalScrollIndicator={false}
            onContentSizeChange={() => scrollRef.current?.scrollToEnd({ animated: true })}
          >
            {messages.map((m) => {
              const isUser = m.role === "user";
              const isNew = !loadedIds.has(m.id);

              const bubbleContent = (
                <View
                  style={{
                    alignSelf: isUser ? "flex-end" : "flex-start",
                    maxWidth: isUser ? "90%" : "100%",
                    paddingVertical: 10,
                    paddingHorizontal: 12,
                    borderRadius: 18,
                    backgroundColor: isUser ? theme.colors.alpha.userBubbleBg : theme.colors.surface.raised,
                    borderWidth: 1,
                    borderColor: isUser ? theme.colors.alpha.userBubbleBorder : theme.colors.stroke.subtle,
                  }}
                >
                  {renderMessageText(m.text)}
                </View>
              );

              // New messages get glass entrance animation
              if (isNew) {
                return (
                  <Animated.View
                    key={m.id}
                    entering={isUser ? BUBBLE_USER : BUBBLE_ASSISTANT}
                  >
                    {bubbleContent}
                  </Animated.View>
                );
              }

              return <View key={m.id}>{bubbleContent}</View>;
            })}
             {busy && (
                <Animated.View entering={FadeIn}>
                     <Text variant="muted" style={{ alignSelf: "center", color: theme.colors.base.textMuted }}>
                      {`Guappa is thinking${thinkingDots}`}
                     </Text>
                </Animated.View>
             )}
          </ScrollView>

          {(voice.state !== "idle" || !!voice.transcript || !!voice.interimText) && (
            <View style={{ marginBottom: theme.spacing.sm }}>
              <TranscriptOverlay state={voice.state} transcript={voice.transcript} interimText={voice.interimText} />
            </View>
          )}

          <View style={{ flexDirection: "row", alignItems: "center", gap: theme.spacing.sm, paddingTop: theme.spacing.sm }}>
            <TouchableOpacity onPress={pickImage} style={{ padding: 10 }}>
                <Ionicons name="image-outline" size={24} color={theme.colors.base.textMuted} />
            </TouchableOpacity>
            
            <View style={{ flex: 1 }}>
              <TextInput
                value={draft}
                onChangeText={setDraft}
                placeholder={busy ? "Thinking..." : "Say what to change..."}
                placeholderTextColor={theme.colors.alpha.textPlaceholder}
                editable={!busy}
                multiline
                style={{
                  minHeight: 56,
                  maxHeight: 120,
                  borderRadius: theme.radii.lg,
                  paddingHorizontal: theme.spacing.md,
                  paddingVertical: 14,
                  backgroundColor: theme.colors.surface.raised,
                  borderWidth: 1,
                  borderColor: theme.colors.stroke.subtle,
                  color: theme.colors.base.text,
                  fontFamily: theme.typography.body,
                  opacity: busy ? 0.5 : 1
                }}
              />
            </View>

            <VoiceRecordButton
              size={56}
              style={{ alignSelf: "center" }}
              mode={hasDraft ? "send" : "voice"}
              disabled={busy || (hasDraft && !canSend)}
              onPress={hasDraft ? () => (canSend ? send(draft) : undefined) : undefined}
              onRecordStart={hasDraft ? undefined : voice.start}
              onRecordEnd={hasDraft ? undefined : voice.stop}
              volume={voice.volume}
              onVoiceResult={hasDraft ? undefined : (t) => send("", t)}
            />
          </View>
        </View>
      </View>
    </Screen>
  );
}
