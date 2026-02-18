import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { View, ScrollView, TextInput, Pressable, Linking } from "react-native";
import Animated, { FadeIn, SlideInLeft, SlideInRight } from "react-native-reanimated";
import { useFocusEffect } from "@react-navigation/native";
import Markdown from "react-native-markdown-display";
import MarkdownIt from "markdown-it";

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
const MARKDOWN_NO_TABLES = new MarkdownIt({ breaks: true, linkify: true, typographer: true }).disable(["table"]);

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

  const markdownStyles = useMemo(
    () => ({
      body: { color: theme.colors.base.text, fontFamily: theme.typography.body, fontSize: 15, lineHeight: 22 },
      paragraph: { marginTop: 0, marginBottom: 8 },
      heading1: { fontFamily: theme.typography.bodyMedium, fontSize: 24, lineHeight: 30, marginBottom: 8 },
      heading2: { fontFamily: theme.typography.bodyMedium, fontSize: 20, lineHeight: 28, marginBottom: 8 },
      heading3: { fontFamily: theme.typography.bodyMedium, fontSize: 18, lineHeight: 24, marginBottom: 6 },
      heading4: { fontFamily: theme.typography.bodyMedium, fontSize: 16, lineHeight: 22, marginBottom: 6 },
      heading5: { fontFamily: theme.typography.bodyMedium, fontSize: 15, lineHeight: 22, marginBottom: 6 },
      heading6: { fontFamily: theme.typography.bodyMedium, fontSize: 14, lineHeight: 20, marginBottom: 6 },
      bullet_list: { marginTop: 0, marginBottom: 8 },
      ordered_list: { marginTop: 0, marginBottom: 8 },
      list_item: { marginBottom: 4 },
      blockquote: {
        marginTop: 0,
        marginBottom: 8,
        paddingVertical: 8,
        paddingHorizontal: 10,
        borderLeftWidth: 3,
        borderColor: theme.colors.stroke.subtle,
        backgroundColor: theme.colors.surface.panel,
      },
      strong: { fontFamily: theme.typography.bodyMedium },
      em: { fontStyle: "italic" },
      code_inline: {
        fontFamily: theme.typography.mono,
        backgroundColor: theme.colors.surface.panel,
        paddingHorizontal: 4,
        paddingVertical: 2,
      },
      fence: {
        fontFamily: theme.typography.mono,
        backgroundColor: theme.colors.surface.panel,
        borderWidth: 1,
        borderColor: theme.colors.stroke.subtle,
        borderRadius: 10,
        paddingHorizontal: 10,
        paddingVertical: 8,
      },
      code_block: {
        fontFamily: theme.typography.mono,
        backgroundColor: theme.colors.surface.panel,
        borderWidth: 1,
        borderColor: theme.colors.stroke.subtle,
        borderRadius: 10,
        paddingHorizontal: 10,
        paddingVertical: 8,
      },
      link: { color: theme.colors.base.primary },
      hr: { backgroundColor: theme.colors.stroke.subtle },
    }),
    [],
  );

  const renderMessageText = useCallback(
    (message: ChatMessage) => {
      if (message.role !== "assistant") {
        return (
          <Text variant="body" style={{ lineHeight: 22 }}>
            {message.text}
          </Text>
        );
      }

      return (
        <Markdown
          markdownit={MARKDOWN_NO_TABLES}
          style={markdownStyles}
          onLinkPress={(url) => {
            void Linking.openURL(url);
            return false;
          }}
        >
          {message.text}
        </Markdown>
      );
    },
    [markdownStyles],
  );

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
                {renderMessageText(m)}
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
