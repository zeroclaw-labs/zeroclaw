# MobileClaw Mobile (Expo)

Environment:
- Copy `.env.example` to `.env` and adjust values.
- Keep `EXPO_PUBLIC_PLATFORM_URL` pointed to your local/mobileclaw backend.

Run:

```bash
npm install
npm start
```

Native simulator/device:

```bash
npm --prefix mobile-app run ios:native
npm --prefix mobile-app run android:native
```

Features shipped in this app:
- Chat screen with voice mode (Deepgram key in Settings)
- Activity timeline for agent actions/messages/logs
- Provider settings (OpenAI, Anthropic, Gemini, OpenRouter, Copilot, Ollama)
- Integrations configuration screen
- Device actions screen
- Security policy screen

EAS builds:

```bash
eas build --platform ios --profile preview
eas build --platform android --profile preview
```
