import { useEffect, useState } from "react";
import { Button, Select } from "@/components/ui";
import { HttpError, changeHttpCredential, listHttpCredentials, type HttpCredentialRow, type RemoteFilesGrant } from "@/lib/api";
import { t } from "@/lib/i18n";
import { parseSshDestinations } from "./HttpCredentials";

type Consumer = "http" | "browser" | "ssh" | "remote_files";
type Grant = { alias: string; consumer: Consumer; destination: string; remote?: RemoteFilesGrant };

const consumerLabel = (consumer: Consumer) => consumer === "remote_files"
  ? t("http_credentials.remote_files")
  : { http: "HTTP Request", browser: "Browser", ssh: "SSH" }[consumer];

const webdavOperations = [
  "list_directory", "get_file_info", "read_file", "create_file",
  "copy_files", "move_files", "delete_files", "rename_files",
  "create_directory", "rename_directory",
] as const;

function webdavDraft(row: HttpCredentialRow, resourceName?: string): RemoteFilesGrant | undefined {
  const resource = resourceName
    ? row.remote_files_resources?.find(item => item.resource === resourceName)
    : row.remote_files_resources?.[0];
  return resource && { ...resource, rights: { operations: [], require_human_approval: false } };
}

function consumers(row?: HttpCredentialRow): Consumer[] {
  if (!row) return [];
  if (row.kind === "ssh_key") return ["ssh"];
  if (row.kind === "totp") return ["browser"];
  const result: Consumer[] = row.kind === "user_password" ? ["http", "browser"] : ["http"];
  if (row.remote_files_resources?.length) result.push("remote_files");
  return result;
}

function grantsFor(row: HttpCredentialRow): Grant[] {
  const entry = (consumer: Consumer, destination: string): Grant => ({ alias: row.name, consumer, destination });
  return [
    ...(row.http_origins ?? (row.http_allowed ? [row.origin] : [])).map(origin => entry("http", origin)),
    ...(row.browser_origins ?? (row.browser_origin ? [row.browser_origin] : [])).map(origin => entry("browser", origin)),
    ...(row.destinations ?? []).map(d => entry("ssh", `${d.username}@${d.host.includes(":") ? `[${d.host}]` : d.host}:${d.port}`)),
    ...(row.remote_files_grants ?? []).map(remote => ({ alias: row.name, consumer: "remote_files" as const, destination: remote.root, remote })),
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
      const permissions = (() => {
        if (entry.consumer === "remote_files") {
          const remote = entry.remote;
          if (!remote || (!remove && !remote.rights.operations.length)) return undefined;
          const key = (grant: RemoteFilesGrant) => grant.resource.toLowerCase();
          const edited = remove ? entry.remote : replaced?.remote;
          const remaining = (current.remote_files_grants ?? []).filter(grant => !edited || key(grant) !== key(edited));
          if (!remove) {
            if (remaining.some(grant => key(grant) === key(remote))) return undefined;
            remaining.push(remote);
          }
          return { remote_files_grants: remaining };
        }
        const siblings = grants.filter(item => item.alias === entry.alias && item.consumer === entry.consumer &&
          item.destination !== (remove ? entry.destination : replaced?.destination)).map(item => item.destination);
        if (!remove) siblings.push(entry.destination.trim());
        const destinations = [...new Set(siblings)];
        if (entry.consumer === "http") return { http_origins: destinations };
        if (entry.consumer === "browser") return { browser: { origins: destinations } };
        const ssh = parseSshDestinations(destinations.join("\n"));
        return ssh.length === destinations.length ? { destinations: ssh } : undefined;
      })();
      if (!permissions) {
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
            {editing ? <span className={field}>{consumerLabel(draft.consumer)}</span> : <Select value={draft.consumer} aria-label={t("http_credentials.consumer")}
              onChange={value => {
                const consumer = value as Consumer;
                const remote = consumer === "remote_files" ? webdavDraft(row) : undefined;
                setDraft({ alias: draft.alias, consumer, destination: remote?.root ?? row.origin, remote });
              }}
              options={consumers(row).map(consumer => ({ value: consumer, label: consumerLabel(consumer) }))} />}
          </label>
          {draft.consumer === "remote_files" && draft.remote ? (
            <div className="space-y-4">
              <label className="block space-y-1">
                <span>{t("http_credentials.webdav_resource")}</span>
                <Select value={draft.remote.resource} aria-label={t("http_credentials.webdav_resource")}
                  onChange={value => {
                    const remote = webdavDraft(row, value);
                    if (remote) setDraft({ ...draft, destination: remote.root, remote });
                  }}
                  options={(row.remote_files_resources ?? []).map(resource => ({
                    value: resource.resource, label: `${resource.resource} · ${resource.root}`,
                  }))} />
              </label>
              <p className="break-words text-sm text-muted-foreground">{draft.remote.root}</p>
              <fieldset className="space-y-2 rounded-md border border-border p-3">
                <legend className="px-1">{t("http_credentials.webdav_operations")}</legend>
                <div className="grid gap-x-4 gap-y-2 sm:grid-cols-2">
                  {webdavOperations.map(operation => (
                    <label key={operation} className="flex items-center gap-2">
                      <input type="checkbox" checked={draft.remote?.rights.operations.includes(operation)}
                        onChange={event => {
                          const remote = draft.remote;
                          if (!remote) return;
                          const operations = event.target.checked
                            ? [...remote.rights.operations, operation]
                            : remote.rights.operations.filter(item => item !== operation);
                          setDraft({ ...draft, remote: { ...remote, rights: { ...remote.rights, operations } } });
                        }} />
                      <span>{t(`http_credentials.webdav.${operation}`)}</span>
                    </label>
                  ))}
                </div>
              </fieldset>
              <label className="flex items-center gap-2">
                <input type="checkbox" checked={draft.remote.rights.require_human_approval}
                  onChange={event => {
                    const remote = draft.remote;
                    if (remote) setDraft({
                      ...draft,
                      remote: { ...remote, rights: { ...remote.rights, require_human_approval: event.target.checked } },
                    });
                  }} />
                <span>{t("http_credentials.require_human_approval")}</span>
              </label>
            </div>
          ) : (
            <label className="block space-y-1">
            <span>{t(draft.consumer === "ssh" ? "http_credentials.ssh_destinations" : "http_credentials.origin")}</span>
            <input className={field} type={draft.consumer === "ssh" ? "text" : "url"} required
              placeholder={draft.consumer === "ssh" ? "operator@server.example.com:22" : "https://login.example.com"}
              value={draft.destination} onChange={event => setDraft({ ...draft, destination: event.target.value })} />
          </label>
          )}
          <div className="flex gap-2">
            <Button type="submit" disabled={busy || (draft.consumer === "remote_files" && !draft.remote?.rights.operations.length)}>{t(editing ? "http_credentials.save_permissions" : "http_credentials.add_permission")}</Button>
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
              <p>{consumerLabel(entry.consumer)} · {entry.destination}</p>
              {entry.remote && (
                <p className="text-sm text-muted-foreground">
                  {entry.remote.resource} · {entry.remote.rights.operations.map(operation => t(`http_credentials.webdav.${operation}`)).join(", ")}
                  {entry.remote.rights.require_human_approval && ` · ${t("http_credentials.require_human_approval")}`}
                </p>
              )}
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
