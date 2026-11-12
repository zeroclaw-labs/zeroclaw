import { type FormEvent, useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui";
import {
  changePluginTlsCredential,
  listPluginTlsCredentials,
  type PluginTlsCredential,
} from "@/lib/api";

type Catalog = Awaited<ReturnType<typeof listPluginTlsCredentials>>;

export default function PluginTlsCredentials() {
  const [catalog, setCatalog] = useState<Catalog | null>(null);
  const [editing, setEditing] = useState<PluginTlsCredential | null>(null);
  const [removing, setRemoving] = useState<PluginTlsCredential | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const draft = useRef<HTMLTextAreaElement>(null);
  const inFlight = useRef(false);

  useEffect(() => {
    let active = true;
    void listPluginTlsCredentials().then(
      (result) => { if (active) setCatalog(result); },
      () => { if (active) setMessage("Plugin TLS credentials are available to authenticated operators."); },
    );
    return () => { active = false; };
  }, []);

  function clearDraft() {
    if (draft.current) draft.current.value = "";
    setEditing(null);
  }

  async function update(
    credential: PluginTlsCredential,
    mutation: Parameters<typeof changePluginTlsCredential>[1],
  ) {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setMessage("");
    try {
      const result = await changePluginTlsCredential(credential, mutation);
      setCatalog((current) => current && ({
        ...current,
        credentials: current.credentials.map((row) =>
          row.instance === credential.instance && row.property === credential.property
            ? { ...row, configured: result.configured }
            : row,
        ),
      }));
      setMessage(result.configured ? "Credential saved." : "Credential deleted.");
    } catch {
      setMessage("The change could not be confirmed. Refresh before trying again.");
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  }

  function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!editing || !draft.current || !draft.current.value.trim()) return;
    const value = draft.current.value;
    const operation = editing.configured ? "replace" : "create";
    clearDraft();
    void update(editing, { operation, value });
  }

  return (
    <section className="space-y-4">
      <h2 className="text-lg font-semibold">Plugin TLS credentials</h2>
      {message && <p role="status">{message}</p>}
      {!catalog && !message && <p>Loading credentials…</p>}
      {catalog && !catalog.encryption_enabled && (
        <p>Enable secret encryption to change credentials.</p>
      )}
      {catalog?.credentials.length === 0 && (
        <p>Configure a TLS profile for a plugin instance to add a credential here. An initialized secret store is required to save changes.</p>
      )}
      {catalog?.credentials.map((credential) => (
        <div
          key={JSON.stringify([credential.instance, credential.property])}
          className="space-y-2 rounded-md border p-3"
        >
          <p className="break-all font-medium">{credential.property}</p>
          <p className="break-all text-sm">{credential.instance} · {credential.configured ? "Configured" : "Unset"}</p>
          <ul className="space-y-1 text-sm">
            {Object.entries(credential.profiles).map(([profile, hosts]) => (
              <li key={profile} className="break-words">{profile} · {hosts.join(", ")}</li>
            ))}
          </ul>
          <div className="flex flex-wrap gap-2">
            <Button type="button" size="sm" disabled={busy || !catalog.encryption_enabled}
              onClick={() => { clearDraft(); setRemoving(null); setEditing(credential); }}>
              {credential.configured ? "Replace" : "Set"}
            </Button>
            {credential.configured && (
              <Button type="button" size="sm" variant="ghost" disabled={busy || !catalog.encryption_enabled}
                onClick={() => { clearDraft(); setRemoving(credential); }}>
                Delete
              </Button>
            )}
          </div>
        </div>
      ))}
      {editing && (
        <form className="space-y-2" onSubmit={save}>
          <label className="block" htmlFor="plugin-tls-draft">New value for {editing.property}</label>
          <textarea
            className="block w-full rounded-md border p-2 font-mono"
            id="plugin-tls-draft" ref={draft} autoComplete="off" spellCheck={false}
            autoCapitalize="off" rows={4} required maxLength={32768}
            style={{ WebkitTextSecurity: "disc" } as React.CSSProperties}
          />
          <div className="flex gap-2">
            <Button type="submit" disabled={busy}>Save credential</Button>
            <Button type="button" variant="ghost" onClick={clearDraft}>Cancel</Button>
          </div>
        </form>
      )}
      {removing && (
        <div className="space-y-2">
          <p>Delete {removing.property}? Its TLS profiles may stop connecting. Existing backups and upstream credentials require separate management.</p>
          <div className="flex gap-2">
            <Button type="button" disabled={busy} onClick={() => {
              setRemoving(null);
              void update(removing, { operation: "delete" });
            }}>Delete credential</Button>
            <Button type="button" variant="ghost" onClick={() => setRemoving(null)}>Cancel</Button>
          </div>
        </div>
      )}
    </section>
  );
}
