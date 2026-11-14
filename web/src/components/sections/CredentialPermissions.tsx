import { useEffect, useState } from "react";
import { Button } from "@/components/ui";
import { changeHttpCredential, listHttpCredentials, type HttpCredentialRow } from "@/lib/api";
import { t } from "@/lib/i18n";
import { parseSshDestinations } from "./HttpCredentials";

export default function CredentialPermissions() {
  const [rows, setRows] = useState<HttpCredentialRow[]>();
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  useEffect(() => {
    let active = true;
    void listHttpCredentials().then(
      (result) => { if (active) setRows(result.credentials); },
      () => { if (active) setMessage(t("plugin_tls.operator")); },
    );
    return () => { active = false; };
  }, []);

  async function save(name: string, permissions: { http_allowed: boolean } | { destinations: ReturnType<typeof parseSshDestinations> }) {
    setBusy(true);
    setMessage("");
    try {
      await changeHttpCredential({ name, operation: "replace", permissions });
      setRows((await listHttpCredentials()).credentials);
      setMessage(t("plugin_tls.saved"));
    } catch {
      setMessage(t("plugin_tls.failed"));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="space-y-4">
      {message && <p role="status">{message}</p>}
      {rows?.length === 0 && <p>{t("http_credentials.empty")}</p>}
      {rows?.map(row => (
        <form key={row.name} className="space-y-3 rounded-md border p-4" onSubmit={event => {
          event.preventDefault();
          const data = new FormData(event.currentTarget);
          if (row.kind === "ssh_key") {
            void save(row.name, { destinations: parseSshDestinations(String(data.get("destinations") ?? "")) });
          } else {
            void save(row.name, { http_allowed: data.has("http") });
          }
        }}>
          <p className="font-medium">{row.name}</p>
          {row.kind === "ssh_key" ? (
            <label className="block space-y-1">
              <span>{t("http_credentials.ssh_grants")}</span>
              <textarea name="destinations" className="block w-full rounded-md border bg-background p-2"
                defaultValue={row.destinations?.map(d => `${d.username}@${d.host.includes(":") ? `[${d.host}]` : d.host}:${d.port}`).join("\n")} />
            </label>
          ) : row.http_allowed != null ? (
            <label className="flex items-center gap-2">
              <input type="checkbox" name="http" defaultChecked={row.http_allowed} />
              <span>HTTP Request · {row.origin}</span>
            </label>
          ) : <p>{t("http_credentials.no_consumer")}</p>}
          {(row.kind === "ssh_key" || row.http_allowed != null) && (
            <Button type="submit" disabled={busy}>{t("http_credentials.save_permissions")}</Button>
          )}
        </form>
      ))}
    </section>
  );
}
