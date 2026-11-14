import { type FormEvent, useEffect, useRef, useState } from "react";
import { t } from "@/lib/i18n";
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
      () => { if (active) setMessage(t("plugin_tls.operator")); },
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
      setMessage(result.configured ? t("plugin_tls.saved") : t("plugin_tls.deleted"));
    } catch {
      setMessage(t("plugin_tls.failed"));
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
      <h2 className="text-lg font-semibold">{t("plugin_tls.title")}</h2>
      {message && <p role="status">{message}</p>}
      {!catalog && !message && <p>{t("plugin_tls.loading")}</p>}
      {catalog && !catalog.encryption_enabled && (
        <p>{t("plugin_tls.encryption")}</p>
      )}
      {catalog?.credentials.length === 0 && (
        <p>{t("plugin_tls.empty")}</p>
      )}
      {catalog?.credentials.map((credential) => (
        <div
          key={JSON.stringify([credential.instance, credential.property])}
          className="space-y-2 rounded-md border p-3"
        >
          <p className="break-all font-medium">{credential.property}</p>
          <p className="break-all text-sm">{credential.instance} · {credential.configured ? t("plugin_tls.configured") : t("plugin_tls.unset")}</p>
          <ul className="space-y-1 text-sm">
            {Object.entries(credential.profiles).map(([profile, hosts]) => (
              <li key={profile} className="break-words">{profile} · {hosts.join(", ")}</li>
            ))}
          </ul>
          <div className="flex flex-wrap gap-2">
            <Button type="button" size="sm" disabled={busy || !catalog.encryption_enabled}
              onClick={() => { clearDraft(); setRemoving(null); setEditing(credential); }}>
              {credential.configured ? t("plugin_tls.replace") : t("plugin_tls.set")}
            </Button>
            {credential.configured && (
              <Button type="button" size="sm" variant="ghost" disabled={busy || !catalog.encryption_enabled}
                onClick={() => { clearDraft(); setRemoving(credential); }}>
                {t("plugin_tls.delete")}
              </Button>
            )}
          </div>
        </div>
      ))}
      {editing && (
        <form className="space-y-2" onSubmit={save}>
          <label className="block" htmlFor="plugin-tls-draft">{t("plugin_tls.new_value").replace("{property}", () => editing.property)}</label>
          <textarea
            className="block w-full rounded-md border p-2 font-mono"
            id="plugin-tls-draft" ref={draft} autoComplete="off" spellCheck={false}
            autoCapitalize="off" rows={4} required maxLength={32768}
            style={{ WebkitTextSecurity: "disc" } as React.CSSProperties}
          />
          <div className="flex gap-2">
            <Button type="submit" disabled={busy}>{t("plugin_tls.save")}</Button>
            <Button type="button" variant="ghost" onClick={clearDraft}>{t("common.cancel")}</Button>
          </div>
        </form>
      )}
      {removing && (
        <div className="space-y-2">
          <p>{t("plugin_tls.confirm_delete").replace("{property}", () => removing.property)}</p>
          <div className="flex gap-2">
            <Button type="button" disabled={busy} onClick={() => {
              setRemoving(null);
              void update(removing, { operation: "delete" });
            }}>{t("plugin_tls.delete_action")}</Button>
            <Button type="button" variant="ghost" onClick={() => setRemoving(null)}>{t("common.cancel")}</Button>
          </div>
        </div>
      )}
    </section>
  );
}
