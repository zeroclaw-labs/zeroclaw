import "react-native-gesture-handler";

import React, { useEffect } from "react";
import { Platform } from "react-native";
import { GestureHandlerRootView } from "react-native-gesture-handler";
import { NavigationContainer } from "@react-navigation/native";
import { StatusBar } from "expo-status-bar";
import * as Font from "expo-font";
import { SafeAreaProvider } from "react-native-safe-area-context";

import { Inter_400Regular, Inter_500Medium } from "@expo-google-fonts/inter";
import { SpaceGrotesk_600SemiBold } from "@expo-google-fonts/space-grotesk";
import { JetBrainsMono_500Medium } from "@expo-google-fonts/jetbrains-mono";

import { theme } from "./ui/theme";
import { ToastProvider } from "./src/state/toast";
import { ActivityProvider } from "./src/state/activity";
import { RootNavigator } from "./src/navigation/RootNavigator";
import { log } from "./src/logger";
import { ErrorBoundary } from "./src/state/ErrorBoundary";
import { addActivity } from "./src/state/activity";
import { loadSecurityConfig } from "./src/state/mobileclaw";
import { subscribeIncomingCalls } from "./src/native/incomingCalls";

export default function App() {
  const [fontsLoaded] = Font.useFonts({
    Inter_400Regular,
    Inter_500Medium,
    SpaceGrotesk_600SemiBold,
    JetBrainsMono_500Medium
  });

  useEffect(() => {
    if (!fontsLoaded) return;
    log("info", "app started", { platform: Platform.OS });
  }, [fontsLoaded]);

  useEffect(() => {
    if (!fontsLoaded) return;

    const unsubscribe = subscribeIncomingCalls((event) => {
      void (async () => {
        const security = await loadSecurityConfig();
        if (!security.incomingCallHooks) return;
        const suffix = security.includeCallerNumber && event.phone.trim() ? event.phone.trim() : "redacted";
        await addActivity({
          kind: "action",
          source: "device",
          title: "Incoming call hook",
          detail: `${event.state} from ${suffix}`,
        });
      })();
    });

    return () => unsubscribe();
  }, [fontsLoaded]);

  if (!fontsLoaded) return null;

  return (
    <GestureHandlerRootView style={{ flex: 1, backgroundColor: theme.colors.base.background }}>
      <StatusBar style="light" />
      <SafeAreaProvider>
        <ToastProvider>
          <ActivityProvider>
            <NavigationContainer>
              <ErrorBoundary>
                <RootNavigator />
              </ErrorBoundary>
            </NavigationContainer>
          </ActivityProvider>
        </ToastProvider>
      </SafeAreaProvider>
    </GestureHandlerRootView>
  );
}
