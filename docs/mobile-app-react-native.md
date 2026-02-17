# MobileClaw React Native App

MobileClaw mobile client is implemented in `mobile-app/` as an Expo/React Native application.
The UI baseline is adapted from `/Users/aostapenko/Work/guappa.ai/mobile-app` and keeps its theme, animation language, and safe-area layout patterns.

## Core screens

- `Chat`: text + voice mode (Deepgram STT)
- `Activity`: agent actions, messages, logs, errors
- `Settings`: provider configuration and credentials
- `Integrations`: Telegram/Discord/Slack/WhatsApp/Composio setup fields
- `Device`: user-facing device action stubs/logs
- `Security`: approval/high-risk toggles

## Voice mode

- Voice mode is available in chat.
- It records short audio clip and sends it to Deepgram.
- Deepgram API key is configured in Settings UI (`Deepgram API Key`).

## Provider auth

Provider setup is configurable in app UI:

- Ollama
- OpenAI
- OpenRouter
- Anthropic
- Gemini
- GitHub Copilot

Credential modes:

- API key
- OAuth token (provider-dependent)

Model selection notes:

- Provider picker is a dropdown list.
- Model picker is a searchable dropdown list.
- For OpenRouter, model list is fetched from the live OpenRouter catalog (with fallback seed list).

## Integrations UI setup

The app includes friendly setup forms with hints for:

- Telegram bot token + chat id
- Discord bot token
- Slack bot token
- WhatsApp access token
- Composio API key

## Run

```bash
cd mobile-app
npm install
npm run start
```

Then run on device/emulator from Expo CLI.

## E2E UI tests

Maestro flows are in `mobile-app/.maestro/` and cover:

- app launch + chat tab
- settings save (including Deepgram key)
- integrations save
- security save
- activity visibility

Run:

```bash
cd mobile-app
maestro test .maestro/smoke_navigation.yaml
maestro test .maestro/chat_activity.yaml
```
