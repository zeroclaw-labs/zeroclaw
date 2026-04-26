# Demo Prompts — Copy/Paste Reference

Paste these in order during the live demo.

## 1. System primer (paste FIRST, before any other prompt)

> You are controlling a simulated ESP32 microcontroller wired into a smart room. The chip exposes these GPIO pins through the `gpio_write` and `gpio_read` tools:
>
> | Pin | Device           | Direction |
> |-----|------------------|-----------|
> | 12  | reading lamp     | output    |
> | 13  | overhead light   | output    |
> | 14  | space heater     | output    |
> | 2   | fan / status LED | output    |
> | 5   | motion sensor    | input     |
>
> Rules of engagement:
> - **Always actuate via tool calls.** Do not describe what you would do — call `gpio_write` and `gpio_read` directly.
> - For output pins, `value: 1` means on, `value: 0` means off.
> - Before changing the room, read the motion sensor (pin 5) once to confirm presence.
> - After all tool calls complete, write ONE sentence summarizing what you did.
>
> Acknowledge by reading the motion sensor.

Expected response: a single `gpio_read(pin=5)` tool call, then a one-liner like "Motion sensor reads 1 — presence confirmed."

## 2. Demo turn — the headline

> It's getting dark and chilly. I'm settling in to read for an hour.

Expected tool calls (any order):
- `gpio_write(pin=12, value=1)` → reading lamp on
- `gpio_write(pin=14, value=1)` → heater on
- `gpio_write(pin=13, value=0)` → overhead off (already off, but explicit)
- summary line

## 3. Demo turn — the contrast

> Going to bed now.

Expected:
- `gpio_write(pin=12, value=0)`
- `gpio_write(pin=14, value=0)`
- `gpio_write(pin=2, value=0)` (fan off)
- summary

## 4. Improv (only if 30+ sec on the clock)

> Make it dramatic for movie night.

Loose expected:
- overhead off, lamp dim/off (set to 0)
- fan/LED on (ambient light)
- heater on for cozy

## Manual fallback

If the model freelances or stalls, click the manual flip buttons at the bottom-right of the frontend. The narration becomes:

> "The agent makes the same tool calls behind the scenes — let's flip them manually so you can see the protocol works end-to-end."

## What "good" looks like

Pin-flip latency from prompt submission to icon update should be under ~3 seconds with M2.7 on a US connection. Any longer and either the API is slow or the model is reasoning out of band; the manual buttons keep the show moving.
