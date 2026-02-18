import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { View, ScrollView, TextInput, Pressable } from "react-native";
import Animated, { FadeIn, SlideInLeft, SlideInRight } from "react-native-reanimated";
import { useFocusEffect } from "@react-navigation/native";

import { Screen } from "../../../ui/primitives/Screen";
import { Text } from "../../../ui/primitives/Text";
import { theme } from "../../../ui/theme";
import { VoiceRecordButton } from "../../../ui/voice/VoiceRecordButton";
import { TranscriptOverlay } from "../../../ui/voice/TranscriptOverlay";
import { useVoiceRecording } from "../../hooks/useVoiceRecording";
import { useToast } from "../../state/toast";
import { appendChat, loadChat, type ChatMessage } from "../../state/chat";
import { addActivity } from "../../state/activity";
import { loadAgentConfig } from "../../state/mobileclaw";
import { runAgentTurn } from "../../runtime/session";

const BUBBLE_USER = SlideInRight.duration(280).springify().damping(18).stiffness(180);
const BUBBLE_ASSISTANT = SlideInLeft.duration(280).springify().damping(18).stiffness(180);

export function ChatScreen() {
  const toast = useToast();
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [thinkingDots, setThinkingDots] = useState(".");
  const [loadedIds, setLoadedIds] = useState<Set<string>>(new Set());
  const [deepgramApiKey, setDeepgramApiKey] = useState("");
  const voice = useVoiceRecording(deepgramApiKey);
  const scrollRef = useRef<ScrollView | null>(null);
  const runNonceRef = useRef(0);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const [saved, cfg] = await Promise.all([loadChat(), loadAgentConfig()]);
      if (cancelled) return;
      setMessages(saved);
      setLoadedIds(new Set(saved.map((m) => m.id)));
      setDeepgramApiKey(cfg.deepgramApiKey);
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useFocusEffect(
    useCallback(() => {
      let cancelled = false;
      (async () => {
        const cfg = await loadAgentConfig();
        if (!cancelled) setDeepgramApiKey(cfg.deepgramApiKey);
      })();
      return () => {
        cancelled = true;
      };
    }, []),
  );

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
      return (
        <Text variant="body" style={{ lineHeight: 22 }}>
          {text}
        </Text>
      );
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

  const runTurnWithTimeout = useCallback(async (prompt: string) => {
    const timeoutMs = 90_000;
    return await Promise.race([
      runAgentTurn(prompt),
      new Promise<never>((_, reject) => {
        setTimeout(() => reject(new Error("Agent request timed out. You can restart and retry.")), timeoutMs);
      }),
    ]);
  }, []);

  const restartAgent = useCallback(async () => {
    runNonceRef.current += 1;
    setBusy(false);
    setThinkingDots(".");
    const assistantMsg: ChatMessage = {
      id: `a_restart_${Date.now()}`,
      role: "assistant",
      text: "Agent runtime restarted. Please retry your request.",
      ts: Date.now(),
    };
    setMessages((prev) => [...prev, assistantMsg]);
    await appendChat(assistantMsg);
    await addActivity({ kind: "action", source: "chat", title: "Agent restarted", detail: "Runtime state was reset from chat screen" });
  }, []);

  const send = useCallback(
    async (text: string, voiceText?: string | null) => {
      const trimmed = text.trim();
      if (!trimmed && !voiceText) return;

      const userMsg: ChatMessage = {
        id: `m_${Date.now()}`,
        role: "user",
        text: voiceText || trimmed || "(voice)",
        ts: Date.now(),
      };

      setDraft("");
      setMessages((prev) => [...prev, userMsg]);
      await appendChat(userMsg);
      await addActivity({ kind: "message", source: "chat", title: "User message", detail: userMsg.text.slice(0, 120) });

      const runNonce = runNonceRef.current + 1;
      runNonceRef.current = runNonce;
      setBusy(true);
      try {
        const result = await runTurnWithTimeout(userMsg.text);
        if (runNonceRef.current !== runNonce) return;

        const assistantMsg: ChatMessage = {
          id: `a_${Date.now()}_${Math.random()}`,
          role: "assistant",
          text: result.assistantText || "(empty response)",
          ts: Date.now(),
        };
        setMessages((prev) => [...prev, assistantMsg]);
        await appendChat(assistantMsg);
        await addActivity({ kind: "action", source: "chat", title: "Agent response", detail: assistantMsg.text.slice(0, 120) });

        for (const event of result.toolEvents) {
          await addActivity({
            kind: "action",
            source: "chat",
            title: `Tool ${event.status}`,
            detail: `${event.tool}: ${event.detail}`,
          });
        }
      } catch (error) {
        if (runNonceRef.current !== runNonce) return;
        const detail = error instanceof Error ? error.message : "Unknown error";
        toast.show(detail);
        const assistantMsg: ChatMessage = {
          id: `a_err_${Date.now()}_${Math.random()}`,
          role: "assistant",
          text: `Agent error: ${detail}. You can tap Restart Agent and try again.`,
          ts: Date.now(),
        };
        setMessages((prev) => [...prev, assistantMsg]);
        await appendChat(assistantMsg);
        await addActivity({ kind: "log", source: "chat", title: "Agent error", detail });
      } finally {
        if (runNonceRef.current === runNonce) {
          setBusy(false);
        }
      }
    },
    [runTurnWithTimeout, toast],
  );

  const canSend = useMemo(() => !!draft.trim() && !busy, [draft, busy]);
  const hasDraft = useMemo(() => !!draft.trim(), [draft]);

  return (
    <Screen>
      <View style={{ flex: 1, paddingHorizontal: theme.spacing.lg, paddingTop: theme.spacing.xl, paddingBottom: 92 }}>
        <Text testID="screen-chat" variant="display">Chat</Text>
        <View style={{ flexDirection: "row", justifyContent: "space-between", alignItems: "center", marginTop: theme.spacing.xs }}>
          <Text variant="muted">MobileClaw agent chat with voice mode.</Text>
          <Pressable
            testID="chat-restart-agent"
            onPress={() => {
              void restartAgent();
            }}
            style={{
              paddingHorizontal: 10,
              paddingVertical: 6,
              borderRadius: theme.radii.md,
              backgroundColor: theme.colors.surface.panel,
              borderWidth: 1,
              borderColor: theme.colors.stroke.subtle,
            }}
          >
            <Text variant="label">Restart Agent</Text>
          </Pressable>
        </View>
        <View style={{ marginBottom: theme.spacing.md }} />

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
            if (isNew) {
              return (
                <Animated.View key={m.id} entering={isUser ? BUBBLE_USER : BUBBLE_ASSISTANT}>
                  {bubbleContent}
                </Animated.View>
              );
            }
            return <View key={m.id}>{bubbleContent}</View>;
          })}
          {busy && (
            <Animated.View entering={FadeIn}>
              <Text variant="muted" style={{ alignSelf: "center", color: theme.colors.base.textMuted }}>
                {`MobileClaw is thinking${thinkingDots}`}
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
          <View style={{ flex: 1 }}>
            <TextInput
              testID="chat-input"
              value={draft}
              onChangeText={setDraft}
              placeholder={busy ? "Thinking..." : "Tell agent what to do..."}
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
                opacity: busy ? 0.5 : 1,
              }}
            />
          </View>

          <VoiceRecordButton
            testID="chat-send-or-voice"
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
    </Screen>
  );
}
