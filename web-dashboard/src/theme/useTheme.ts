/**
 * `useTheme` (M3, US-004).
 *
 * Reads/writes the user's theme + mode preference to the same
 * localStorage key the FOUC-avoidance script in `index.html` reads
 * before React hydrates. That script sets the *initial* `<html
 * data-theme data-mode>` attributes; this hook keeps them in sync as
 * the user toggles the switcher at runtime.
 *
 * Storage shape (versioned key so we can migrate later without
 * stranding existing users on a malformed blob):
 *
 *   {
 *     "theme": "default" | "monochrome" | "contrast",
 *     "mode":  "light"   | "dark"
 *   }
 */
import { useCallback, useEffect, useState } from "react";

export const THEME_STORAGE_KEY = "zeroclaw.control.settings.v1";

export const THEMES = ["default", "monochrome", "contrast"] as const;
export const MODES = ["light", "dark"] as const;

export type Theme = (typeof THEMES)[number];
export type Mode = (typeof MODES)[number];

interface StoredSettings {
  theme?: Theme;
  mode?: Mode;
}

interface UseThemeResult {
  theme: Theme;
  mode: Mode;
  setTheme: (theme: Theme) => void;
  setMode: (mode: Mode) => void;
}

export function useTheme(): UseThemeResult {
  const [theme, setThemeState] = useState<Theme>(() => readStored().theme ?? "default");
  const [mode, setModeState] = useState<Mode>(() => {
    const stored = readStored().mode;
    if (stored) return stored;
    return prefersDarkMode() ? "dark" : "light";
  });

  // Apply attributes on every change so CSS tokens flip immediately.
  useEffect(() => {
    document.documentElement.setAttribute("data-theme", theme);
  }, [theme]);
  useEffect(() => {
    document.documentElement.setAttribute("data-mode", mode);
  }, [mode]);

  // Track OS-level dark/light preference for users who haven't
  // explicitly chosen a mode. If they have a stored preference, we
  // respect it and ignore OS changes.
  useEffect(() => {
    if (readStored().mode) return;
    if (typeof window.matchMedia !== "function") return;
    const mql = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => {
      // Re-check stored just in case the user set a preference between
      // mount and event firing.
      if (readStored().mode) return;
      setModeState(mql.matches ? "dark" : "light");
    };
    mql.addEventListener("change", onChange);
    return () => mql.removeEventListener("change", onChange);
  }, []);

  const setTheme = useCallback((next: Theme) => {
    setThemeState(next);
    persistChange({ theme: next });
  }, []);

  const setMode = useCallback((next: Mode) => {
    setModeState(next);
    persistChange({ mode: next });
  }, []);

  return { theme, mode, setTheme, setMode };
}

function readStored(): StoredSettings {
  try {
    const raw = localStorage.getItem(THEME_STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as StoredSettings;
    return {
      theme: isTheme(parsed.theme) ? parsed.theme : undefined,
      mode: isMode(parsed.mode) ? parsed.mode : undefined,
    };
  } catch {
    return {};
  }
}

function persistChange(patch: StoredSettings): void {
  try {
    const current = readStored();
    const next = { ...current, ...patch };
    localStorage.setItem(THEME_STORAGE_KEY, JSON.stringify(next));
  } catch {
    // localStorage unavailable (private mode, quota, …) — leave the
    // in-memory state alone; CSS already updated.
  }
}

function isTheme(v: unknown): v is Theme {
  return typeof v === "string" && (THEMES as readonly string[]).includes(v);
}

function isMode(v: unknown): v is Mode {
  return typeof v === "string" && (MODES as readonly string[]).includes(v);
}

function prefersDarkMode(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return false;
  }
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}
