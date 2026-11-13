import { type FormEvent, useEffect, useState } from "react";
import { Button } from "@/components/ui";
import { changeHttpCredential, listHttpCredentials, type HttpCredentialKind, type HttpCredentialRow, type HttpCredentialSource } from "@/lib/api";
import { t } from "@/lib/i18n";

type Catalog = Awaited<ReturnType<typeof listHttpCredentials>>;

const sources = {
  local: "",
  open_bao: "secret/team/service/password",
  one_password: "op://Vault/Item/password",
  azure: "vault-name/secret-name",
  aws: "us-east-1/team/service-token",
  google: "project-id/secret-name/latest",
} satisfies Record<HttpCredentialSource, string>;

export default function HttpCredentials({ kind }: { kind: HttpCredentialKind }) {
  const [catalog, setCatalog] = useState<Catalog>();
  const [editing, setEditing] = useState<HttpCredentialRow | "new" | null>(null);
  const [removing, setRemoving] = useState<HttpCredentialRow | null>(null);
  const [source, setSource] = useState<HttpCredentialSource>("local");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");

  useEffect(() => {
    let active = true;
    void listHttpCredentials().then(
      (result) => { if (active) setCatalog(result); },
      () => { if (active) setMessage(t("plugin_tls.operator")); },
    );
    return () => { active = false; };
  }, []);

  async function update(mutation: Parameters<typeof changeHttpCredential>[0]) {
    if (busy) return;
    setBusy(true);
    setMessage("");
    try {
      await changeHttpCredential(mutation);
      setCatalog(await listHttpCredentials());
      setMessage(t(mutation.operation === "delete" ? "plugin_tls.deleted" : "plugin_tls.saved"));
    } catch {
      setMessage(t("plugin_tls.failed"));
    } finally {
      setBusy(false);
    }
  }

  function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!editing || busy) return;
    const form = event.currentTarget;
    const data = new FormData(form);
    const name = editing === "new" ? String(data.get("name")) : editing.name;
    const origin = String(data.get("origin"));
    const secret = String(data.get("secret"));
    const auth = kind === "api_key"
      ? { kind, value: secret, header: data.get("header") === "x_api_key" ? "x_api_key" : "bearer" } as const
      : { kind, username: String(data.get("username")), password: secret } as const;
    form.reset();
    setEditing(null);
    void update({ name, operation: editing === "new" ? "create" : "replace", credential: { origin, source, auth } });
  }

  const rows = catalog?.credentials.filter((row) => row.kind === kind) ?? [];
  const disabled = busy || !catalog?.encryption_enabled;
  const inputClass = "block w-full rounded-md border border-border bg-background px-3 py-2";
  return (
    <section className="space-y-4">
      <p className="text-sm">{t("http_credentials.hint")}</p>
      {message && <p role="status">{message}</p>}
      {!catalog && !message && <p>{t("plugin_tls.loading")}</p>}
      {catalog && !catalog.encryption_enabled && <p>{t("plugin_tls.encryption")}</p>}
      {catalog && rows.length === 0 && <p>{t("http_credentials.empty")}</p>}
      {rows.map((row) => (
        <div key={row.name} className="space-y-2 rounded-md border p-3">
          <p className="break-all font-medium">{row.name}</p>
          <p className="break-all text-sm">{row.origin} · {t(`http_credentials.source_${row.source}`)}</p>
          <div className="flex flex-wrap gap-2">
            <Button size="sm" disabled={disabled} onClick={() => { setRemoving(null); setSource(row.source); setEditing(row); }}>
              {t("plugin_tls.replace")}
            </Button>
            <Button size="sm" variant="ghost" disabled={disabled} onClick={() => { setEditing(null); setRemoving(row); }}>
              {t("plugin_tls.delete")}
            </Button>
          </div>
        </div>
      ))}
      <Button disabled={disabled} onClick={() => { setRemoving(null); setSource("local"); setEditing("new"); }}>
        {t("http_credentials.add")}
      </Button>
      {editing && (
        <form key={editing === "new" ? "create" : editing.name} className="max-w-lg space-y-4" onSubmit={save}>
          <label className="block space-y-1">
            <span>{t("http_credentials.name")}</span>
            <input className={inputClass} name="name" required maxLength={64} pattern={"[A-Za-z0-9_\\-]+"} readOnly={editing !== "new"} defaultValue={editing === "new" ? "" : editing.name} />
          </label>
          <label className="block space-y-1">
            <span>{t("http_credentials.origin")}</span>
            <input className={inputClass} name="origin" type="url" required placeholder="https://api.example.com" defaultValue={editing === "new" ? "" : editing.origin} />
          </label>
          <label className="block space-y-1">
            <span>{t("http_credentials.source")}</span>
            <select className={inputClass} value={source} onChange={(event) => {
              if (event.currentTarget.value in sources) {
                setSource(event.currentTarget.value as HttpCredentialSource);
              }
            }}>
              {(Object.keys(sources) as HttpCredentialSource[]).map((key) => (
                <option key={key} value={key}>{t(`http_credentials.source_${key}`)}</option>
              ))}
            </select>
          </label>
          {kind === "api_key" && (
            <label className="block space-y-1">
              <span>{t("http_credentials.header")}</span>
              <select className={inputClass} name="header">
                <option value="bearer">Authorization: Bearer</option>
                <option value="x_api_key">X-API-Key</option>
              </select>
            </label>
          )}
          {kind === "user_password" && (
            <label className="block space-y-1">
              <span>{t("http_credentials.username")}</span>
              <input className={inputClass} name="username" autoComplete="off" required />
            </label>
          )}
          <label className="block space-y-1">
            <span>{t(source === "local" ? (kind === "api_key" ? "http_credentials.token" : "http_credentials.password") : "http_credentials.reference")}</span>
            <input key={source} className={inputClass} name="secret" type={source === "local" ? "password" : "text"} placeholder={sources[source] || undefined} autoComplete="new-password" required />
          </label>
          <div className="flex flex-wrap gap-2">
            <Button type="submit" disabled={disabled}>{t(editing === "new" ? "http_credentials.add" : "plugin_tls.replace")}</Button>
            <Button type="button" variant="ghost" onClick={() => setEditing(null)}>{t("common.cancel")}</Button>
          </div>
        </form>
      )}
      {removing && (
        <div className="flex flex-wrap items-center gap-2">
          <span>{removing.name}</span>
          <Button disabled={disabled} onClick={() => {
            setRemoving(null);
            void update({ name: removing.name, operation: "delete" });
          }}>{t("plugin_tls.delete")}</Button>
          <Button variant="ghost" onClick={() => setRemoving(null)}>{t("common.cancel")}</Button>
        </div>
      )}
    </section>
  );
}
