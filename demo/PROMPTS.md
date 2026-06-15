# Demo Prompts — Copy/Paste Reference

Paste these in order during the live demo.

## 1. System primer (paste FIRST, before any other prompt)

> You are controlling a simulated ESP32 microcontroller wired into a smart room.
> You have access to high-level smart-room tools:
>
> - `set_device(device="reading_lamp", state="on"|"off")` — controls lights, heater, fan by friendly name
> - `read_device(device="motion_sensor")` — reads sensors
>
> Available devices for set_device: reading_lamp, overhead_light, heater, fan.
> Use read_device for the motion_sensor.
>
> Rules of engagement:
> - **Always use the smart-room tools** (`set_device` / `read_device`) when the user asks about lights, comfort, or presence.
> - Do NOT guess pin numbers or call raw gpio_write unless the user explicitly asks for low-level control.
> - Before changing the room, read the motion sensor once to confirm presence.
> - After tool calls, write ONE short sentence summarizing the physical changes.

Expected response: a `read_device(device="motion_sensor")` call, then a one-liner.

## 2. Demo turn — the headline

> It's getting dark and chilly. I'm settling in to read for an hour.

Expected tool calls (any order):
- `read_device(device="motion_sensor")`
- `set_device(device="reading_lamp", state="on")`
- `set_device(device="heater", state="on")`
- `set_device(device="overhead_light", state="off")`
- summary sentence

## 3. Demo turn — the contrast

> Going to bed now.

Expected:
- `read_device(device="motion_sensor")`
- `set_device(device="reading_lamp", state="off")`
- `set_device(device="heater", state="off")`
- `set_device(device="fan", state="off")`
- summary

## 4. Improv (only if 30+ sec on the clock)

> Make it dramatic for movie night.

Use the smart-room tools to create atmosphere (cozy lighting, fan for "air", etc.).

## Manual fallback

If the model freelances or stalls, click the manual flip buttons at the bottom-right of the frontend. The narration becomes:

> "The agent makes the same tool calls behind the scenes — let's flip them manually so you can see the protocol works end-to-end."

## What "good" looks like

Pin-flip latency from prompt submission to icon update should be under ~3-5 seconds with a reasonable model on OpenRouter (US connection). Any longer and either the API is slow or the model is reasoning out of band; the manual buttons on the visualizer keep the show moving (they call the same /manual endpoint the agent would).
