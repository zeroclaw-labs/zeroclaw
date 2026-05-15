/**
 * Persona preset queries + mutations (M4a, US-004).
 *
 * Personas are user-authored bundles of `(provider, model,
 * personality, mode)` saved as TOML files under
 * `<workspace_dir>/personas/`. The Quick tab of the slot settings
 * drawer uses the list to render a preset dropdown; selecting one
 * stamps all four fields onto the slot via `PATCH /api/slots/:id`.
 *
 * Wire shapes mirror `crates/zeroclaw-gateway/src/api_personas.rs`:
 *   GET    /api/personas          -> PersonaListResponse
 *   GET    /api/personas/:name    -> PersonaPreset (404 when missing)
 *   POST   /api/personas          PersonaPreset -> PersonaPreset
 *   DELETE /api/personas/:name    -> 204
 *
 * On first call against an empty/missing personas dir the gateway
 * seeds four bundled defaults: claude-code-default, codex-researcher,
 * gemini-cli-coder, bedrock-claude.
 */
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "@/lib/apiFetch";

export type SlotMode = "normal" | "trust" | "yolo";

export interface PersonaPreset {
  name: string;
  provider: string;
  model?: string;
  personality?: string;
  mode: SlotMode;
  description?: string;
}

export interface PersonaListResponse {
  personas: PersonaPreset[];
}

export const PERSONAS_QUERY_KEY = ["personas"] as const;

async function fetchPersonas(): Promise<PersonaListResponse> {
  return apiFetch<PersonaListResponse>("/api/personas");
}

export function usePersonas() {
  return useQuery({
    queryKey: PERSONAS_QUERY_KEY,
    queryFn: fetchPersonas,
    staleTime: 30_000,
  });
}

export function useCreatePersona() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (preset: PersonaPreset) =>
      apiFetch<PersonaPreset>("/api/personas", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(preset),
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: PERSONAS_QUERY_KEY });
    },
  });
}

export function useDeletePersona() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (name: string) => {
      await apiFetch(`/api/personas/${encodeURIComponent(name)}`, {
        method: "DELETE",
        json: false,
      });
      return name;
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: PERSONAS_QUERY_KEY });
    },
  });
}
