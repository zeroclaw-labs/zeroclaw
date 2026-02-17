import React, { useCallback, useState } from "react";
import { View, TextInput, KeyboardAvoidingView, Platform } from "react-native";
import { useNavigation } from "@react-navigation/native";

import { createProject, setProjectExecutionMode } from "../../api/platform";
import { Screen } from "../../../ui/primitives/Screen";
import { Text } from "../../../ui/primitives/Text";
import { LiquidButton } from "../../../ui/primitives/LiquidButton";
import { VoiceRecordButton } from "../../../ui/voice/VoiceRecordButton";
import { TranscriptOverlay } from "../../../ui/voice/TranscriptOverlay";
import { useVoiceRecording } from "../../hooks/useVoiceRecording";
import { useToast } from "../../state/toast";
import { appendChat, type ChatMessage } from "../../state/chat";
import { setPendingAgentStart } from "../../state/pendingAgentStart";
import { addActivity } from "../../state/activity";
import { theme } from "../../../ui/theme";

export function CreateScreen() {
  const navigation = useNavigation<any>();
  const toast = useToast();
  const voice = useVoiceRecording();
  const [idea, setIdea] = useState("");
  const [busy, setBusy] = useState(false);

  const onCreate = useCallback(async () => {
    const prompt = idea.trim();
    if (!prompt) {
      toast.show("Add an idea first.");
      return;
    }
    setBusy(true);
    try {
      const project = await createProject({ name: "New Project", visibility: "private" });
      try {
        await setProjectExecutionMode(project.id, "quick_preview");
      } catch {
        // best effort
      }

      // Seed the project chat with the user's idea, then simulate an agent kickoff.
      const userMsg: ChatMessage = {
        id: `m_${Date.now()}`,
        role: "user",
        text: prompt,
        ts: Date.now(),
      };
      await appendChat(project.id, userMsg);

      const assistantKickoff: ChatMessage = {
        id: `a_${Date.now()}`,
        role: "assistant",
        text: "Got it. I'm creating your app now.",
        ts: Date.now(),
      };
      await appendChat(project.id, assistantKickoff);

      await setPendingAgentStart(project.id, prompt);
      await addActivity({ title: "Project created", detail: project.name });
      navigation.navigate("Project", { projectId: project.id });
    } catch {
      toast.show("Couldn't start that build.");
    } finally {
      setBusy(false);
    }
  }, [idea, navigation, toast]);

  return (
    <Screen>
      <KeyboardAvoidingView
        behavior={Platform.OS === "ios" ? "padding" : undefined}
        style={{ flex: 1, paddingHorizontal: theme.spacing.lg, paddingTop: theme.spacing.xl, paddingBottom: 170 }}
      >
        <Text variant="display">Create</Text>
        <Text variant="muted" style={{ marginTop: theme.spacing.xs, maxWidth: 320 }}>
          Describe your app in one sentence. Tap the Nucleus to start and stop voice.
        </Text>

        <View style={{ alignItems: "center", marginTop: theme.spacing.lg }}>
          <VoiceRecordButton
            size={132}
            disabled={busy}
            onRecordStart={voice.start}
            onRecordEnd={voice.stop}
            volume={voice.volume}
            onVoiceResult={(t) => setIdea((prev) => (prev ? `${prev}\n${t}` : t))}
          />

          <View style={{ marginTop: theme.spacing.sm, minHeight: 40 }}>
            <TranscriptOverlay state={voice.state} transcript={voice.transcript} interimText={voice.interimText} />
          </View>
        </View>

        <View style={{ marginTop: theme.spacing.xl }}>
          <Text variant="label" style={{ marginBottom: theme.spacing.xs }}>
            Idea
          </Text>
          <TextInput
            testID="create-idea-input"
            value={idea}
            onChangeText={setIdea}
            onSubmitEditing={() => {
              void onCreate();
            }}
            placeholder="Make a simple shopping list app"
            placeholderTextColor={theme.colors.alpha.textPlaceholder}
            multiline
            blurOnSubmit
            returnKeyType="done"
            style={{
              minHeight: 110,
              borderRadius: theme.radii.lg,
              padding: theme.spacing.md,
              backgroundColor: theme.colors.surface.raised,
              borderWidth: 1,
              borderColor: theme.colors.stroke.subtle,
              color: theme.colors.base.text,
              fontFamily: theme.typography.body,
              lineHeight: 22
            }}
          />
        </View>

        <View style={{ marginTop: theme.spacing.lg }}>
          <LiquidButton testID="create-project-button" label={busy ? "Building…" : "Create project"} onPress={onCreate} disabled={busy} />
        </View>
      </KeyboardAvoidingView>
    </Screen>
  );
}
