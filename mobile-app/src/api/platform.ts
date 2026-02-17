import { config } from "../config";
import { addDemoProject, demoGallery, getDemoProjects } from "../dev/demoData";
import { log } from "../logger";

type HttpMethod = "GET" | "POST" | "PUT" | "PATCH" | "DELETE";

async function requestJson<T>(method: HttpMethod, path: string, body?: unknown, timeoutMs: number = 20000): Promise<T> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), timeoutMs);

  try {
    const res = await fetch(`${config.platformUrl}${path}`, {
      method,
      headers: {
        "Content-Type": "application/json"
      },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: controller.signal
    });

    const text = await res.text();
    const json = text ? (JSON.parse(text) as unknown) : null;

    if (!res.ok) {
      const detail = (json as any)?.detail;
      const message = typeof detail === "string" ? detail : `HTTP ${res.status}`;
      throw new Error(message);
    }

    return json as T;
  } finally {
    clearTimeout(timeout);
  }
}

export type Project = {
  id: string;
  name: string;
  visibility: string;
  template: string;
  theme: string;
  latest_snapshot_id: string | null;
  runtime_status?: string | null;
  runtime_mode?: string | null;
  runtime_deployment?: string | null;
  runtime_project_ref?: string | null;
  execution_mode?: "quick_preview" | "gallery_preview" | "full_preview" | "production" | null;
  runtime_supabase_url?: string | null;
  subscription_tier?: "free" | "starter" | "premium" | null;
  export_source_enabled?: boolean;
  agent_total_input_tokens?: number;
  agent_total_output_tokens?: number;
  agent_total_cost_usd?: number;
  agent_todo_summary?: string | null;
};

export type ProjectCreate = {
  name: string;
  visibility?: string;
  template?: string;
  theme?: string;
};

export type GalleryItem = {
  project_id: string;
  name: string;
  template: string;
  theme: string;
  release_id?: string | null;
  published_at?: string | null;
};

export type GenerateRequest = {
  text?: string;
  voice_text?: string | null;
  photo_url?: string | null;
  execution_mode?: "quick_preview" | "gallery_preview" | "full_preview" | "production" | null;
};

export type GenerateResponse = {
  status: "ok";
  snapshot_id: string;
  preview?: {
    manifest_url?: string;
    bundle_url?: string;
  };
};

export type PreviewResponse = {
  project_id: string;
  snapshot_id: string;
  deeplink: string;
  manifest_url: string;
  release_id?: string | null;
  ready?: boolean;
  publish_state?: "ready" | "in_progress" | "failed" | null;
  publish_error?: string | null;
};

export async function getProjects(): Promise<Project[]> {
  if (config.demoMode) return getDemoProjects();
  return requestJson<Project[]>("GET", "/projects");
}

export async function getProject(projectId: string): Promise<Project> {
  if (config.demoMode) {
    const p = getDemoProjects().find((x) => x.id === projectId);
    if (!p) throw new Error("Project not found");
    return p;
  }
  return requestJson<Project>("GET", `/projects/${encodeURIComponent(projectId)}`);
}

export async function createProject(input: ProjectCreate): Promise<Project> {
  if (config.demoMode) {
    const created: Project = {
      id: `demo_${Date.now()}`,
      name: input.name,
      visibility: input.visibility ?? "private",
      template: input.template ?? "Base Mobile App",
      theme: input.theme ?? "Tech",
      latest_snapshot_id: null
    };
    addDemoProject(created);
    return created;
  }
  return requestJson<Project>("POST", "/projects", input);
}

export async function generateProject(projectId: string, input: GenerateRequest): Promise<GenerateResponse> {
  if (config.demoMode) {
    return {
      status: "ok",
      snapshot_id: `snap_demo_${Math.floor(Math.random() * 1000)}`,
      preview: { manifest_url: "", bundle_url: "" }
    };
  }
  return requestJson<GenerateResponse>("POST", `/projects/${encodeURIComponent(projectId)}/generate`, input);
}

export async function getPreview(projectId: string): Promise<PreviewResponse> {
  if (config.demoMode) {
    return {
      project_id: projectId,
      snapshot_id: "snap_demo",
        deeplink: `guappa-preview://preview/${projectId}?channel=preview`,
      manifest_url: ""
    };
  }
  return requestJson<PreviewResponse>("GET", `/projects/${encodeURIComponent(projectId)}/preview`);
}

export async function publishPreview(projectId: string): Promise<{ status: "ok"; snapshot_id: string; deeplink: string; channel?: string | null; release_id?: string | null }> {
  return requestJson<{ status: "ok"; snapshot_id: string; deeplink: string; channel?: string | null; release_id?: string | null }>(
    "POST",
    `/projects/${encodeURIComponent(projectId)}/publish-preview`,
    undefined,
    180000
  );
}

// ---------------------------------------------------------------------------
// Uploads
// ---------------------------------------------------------------------------

export type UploadAsset = {
  id: string;
  filename: string;
  stored_name: string;
  rel_path: string;
  mime: string;
  size: number;
  label: string;
  created_at: string;
};

