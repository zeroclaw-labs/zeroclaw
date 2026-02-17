import React from "react";
import { createBottomTabNavigator } from "@react-navigation/bottom-tabs";
import { createNativeStackNavigator } from "@react-navigation/native-stack";

import { FloatingDock } from "../../ui/navigation/FloatingDock";
import { DockMorphProvider } from "../../ui/animation/DockMorphProvider";

import { ActivityScreen } from "../screens/tabs/ActivityScreen";
import { ChatScreen } from "../screens/tabs/ChatScreen";
import { SettingsScreen } from "../screens/tabs/SettingsScreen";
import { IntegrationsScreen } from "../screens/tabs/IntegrationsScreen";
import { DeviceScreen } from "../screens/tabs/DeviceScreen";
import { SecurityScreen } from "../screens/tabs/SecurityScreen";

type MainTabParamList = {
  chat: undefined;
  activity: undefined;
  settings: undefined;
  integrations: undefined;
  device: undefined;
};

const MainTabs = createBottomTabNavigator<MainTabParamList>();
const Stack = createNativeStackNavigator();

function MainTabsNavigator() {
  return (
    <MainTabs.Navigator
      id="main-tabs"
      initialRouteName="chat"
      screenOptions={{ headerShown: false }}
      tabBar={(props) => <FloatingDock {...props} />}
    >
      <MainTabs.Screen name="activity" component={ActivityScreen} options={{ title: "Activity" }} />
      <MainTabs.Screen name="integrations" component={IntegrationsScreen} options={{ title: "Integrations" }} />
      <MainTabs.Screen name="chat" component={ChatScreen} options={{ title: "Chat" }} />
      <MainTabs.Screen name="device" component={DeviceScreen} options={{ title: "Device" }} />
      <MainTabs.Screen name="settings" component={SettingsScreen} options={{ title: "Settings" }} />
    </MainTabs.Navigator>
  );
}

export function RootNavigator() {
  return (
    <DockMorphProvider>
      <Stack.Navigator id="root-stack" screenOptions={{ headerShown: false }}>
        <Stack.Screen name="MainTabs" component={MainTabsNavigator} />
        <Stack.Screen name="Security" component={SecurityScreen} />
      </Stack.Navigator>
    </DockMorphProvider>
  );
}
