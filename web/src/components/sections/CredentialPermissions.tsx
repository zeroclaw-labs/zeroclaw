import { useEffect, useState } from "react";
import { Button, Select } from "@/components/ui";
import { HttpError, changeHttpCredential, listHttpCredentials, type HttpCredentialRow } from "@/lib/api";
import { t } from "@/lib/i18n";
import { parseSshDestinations } from "./HttpCredentials";

type Consumer = "http" | "browser" | "ssh";
type Grant = { alias: string; consumer: Consumer; destination: string };

const consumerLabel: Record<Consumer, string> = { http: "HTTP Request", browser: "Browser", ssh: "SSH" };

function consumers(row?: HttpCredentialRow): Consumer[] {
  if (!row) return [];
  if (row.kind === "ssh_key") return ["ssh"];
  if (row.kind === "totp") return ["browser"];
  return row.kind === "user_password" ? ["http", "browser"] : ["http"];
}

function grantsFor(row: HttpCredentialRow): Grant[] {
  const entry = (consumer: Consumer, destination: string): Grant => ({ alias: row.name, consumer, destination });
  return [
    ...(row.http_origins ?? (row.http_allowed ? [row.origin] : [])).map(origin => entry("http", origin)),
    ...(row.browser_origins ?? (row.browser_origin ? [row.browser_origin] : [])).map(origin => entry("browser", origin)),
    ...(row.destinations ?? []).map(d => entry("ssh", `${d.username}@${d.host.includes(":") ? `[${d.host}]` : d.host}:${d.port}`)),
  ];
}

export default function CredentialPermissions() {
  const [rows, setRows] = useState<HttpCredentialRow[]>([]);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [draft, setDraft] = useState<Grant>();
  const [editing, setEditing] = useState<Grant>();
  useEffect(() => {
    let active = true;
    void listHttpCredentials().then(
      (result) => { if (active) setRows(result.credentials); },
      () => { if (active) setMessage(t("plugin_tls.operator")); },
    );
    return () => { active = false; };
  }, []);

  const row = rows.find(item => item.name === draft?.alias);
  const eligible = rows.filter(item => consumers(item).length > 0);
  const grants = rows.flatMap(grantsFor);

  function select(alias: string) {
    const selected = rows.find(item => item.name === alias);
    const consumer = consumers(selected)[0];
    if (selected && consumer) setDraft({ alias, consumer, destination: selected.origin });
  }

  async function save(entry: Grant, replaced?: Grant, remove = false) {
    const current = rows.find(item => item.name === entry.alias);
    if (busy || !current) return;
    setBusy(true);
    setMessage("");
    try {
      const siblings = grants.filter(item => item.alias === entry.alias && item.consumer === entry.consumer &&
        item.destination !== (remove ? entry.destination : replaced?.destination)).map(item => item.destination);
      if (!remove) siblings.push(entry.destination.trim());
      const destinations = [...new Set(siblings)];
      const permissions = entry.consumer === "http" ? { http_origins: destinations }
        : entry.consumer === "browser" ? { browser: { origins: destinations } }
        : { destinations: parseSshDestinations(destinations.join("\n")) };
      if (entry.consumer === "ssh" && "destinations" in permissions && permissions.destinations?.length !== destinations.length) {
        setMessage(t("plugin_tls.failed"));
        return;
      }
      await changeHttpCredential({
        name: entry.alias,
        operation: "replace",
        permissions: { ...permissions, expected_revision: current.permission_revision },
      });
      setRows((await listHttpCredentials()).credentials);
      setDraft(undefined);
      setEditing(undefined);
      setMessage(t("plugin_tls.saved"));
    } catch (error) {
      if (error instanceof HttpError && error.status === 409) {
        setDraft(undefined);
        setEditing(undefined);
        await listHttpCredentials().then(
          result => setRows(result.credentials),
          () => setRows([]),
        );
      }
      setMessage(t("plugin_tls.failed"));
    } finally {
      setBusy(false);
    }
  }

  const field = "block w-full rounded-md border border-border bg-background p-2";
  return (
    <section className="space-y-4">
      {message && <p role="status">{message}</p>}
      <Button type="button" disabled={busy || !eligible.length} onClick={() => {
        setEditing(undefined);
        if (eligible[0]) select(eligible[0].name);
      }}>{t("http_credentials.add_permission")}</Button>
      {draft && row && (
        <form className="space-y-4 rounded-md border p-4" onSubmit={event => {
          event.preventDefault();
          void save(draft, editing);
        }}>
          <label className="block space-y-1">
            <span>{t("http_credentials.name")}</span>
            {editing ? <span className={field}>{draft.alias}</span> : <Select value={draft.alias} aria-label={t("http_credentials.name")}
              onChange={select} options={eligible.map(item => ({ value: item.name, label: item.name }))} />}
          </label>
          <label className="block space-y-1">
            <span>{t("http_credentials.consumer")}</span>
            {editing ? <span className={field}>{consumerLabel[draft.consumer]}</span> : <Select value={draft.consumer} aria-label={t("http_credentials.consumer")}
              onChange={value => setDraft({ ...draft, consumer: value as Consumer, destination: row.origin })}
              options={consumers(row).map(consumer => ({ value: consumer, label: consumerLabel[consumer] }))} />}
          </label>
          <label className="block space-y-1">
            <span>{t(draft.consumer === "ssh" ? "http_credentials.ssh_destinations" : "http_credentials.origin")}</span>
            <input className={field} type={draft.consumer === "ssh" ? "text" : "url"} required
              placeholder={draft.consumer === "ssh" ? "operator@server.example.com:22" : "https://login.example.com"}
              value={draft.destination} onChange={event => setDraft({ ...draft, destination: event.target.value })} />
          </label>
          <div className="flex gap-2">
            <Button type="submit" disabled={busy}>{t(editing ? "http_credentials.save_permissions" : "http_credentials.add_permission")}</Button>
            <Button type="button" disabled={busy} onClick={() => { setDraft(undefined); setEditing(undefined); }}>{t("http_credentials.cancel_permission")}</Button>
          </div>
        </form>
      )}
      {grants.length === 0 && <p>{t("http_credentials.no_permissions")}</p>}
      <ul className="space-y-2">
        {grants.map(entry => (
          <li key={JSON.stringify(entry)} className="flex flex-wrap items-center justify-between gap-3 rounded-md border p-4">
            <div className="min-w-0 break-words">
              <p className="font-medium">{entry.alias}</p>
              <p>{consumerLabel[entry.consumer]} · {entry.destination}</p>
            </div>
            <div className="flex gap-2">
              <Button type="button" disabled={busy} onClick={() => { setDraft(entry); setEditing(entry); }}>{t("http_credentials.edit_permission")}</Button>
              <Button type="button" disabled={busy} onClick={() => void save(entry, undefined, true)}>{t("http_credentials.remove_permission")}</Button>
            </div>
          </li>
        ))}
      </ul>
    </section>
  );
}