export async function uploadProjectFile(projectId: string, fileUri: string, label?: string): Promise<UploadAsset> {
  const ext = fileUri.split("?")[0].split("#")[0].split(".").pop()?.toLowerCase() ?? "jpg";
  const typeByExt: Record<string, string> = {
    jpg: "image/jpeg",
    jpeg: "image/jpeg",
    png: "image/png",
    webp: "image/webp",
    heic: "image/heic",
    gif: "image/gif",
    pdf: "application/pdf",
  };
  const mimeType = typeByExt[ext] ?? "application/octet-stream";
  const filename = `upload.${ext}`;

  const formData = new FormData();
  formData.append("file", { uri: fileUri, type: mimeType, name: filename } as any);
  if (label) formData.append("label", label);

  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 45000);
  try {
    const res = await fetch(`${config.platformUrl}/projects/${encodeURIComponent(projectId)}/uploads`, {
      method: "POST",
      body: formData,
      signal: controller.signal
    });
    const raw = await res.text();
    const json = raw ? (JSON.parse(raw) as any) : null;
    if (!res.ok) {
      throw new Error(json?.detail ?? `HTTP ${res.status}`);
    }
    return json.asset as UploadAsset;
  } finally {
    clearTimeout(timeout);
  }
}

export async function getGallery(): Promise<GalleryItem[]> {
  if (config.demoMode) return demoGallery;
  return requestJson<GalleryItem[]>("GET", "/gallery");
}

export async function publishProject(projectId: string): Promise<GalleryItem> {
  if (config.demoMode) {
    return {
      project_id: projectId,
      name: "Demo Publish",
      template: "Base Mobile App",
      theme: "Tech"
    };
  }
  return requestJson<GalleryItem>("POST", `/projects/${encodeURIComponent(projectId)}/publish`);
}

export async function forkProject(projectId: string): Promise<{ status: "ok"; project_id: string }> {
  if (config.demoMode) {
    const forked: Project = {
      id: `demo_fork_${Date.now()}`,
      name: "Forked Project",
      visibility: "private",
      template: "Base Mobile App",
      theme: "Tech",
      latest_snapshot_id: "snap_demo_fork"
    };
    addDemoProject(forked);
    return { status: "ok", project_id: forked.id };
  }
  return requestJson<{ status: "ok"; project_id: string }>("POST", `/projects/${encodeURIComponent(projectId)}/fork`);
}

export async function joinWaitlist(email: string): Promise<{ status: "ok" }> {
  return requestJson<{ status: "ok" }>("POST", "/waitlist", { email });
}

export function exportAppSourceUrl(projectId: string): string {
  return `${config.platformUrl}/projects/${encodeURIComponent(projectId)}/exports/app`;
}

export function exportRuntimeSourceUrl(projectId: string): string {
  return `${config.platformUrl}/projects/${encodeURIComponent(projectId)}/exports/runtime`;
}

export async function setProjectSubscriptionTier(
  projectId: string,
  tier: "free" | "starter" | "premium"
): Promise<{ status: "ok"; project_id: string; subscription_tier: "free" | "starter" | "premium" }> {
  return requestJson("POST", `/projects/${encodeURIComponent(projectId)}/entitlements/subscription-tier?tier=${encodeURIComponent(tier)}`);
}

export async function setProjectExecutionMode(
  projectId: string,
  mode: "quick_preview" | "gallery_preview" | "full_preview" | "production"
): Promise<{ status: "ok"; project_id: string; execution_mode: "quick_preview" | "gallery_preview" | "full_preview" | "production" }> {
  return requestJson("POST", `/projects/${encodeURIComponent(projectId)}/execution-mode?mode=${encodeURIComponent(mode)}`);
}

// ---------------------------------------------------------------------------
// Voice / STT
// ---------------------------------------------------------------------------

/**
 * Upload a recorded audio file for transcription via the backend proxy.
 * The backend forwards the file to Deepgram and returns the transcript.
 */
export async function transcribeAudio(fileUri: string): Promise<string> {
  // Transcription should work regardless of demoMode.

  const ext = fileUri.split("?")[0].split("#")[0].split(".").pop()?.toLowerCase() ?? "m4a";
  const typeByExt: Record<string, string> = {
    m4a: "audio/mp4",
    mp4: "audio/mp4",
    wav: "audio/wav",
    webm: "audio/webm",
    ogg: "audio/ogg",
    flac: "audio/flac",
    aac: "audio/aac",
    "3gp": "audio/3gpp",
    "3gpp": "audio/3gpp",
  };
  const mimeType = typeByExt[ext] ?? "audio/mp4";
  const filename = `recording.${ext}`;

  const formData = new FormData();
  formData.append("audio", {
    uri: fileUri,
    type: mimeType,
    name: filename
  } as any);

  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 30000);

  try {
    log("debug", "transcribeAudio: request", { url: `${config.platformUrl}/voice/transcribe`, filename, mimeType });
    const res = await fetch(`${config.platformUrl}/voice/transcribe`, {
      method: "POST",
      body: formData,
      signal: controller.signal
    });

    const rawText = await res.text();
    let json: any = null;
    try {
      json = rawText ? JSON.parse(rawText) : null;
    } catch {
      // ignore
    }
    if (!res.ok) {
      log("error", "transcribeAudio: backend error", { status: res.status, body: rawText });
      throw new Error(json?.detail ?? `HTTP ${res.status}`);
    }
    const text = String(json?.text ?? "");
    log("debug", "transcribeAudio: ok", { chars: text.length });
    return text;
  } finally {
    clearTimeout(timeout);
  }
}

/** @deprecated Use transcribeAudio(uri) instead – kept for backward compat. */
export async function transcribeVoice(): Promise<{ text: string }> {
  throw new Error("transcribeVoice() is deprecated. Use live WS STT or transcribeAudio(uri). ");
}
