/**
 * Step 5 — Extras (optional). Computer-use sidecar endpoint, Browserbase
 * cloud browser, and a purely informational vision toggle (multimodal is
 * automatic when the model supports it — nothing to configure).
 */
import { useState } from "react";
import { Eye, Globe, MonitorSmartphone } from "lucide-react";
import { extrasWrites, writeProps } from "@/lib/companionSetup";
import { C, ErrorNote, Field, StepFooter, StepTitle, TextInput } from "./ui";

export default function StepExtras({
  onBack,
  onDone,
}: {
  onBack: () => void;
  onDone: () => void;
}) {
  const [endpoint, setEndpoint] = useState("");
  const [browserbaseApiKey, setBrowserbaseApiKey] = useState("");
  const [browserbaseProjectId, setBrowserbaseProjectId] = useState("");
  const [persistentContext, setPersistentContext] = useState(false);
  const [browserbaseContextId, setBrowserbaseContextId] = useState("");
  const [visionOpen, setVisionOpen] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const submit = async () => {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      const writes = extrasWrites({
        sidecarEndpoint: endpoint,
        browserbaseApiKey,
        browserbaseProjectId,
        browserbaseContextId: persistentContext ? browserbaseContextId : "",
      });
      if (writes.length > 0) await writeProps(writes);
      onDone();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <StepTitle
        kicker="Step 5 — Extras"
        title="Optional superpowers"
        sub="Everything here can be skipped and configured later from the dashboard."
      />

      <div
        style={{
          border: `1px solid ${C.border}`,
          borderRadius: 10,
          background: C.surface,
          padding: "18px 20px",
          marginBottom: 16,
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 8 }}>
          <MonitorSmartphone size={16} color={C.accent} />
          <span style={{ color: C.text, fontSize: 15, fontWeight: 600 }}>
            Computer-use sidecar
          </span>
        </div>
        <p style={{ color: C.muted, fontSize: 13, lineHeight: 1.6, marginBottom: 14 }}>
          If you run a browser/computer-use sidecar, point your companion at
          its actions endpoint so it can click, type and browse on your behalf.
        </p>
        <Field label="Sidecar endpoint" hint="Leave blank to skip. Example: http://127.0.0.1:8787/v1/actions">
          <TextInput
            value={endpoint}
            onChange={(e) => setEndpoint(e.target.value)}
            placeholder="http://127.0.0.1:8787/v1/actions"
            autoFocus
          />
        </Field>
      </div>

      <div
        style={{
          border: `1px solid ${C.border}`,
          borderRadius: 10,
          background: C.surface,
          padding: "18px 20px",
          marginBottom: 16,
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 8 }}>
          <Globe size={16} color={C.accent} />
          <span style={{ color: C.text, fontSize: 15, fontWeight: 600 }}>
            Browserbase
          </span>
        </div>
        <p style={{ color: C.muted, fontSize: 13, lineHeight: 1.6, marginBottom: 14 }}>
          Cloud browser with a stable identity — your companion can browse,
          log in, and keep sessions.
        </p>
        <Field label="API key" hint="Leave blank to skip Browserbase entirely.">
          <TextInput
            type="password"
            value={browserbaseApiKey}
            onChange={(e) => setBrowserbaseApiKey(e.target.value)}
            placeholder="bb_live_..."
            autoComplete="off"
          />
        </Field>
        <Field label="Project ID">
          <TextInput
            value={browserbaseProjectId}
            onChange={(e) => setBrowserbaseProjectId(e.target.value)}
            placeholder="00000000-0000-0000-0000-000000000000"
          />
        </Field>
        <label
          style={{
            display: "flex",
            alignItems: "center",
            gap: 10,
            cursor: "pointer",
            marginBottom: persistentContext ? 10 : 0,
          }}
        >
          <input
            type="checkbox"
            checked={persistentContext}
            onChange={(e) => setPersistentContext(e.target.checked)}
            style={{ accentColor: C.accent, width: 16, height: 16 }}
          />
          <span style={{ color: C.text, fontSize: 13.5 }}>
            Use a persistent context (keeps cookies &amp; logins between sessions)
          </span>
        </label>
        {persistentContext ? (
          <Field label="Context ID" hint="A Browserbase context id created for this companion.">
            <TextInput
              value={browserbaseContextId}
              onChange={(e) => setBrowserbaseContextId(e.target.value)}
              placeholder="ctx_..."
            />
          </Field>
        ) : null}
      </div>

      <div
        style={{
          border: `1px solid ${C.border}`,
          borderRadius: 10,
          background: C.surface,
          padding: "18px 20px",
        }}
      >
        <label
          style={{
            display: "flex",
            alignItems: "center",
            gap: 10,
            cursor: "pointer",
          }}
        >
          <input
            type="checkbox"
            checked={visionOpen}
            onChange={(e) => setVisionOpen(e.target.checked)}
            style={{ accentColor: C.accent, width: 16, height: 16 }}
          />
          <Eye size={16} color={C.accent} />
          <span style={{ color: C.text, fontSize: 15, fontWeight: 600 }}>Vision</span>
        </label>
        {visionOpen ? (
          <p style={{ color: C.muted, fontSize: 13, lineHeight: 1.6, marginTop: 10, marginBottom: 0 }}>
            Nothing to set up — vision is automatic. When your model supports
            images, anything you show the camera (or attach in chat) rides
            along with your words. Up to 4 images, 5&nbsp;MB each, per message.
          </p>
        ) : null}
      </div>

      {error ? <ErrorNote message={`Saving extras failed — ${error}`} /> : null}

      <StepFooter
        onBack={onBack}
        busy={busy}
        onSkip={onDone}
        skipLabel="Skip extras"
        continueLabel="Continue"
      />
    </form>
  );
}
