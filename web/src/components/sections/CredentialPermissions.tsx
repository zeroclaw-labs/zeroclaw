import { useEffect, useState } from "react";
import { Button, Select } from "@/components/ui";
import { HttpError, changeHttpCredential, listHttpCredentials, type HttpCredentialRow, type PortableSshMode, type SshAuthProfile } from "@/lib/api";
import { t } from "@/lib/i18n";
import { parseSshDestinations } from "./HttpCredentials";

type Consumer = "http" | "browser" | "ssh";
type Grant = { alias: string; consumer: Consumer; destination: string; sequence?: PortableSshMode; totpAlias?: string; sshProfile?: boolean };

const consumerLabel: Record<Consumer, string> = { http: "HTTP Request", browser: "Browser", ssh: "SSH" };

const keyModes: [PortableSshMode, ...PortableSshMode[]] = ["key", "key_then_totp"];
const passwordModes: [PortableSshMode, ...PortableSshMode[]] = ["password", "password_then_totp", "totp_then_password", "password_and_totp"];
const requiresTotp = (mode: PortableSshMode) => mode !== "key" && mode !== "password";
const destinationText = (d: SshAuthProfile["destination"]) =>
  `${d.username}@${d.host.includes(":") ? `[${d.host}]` : d.host}:${d.port}`;

function consumers(row?: HttpCredentialRow): Consumer[] {
  if (!row) return [];
  if (row.kind === "ssh_key") return ["ssh"];
  if (row.kind === "totp") return ["browser"];
  return row.kind === "user_password" ? ["http", "browser", "ssh"] : ["http"];
}

function grantsFor(row: HttpCredentialRow): Grant[] {
  const entry = (consumer: Consumer, destination: string): Grant => ({ alias: row.name, consumer, destination });
  return [
    ...(row.http_origins ?? (row.http_allowed ? [row.origin] : [])).map(origin => entry("http", origin)),
    ...(row.browser_origins ?? (row.browser_origin ? [row.browser_origin] : [])).map(origin => entry("browser", origin)),
    ...(row.destinations ?? []).filter(d => !row.ssh_profiles?.some(profile =>
      destinationText(d) === destinationText(profile.destination))).map(d => entry("ssh", destinationText(d))),
    ...(row.ssh_profiles ?? []).map(profile => ({
      ...entry("ssh", destinationText(profile.destination)),
      sshProfile: true,
      ...(profile.authentication.mode === "portable"
        ? { sequence: profile.authentication.sequence, totpAlias: profile.authentication.totp_alias }
        : {}),
    })),
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
      let permissions;
      if (entry.consumer === "ssh" && (!remove || entry.sshProfile)) {
        const sequence = entry.sequence ?? (current.kind === "ssh_key" ? "key" : "password");
        const changing = remove ? entry : replaced;
        const profiles = (current.ssh_profiles ?? []).filter(profile =>
          !changing || destinationText(profile.destination) !== changing.destination);
        if (!remove) {
          const inputs = entry.destination.split(/[\r\n]+/).map(value => value.trim()).filter(Boolean);
          const requested = parseSshDestinations(inputs.map(value =>
            !value.includes("@") && current.ssh_username ? `${current.ssh_username}@${value}` : value).join("\n"));
          if (!requested.length || requested.some(d => !d.host || !d.username || d.port < 1 || d.port > 65535)
            || (requiresTotp(sequence) && !entry.totpAlias)) {
            setMessage(t("plugin_tls.failed"));
            return;
          }
          profiles.push(...requested.map(destination => ({
            destination, authentication: { mode: "portable" as const, sequence, primary_alias: entry.alias,
              totp_alias: requiresTotp(sequence) ? entry.totpAlias : undefined },
          })));
        }
        permissions = { ssh_profiles: profiles };
      } else {
        permissions = entry.consumer === "http" ? { http_origins: destinations }
          : entry.consumer === "browser" ? { browser: { origins: destinations } }
          : { destinations: parseSshDestinations(siblings.filter(destination =>
              grants.some(item => item.alias === entry.alias && item.consumer === "ssh"
                && !item.sshProfile && item.destination === destination)).join("\n")) };
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
      <div className="flex flex-wrap gap-2">
        <Button type="button" disabled={busy || !eligible.length} onClick={() => {
          setEditing(undefined);
          if (eligible[0]) select(eligible[0].name);
        }}>{t("http_credentials.add_permission")}</Button>
      </div>
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
              onChange={value => setDraft({ ...draft, consumer: value as Consumer, destination: value === "ssh" ? "" : row.origin })}
              options={consumers(row).map(consumer => ({ value: consumer, label: consumerLabel[consumer] }))} />}
          </label>
          <label className="block space-y-1">
            <span>{t(draft.consumer === "ssh" ? "ssh_authorization.destinations" : "http_credentials.origin")}</span>
            {draft.consumer === "ssh" ? <textarea className={field} required rows={3}
              placeholder={row.kind === "ssh_key" ? "operator@server.example.com:22" : "server.example.com"}
              value={draft.destination} onChange={event => setDraft({ ...draft, destination: event.target.value })} />
              : <input className={field} type="url" required placeholder="https://login.example.com"
                value={draft.destination} onChange={event => setDraft({ ...draft, destination: event.target.value })} />}
          </label>
          {draft.consumer === "ssh" && (() => {
            const modes = row.kind === "ssh_key" ? keyModes : passwordModes;
            const mode = draft.sequence ?? modes[0];
            const label = t("ssh_authorization.mode");
            const totp = t("ssh_authorization.totp_alias");
            return <>
              <p className="text-sm text-pc-text-muted">{t("ssh_authorization.destinations_help")}</p>
              <details className="space-y-2 rounded-md border p-3 text-sm">
                <summary className="cursor-pointer font-medium">{t("ssh_authorization.help_title")}</summary>
                <p>{t("ssh_authorization.help_body")}</p>
              </details>
              <label className="block space-y-1"><span>{label}</span>
                <Select aria-label={label} value={mode} onChange={value => setDraft({ ...draft, sequence: value as PortableSshMode })}
                  options={modes.map(value => ({ value, label: t(`ssh_authorization.mode_${value}`) }))} />
              </label>
              {requiresTotp(mode) && <label className="block space-y-1"><span>{totp}</span>
                <Select aria-label={totp} value={draft.totpAlias ?? ""} placeholder={totp}
                  onChange={value => setDraft({ ...draft, totpAlias: value })}
                  options={rows.filter(item => item.kind === "totp").map(item => ({ value: item.name, label: item.name }))} />
              </label>}
            </>;
          })()}
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
              {entry.sequence && <p>{t(`ssh_authorization.mode_${entry.sequence}`)}{entry.totpAlias && ` · ${entry.totpAlias}`}</p>}
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
