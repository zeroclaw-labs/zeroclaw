/**
 * Personality file index query (M4a, US-005).
 *
 * Reads `GET /api/personality` so the SettingsDrawer's Advanced tab
 * can render a dropdown of allowlisted personality filenames the user
 * may attach to a slot. The endpoint already exists upstream and is
 * the source of truth for `EDITABLE_PERSONALITY_FILES` — surfacing it
 * here keeps the frontend out of hardcoding the same list.
 */
import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "@/lib/apiFetch";

export interface PersonalityIndexEntry {
  filename: string;
  exists: boolean;
  size: number;
  mtime_ms?: number | null;
}

export interface PersonalityIndex {
  files: PersonalityIndexEntry[];
  max_chars: number;
}

export const PERSONALITY_INDEX_QUERY_KEY = ["personality", "index"] as const;

export function usePersonalityIndex() {
  return useQuery({
    queryKey: PERSONALITY_INDEX_QUERY_KEY,
    queryFn: () => apiFetch<PersonalityIndex>("/api/personality"),
    staleTime: 60_000,
  });
}
