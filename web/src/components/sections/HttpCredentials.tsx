import { type FormEvent, useEffect, useState } from "react";
import { Button, Select } from "@/components/ui";
import { HttpError, changeHttpCredential, listHttpCredentials, type HttpCredentialKind, type HttpCredentialRow, type HttpCredentialSource } from "@/lib/api";
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

const setupLinks = {
  local: "",
  open_bao: "https://openbao.org/docs/next/commands/",
  one_password: "https://developer.1password.com/docs/service-accounts/use-with-1password-cli/",
  azure: "https://learn.microsoft.com/cli/azure/authenticate-azure-cli",
  aws: "https://docs.aws.amazon.com/cli/latest/userguide/cli-chap-configure.html",
  google: "https://docs.cloud.google.com/sdk/docs/install",
} satisfies Record<HttpCredentialSource, string>;

export function parseSshDestinations(value: string) {
  return value.split("\n").map(line => line.trim()).filter(Boolean).map(line => {
    const match = /^([^@]+)@(?:\[([0-9a-f:.]+)\]|([^:]+))(?::(\d+))?$/i.exec(line);
    return { username: match?.[1] ?? "", host: match?.[2] ?? match?.[3] ?? "", port: Number(match?.[4] ?? 22) };
  });
}

export default function HttpCredentials({ kind }: { kind: HttpCredentialKind }) {
  const [catalog, setCatalog] = useState<Catalog>();
  const [editing, setEditing] = useState<HttpCredentialRow | "new" | null>(null);
  const [removing, setRemoving] = useState<HttpCredentialRow | null>(null);
  const [header, setHeader] = useState<"bearer" | "x_api_key">("bearer");
  const [source, setSource] = useState<HttpCredentialSource>("local");
  const [linkExisting, setLinkExisting] = useState(false);
  const [nameDraft, setNameDraft] = useState("");
  const [referenceOverride, setReferenceOverride] = useState<string>();
  const prefix = source === "open_bao" ? catalog?.openbao_write_prefix : catalog?.vault_write_prefixes?.[source];
  const canWrite = source !== "local" && !!prefix;
  const field = { totp: "seed", ssh_key: "private_key", api_key: "token", user_password: "password" }[kind];
  const defaultReference = (() => {
    if (editing && editing !== "new") return editing.source === source ? editing.reference ?? "" : "";
    if (!prefix || !/^[A-Za-z0-9_-]+$/.test(nameDraft)) return "";
    if (source === "azure") return prefix + nameDraft.replace(/_/g, "-");
    if (source === "google") return prefix + nameDraft + "/latest";
    return [prefix, nameDraft, source === "aws" ? undefined : field].filter(Boolean).join("/");
  })();

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
    } catch (error) {
      const status = error instanceof HttpError ? error.status : undefined;
      setMessage(t(status === 424 ? "http_credentials.external_partial"
        : status === 409 ? "http_credentials.external_conflict" : "plugin_tls.failed"));
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
    const origin = String(data.get("origin") ?? "").trim();
    const secret = String(data.get("secret"));
    const raw = String(data.get("external_value") ?? "");
    const value = kind === "totp" ? raw.replace(/[\s-]+/g, "").replace(/=+$/, "").toUpperCase()
      : kind === "ssh_key" ? raw.trim() : raw;
    const token = String(data.get("writer_token") ?? "").trim();
    if (canWrite && !linkExisting && ((editing === "new" && !value.trim()) || (value && !token))) {
      setMessage(t("http_credentials.write_required"));
      return;
    }
    const write = canWrite && !linkExisting && value !== "" ? {
      value, token,
      ...(source === "aws" ? {
        aws_access_key_id: String(data.get("aws_access_key_id") ?? "").trim(),
        aws_secret_access_key: String(data.get("aws_secret_access_key") ?? "").trim(),
      } : {}),
    } : undefined;
    const destinations = parseSshDestinations(String(data.get("destinations") ?? ""));
    const auth = kind === "totp"
      ? { kind, seed: source === "local" ? secret.replace(/[\s-]+/g, "").replace(/=+$/, "").toUpperCase() : secret.trim() } as const
      : kind === "ssh_key"
      ? { kind, private_key: secret.trim(), destinations } as const
      : kind === "api_key"
      ? { kind, value: secret, header } as const
      : { kind, username: String(data.get("username")), password: secret } as const;
    form.reset();
    setEditing(null);
    void update({ name, operation: editing === "new" ? "create" : "replace", credential: { origin, source, auth }, vault_write: write });
  }

  const rows = catalog?.credentials.filter((row) => row.kind === kind) ?? [];
  const disabled = busy || !catalog?.encryption_enabled;
  const inputClass = "block w-full rounded-md border border-border bg-background px-3 py-2";
  return (
    <section className="space-y-4">
      <p className="text-sm">{t(kind === "ssh_key" ? "http_credentials.ssh_pending" : kind === "totp" ? "http_credentials.totp_hint" : kind === "user_password" ? "http_credentials.service_hint" : "http_credentials.hint")}</p>
      {message && <p role="status">{message}</p>}
      {!catalog && !message && <p>{t("plugin_tls.loading")}</p>}
      {catalog && !catalog.encryption_enabled && <p>{t("plugin_tls.encryption")}</p>}
      {catalog && rows.length === 0 && <p>{t("http_credentials.empty")}</p>}
      {rows.map((row) => (
        <div key={row.name} className="space-y-2 rounded-md border p-3">
          <p className="break-all font-medium">{row.name}</p>
          <p className="break-all text-sm">{row.origin && `${row.origin.replace(/^service:/, "")} · `}{t(`http_credentials.source_${row.source}`)}</p>
          {row.destinations?.map((destination) => (
            <p key={JSON.stringify(destination)} className="text-sm">{destination.username}@{destination.host}:{destination.port}</p>
          ))}
          <div className="flex flex-wrap gap-2">
            <Button size="sm" disabled={disabled} onClick={() => { setRemoving(null); setHeader(row.header ?? "bearer"); setSource(row.source); setLinkExisting(false); setReferenceOverride(undefined); setEditing(row); }}>
              {t("plugin_tls.replace")}
            </Button>
            <Button size="sm" variant="ghost" disabled={disabled} onClick={() => { setEditing(null); setRemoving(row); }}>
              {t("plugin_tls.delete")}
            </Button>
          </div>
        </div>
      ))}
      <Button disabled={disabled} onClick={() => { setRemoving(null); setHeader("bearer"); setSource("local"); setLinkExisting(false); setNameDraft(""); setReferenceOverride(undefined); setEditing("new"); }}>
        {t("http_credentials.add")}
      </Button>
      {editing && (
        <form key={editing === "new" ? "create" : editing.name} className="max-w-lg space-y-4" onSubmit={save}>
          <label className="block space-y-1">
            <span>{t("http_credentials.name")}</span>
            <input className={inputClass} name="name" required maxLength={64} pattern={"[A-Za-z0-9_\\-]+"} readOnly={editing !== "new"} onChange={e => setNameDraft(e.target.value)} defaultValue={editing === "new" ? "" : editing.name} />
          </label>
          {kind === "ssh_key" ? editing === "new" && (
            <label className="block space-y-1">
              <span>{t("http_credentials.ssh_destinations")}</span>
              <textarea className={inputClass} name="destinations" placeholder="operator@server.example.com:22"
              />
            </label>
          ) : kind !== "totp" && (
          <label className="block space-y-1">
            <span>{t(kind === "user_password" ? "http_credentials.service" : "http_credentials.origin")}</span>
            <input className={inputClass} name="origin" type="url" required maxLength={253}
              placeholder={kind === "user_password" ? "ldaps://directory.example.com:636" : "https://api.example.com"}
              defaultValue={editing === "new" ? "" : editing.origin.replace(/^service:/, "")}
            />
          </label>
          )}
          <div className="space-y-1">
            <label htmlFor="credential-source">{t("http_credentials.source")}</label>
            <Select id="credential-source" value={source}
              aria-label={t("http_credentials.source")}
              onChange={(value) => { setSource(value as HttpCredentialSource); setLinkExisting(false); setReferenceOverride(undefined); }}
              options={Object.keys(sources).map((value) => ({ value, label: t(`http_credentials.source_${value}`) }))}
            />
          </div>
          <details className="space-y-2 rounded-md border p-3 text-sm">
            <summary className="cursor-pointer font-medium">{t("http_credentials.setup_help")}</summary>
            <p>{t("http_credentials.setup_host")}</p>
            <p>{t(`http_credentials.setup_${source}`)}</p>
            {sources[source] && <code className="block break-all">{sources[source]}</code>}
            {setupLinks[source] && (
              <a className="underline" href={setupLinks[source]} target="_blank" rel="noreferrer">
                {t("http_credentials.setup_docs")}
              </a>
            )}
          </details>
          {kind === "api_key" && (
            <div className="space-y-1">
              <label htmlFor="credential-header">{t("http_credentials.header")}</label>
              <Select id="credential-header" value={header}
                aria-label={t("http_credentials.header")}
                onChange={(value) => setHeader(value === "x_api_key" ? "x_api_key" : "bearer")}
                options={[
                  { value: "bearer", label: "Authorization: Bearer" },
                  { value: "x_api_key", label: "X-API-Key" },
                ]}
              />
            </div>
          )}
          {kind === "user_password" && (
            <label className="block space-y-1">
              <span>{t("http_credentials.username")}</span>
              <input className={inputClass} name="username" autoComplete="off" required />
            </label>
          )}
          <label className="block space-y-1">
            <span>{t(source !== "local" ? "http_credentials.reference" : kind === "totp" ? "http_credentials.totp_seed" : kind === "ssh_key" ? "http_credentials.ssh_private_key" : kind === "api_key" ? "http_credentials.token" : "http_credentials.password")}</span>
            {kind === "ssh_key" && source === "local" ? (
              <textarea key={source} className={inputClass} name="secret" required spellCheck={false} autoComplete="off" />
            ) : source === "local" ? (
              <input key={source} className={inputClass} name="secret" type="password" autoComplete="new-password" required />
            ) : (
              <input key={source} className={inputClass} name="secret" value={referenceOverride ?? defaultReference}
                onChange={event => setReferenceOverride(event.target.value)} placeholder={sources[source]} spellCheck={false} autoComplete="off" required />
            )}
          </label>
          {canWrite && editing === "new" && <label className="flex items-center gap-2">
            <input type="checkbox" checked={linkExisting} onChange={event => setLinkExisting(event.target.checked)} />
            {t("http_credentials.link_existing")}
          </label>}
          {canWrite && !linkExisting && <>
            <label className="block space-y-1">
              <span>{t("http_credentials.external_value")}</span>
              {kind === "ssh_key" ? <textarea className={inputClass} name="external_value" spellCheck={false} autoComplete="off" required={editing === "new"} />
                : <input className={inputClass} name="external_value" type="password" autoComplete="new-password" required={editing === "new"} />}
            </label>
            {editing !== "new" && <p className="text-sm">{t("http_credentials.preserve_external")}</p>}
            <label className="block space-y-1">
              <span>{t(source === "aws" ? "http_credentials.aws_session_token" : "http_credentials.writer_token")}</span>
              <input className={inputClass} name="writer_token" type="password" autoComplete="off" required={editing === "new"} />
            </label>
            {source === "aws" && <>
              <label className="block space-y-1">
                <span>{t("http_credentials.aws_access_key")}</span>
                <input className={inputClass} name="aws_access_key_id" autoComplete="off" required={editing === "new"} />
              </label>
              <label className="block space-y-1">
                <span>{t("http_credentials.aws_secret_key")}</span>
                <input className={inputClass} name="aws_secret_access_key" type="password" autoComplete="off" required={editing === "new"} />
              </label>
            </>}
          </>}
          {source !== "local" && !canWrite && <p className="text-sm">{t("http_credentials.reference_only")}</p>}
          <div className="flex flex-wrap gap-2">
            <Button type="submit" disabled={disabled}>{t(editing === "new" ? source !== "local" && (!canWrite || linkExisting) ? "http_credentials.link_existing" : "http_credentials.add" : "plugin_tls.replace")}</Button>
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
