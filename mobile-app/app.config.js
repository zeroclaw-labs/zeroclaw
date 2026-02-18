const requireEnv = (key) => {
  const value = process.env[key];
  if (value) return value;
  const defaults = {
    EXPO_APP_NAME: "MobileClaw",
    EXPO_APP_SLUG: "mobileclaw",
    EXPO_APP_SCHEME: "mobileclaw",
    EXPO_APP_VERSION: "0.1.0",
    EXPO_PUBLIC_PLATFORM_URL: "http://10.0.2.2:11434",
    EXPO_PUBLIC_LOG_LEVEL: "info",
    EXPO_PUBLIC_THEME_PRIMARY: "#D4F49C",
    EXPO_PUBLIC_THEME_SECONDARY: "#5CC8FF",
    EXPO_PUBLIC_THEME_ACCENT: "#6CE2C0",
    EXPO_PUBLIC_THEME_BG: "#050A16",
    EXPO_PUBLIC_THEME_TEXT: "#F5F0E6",
    EXPO_PUBLIC_THEME_BORDER: "#FFFFFF",
    EXPO_PUBLIC_THEME_TEXT_MUTED: "#9CAAC4",
  };
  return defaults[key];
};

module.exports = {
  expo: {
    name: requireEnv("EXPO_APP_NAME"),
    slug: requireEnv("EXPO_APP_SLUG"),
    scheme: requireEnv("EXPO_APP_SCHEME"),
    version: requireEnv("EXPO_APP_VERSION"),
    platforms: ["ios", "android", "web"],
    plugins: [],
    ios: {
      bundleIdentifier: "com.mobileclaw.app",
      infoPlist: {
        NSMicrophoneUsageDescription: "We use your microphone to capture voice input for real-time transcription.",
        LSApplicationQueriesSchemes: ["mobileclaw-preview"]
      }
    },
    android: {
      package: "com.mobileclaw.app",
      permissions: [
        "RECORD_AUDIO",
        "POST_NOTIFICATIONS",
        "READ_PHONE_STATE",
        "CALL_PHONE",
        "READ_CALL_LOG",
        "SEND_SMS",
        "READ_SMS",
        "RECEIVE_SMS",
        "ACCESS_COARSE_LOCATION",
        "ACCESS_FINE_LOCATION",
        "CAMERA",
        "READ_CONTACTS",
        "READ_CALENDAR",
        "WRITE_CALENDAR",
        "BLUETOOTH_SCAN",
        "BLUETOOTH_CONNECT",
        "NFC",
        "READ_EXTERNAL_STORAGE",
        "WRITE_EXTERNAL_STORAGE",
        "READ_MEDIA_IMAGES",
        "READ_MEDIA_VIDEO",
        "READ_MEDIA_AUDIO",
        "MANAGE_EXTERNAL_STORAGE"
      ]
    },
    extra: {
      eas: {
        projectId: "255de43e-4d02-4104-ae91-8afe5e732f66"
      },
      EXPO_PUBLIC_PLATFORM_URL: requireEnv("EXPO_PUBLIC_PLATFORM_URL"),
      EXPO_PUBLIC_LOG_LEVEL: requireEnv("EXPO_PUBLIC_LOG_LEVEL"),
      EXPO_PUBLIC_DEMO_MODE: process.env.EXPO_PUBLIC_DEMO_MODE ?? "false",
      EXPO_PUBLIC_THEME_PRIMARY: requireEnv("EXPO_PUBLIC_THEME_PRIMARY"),
      EXPO_PUBLIC_THEME_SECONDARY: requireEnv("EXPO_PUBLIC_THEME_SECONDARY"),
      EXPO_PUBLIC_THEME_ACCENT: requireEnv("EXPO_PUBLIC_THEME_ACCENT"),
      EXPO_PUBLIC_THEME_BG: requireEnv("EXPO_PUBLIC_THEME_BG"),
      EXPO_PUBLIC_THEME_TEXT: requireEnv("EXPO_PUBLIC_THEME_TEXT"),
      EXPO_PUBLIC_THEME_BORDER: requireEnv("EXPO_PUBLIC_THEME_BORDER"),
      EXPO_PUBLIC_THEME_TEXT_MUTED: requireEnv("EXPO_PUBLIC_THEME_TEXT_MUTED")
    }
  }
};
