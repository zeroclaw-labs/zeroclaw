import { useState } from "react";
import { Button, Select } from "@/components/ui";
import { t } from "@/lib/i18n";
import { changeHttpCredential, type HttpCredentialRow, type PamChallengeRole, type SshAuthProfile, type SshDestination, type SshProfileAuth } from "@/lib/api";

const field = "block w-full rounded-md border border-border bg-background p-2";
const roles: Record<PamChallengeRole, string> = {
  username: "Username", password: "Password", totp: "TOTP", password_then_totp: "Password followed by TOTP",
};
const modes: Record<SshProfileAuth["mode"], string> = {
  public_key: "Public key", password: "Username and password", pam: "PAM keyboard-interactive",
};
export type SshProfileForm = { destination: SshDestination; auth: SshProfileAuth; previous?: { anchor: string; index: number; revision: string } };
export const emptySshProfile = (): SshProfileForm => ({
  destination: { host: "", port: 22, username: "" },
  auth: { mode: "password", password_alias: "" },
});
const matches = (a: SshDestination, b: SshDestination) =>
  a.host.toLowerCase() === b.host.toLowerCase() && a.port === b.port && a.username === b.username;

export default function SshProfiles({ rows, reload, form, setForm }: { rows: HttpCredentialRow[]; reload: () => Promise<void>; form: SshProfileForm | undefined; setForm: (form: SshProfileForm | undefined) => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(false);

  async function save(row: HttpCredentialRow, profiles: SshAuthProfile[]) {
    setBusy(true);
    setError(false);
    try {
      await changeHttpCredential({ name: row.name, operation: "replace",
        permissions: { expected_revision: row.permission_revision, ssh_profiles: profiles } });
      await reload();
      setForm(undefined);
    } catch {
      setError(true);
      await reload().catch(() => undefined);
    } finally {
      setBusy(false);
    }
  }

  const options = (kind: HttpCredentialRow["kind"]) => rows.filter(row =>
    form && row.kind === kind && row.destinations?.some(destination => matches(destination, form.destination)));
  const aliasOptions = (kind: HttpCredentialRow["kind"], placeholder: string) => [
    { value: "", label: placeholder },
    ...options(kind).map(row => ({ value: row.name, label: row.name })),
  ];

  function mode(value: SshProfileAuth["mode"]): SshProfileAuth {
    if (value === "public_key") return { mode: value, key_alias: "" };
    if (value === "password") return { mode: value, password_alias: "" };
    return { mode: value, challenges: [] };
  }
  function update(form: SshProfileForm) {
    const { destination, auth, previous } = form;
    const factors = auth.mode === "public_key" ? [auth.key_alias]
      : auth.mode === "password" ? [auth.password_alias] : [auth.password_alias, auth.totp_alias];
    if (factors.every(factor => !factor)) {
      setError(true);
      return;
    }
    const anchor = previous?.anchor ?? factors.find(Boolean);
    const row = rows.find(row => row.name === anchor);
    if (!row || !factors.includes(row.name)
      || (previous && previous.revision !== row.permission_revision)) {
      setError(true);
      return;
    }
    const profiles = [...(row.ssh_profiles ?? [])];
    const profile = { destination, authentication: auth };
    if (previous) profiles[previous.index] = profile;
    else profiles.push(profile);
    void save(row, profiles);
  }

  if (!rows.some(row => row.ssh_profiles !== undefined)) return null;
  return <section className="space-y-4">
    <h3 className="font-medium">SSH authentication profiles</h3>
    {error && <p role="alert">Could not save the SSH profile. Check its destination, grants and challenge settings.</p>}
    {form && <form className="space-y-3" onSubmit={event => { event.preventDefault(); update(form); }}>
      <details className="space-y-2 rounded-md border p-3 text-sm">
        <summary className="cursor-pointer font-medium">{t("ssh_profile_help.title")}</summary>
        <p>{t("ssh_profile_help.destination")}</p>
        <ul className="list-disc space-y-1 pl-5">
          <li>{t("ssh_profile_help.key")}</li>
          <li>{t("ssh_profile_help.password")}</li>
          <li>{t("ssh_profile_help.pam")}
            <ul className="list-disc space-y-1 pl-5">
              <li>{t("ssh_profile_help.username_reply")}</li>
              <li>{t("ssh_profile_help.password_reply")}</li>
              <li>{t("ssh_profile_help.totp_reply")}</li>
              <li>{t("ssh_profile_help.combined_reply")}</li>
            </ul>
          </li>
        </ul>
        <p>{t("ssh_profile_help.example")}</p>
      </details>
      <label className="block">Host<input className={field} required value={form.destination.host}
        onChange={event => setForm({ ...form, destination: { ...form.destination, host: event.target.value } })} /></label>
      <label className="block">Port<input className={field} required type="number" min={1} max={65535} value={form.destination.port}
        onChange={event => setForm({ ...form, destination: { ...form.destination, port: Number(event.target.value) } })} /></label>
      <label className="block">SSH username<input className={field} required value={form.destination.username}
        onChange={event => setForm({ ...form, destination: { ...form.destination, username: event.target.value } })} /></label>
      <label className="block">Authentication
        <Select aria-label="Authentication" value={form.auth.mode}
          onChange={value => setForm({ ...form, auth: mode(value as SshProfileAuth["mode"]) })}
          options={Object.entries(modes).map(([value, label]) => ({ value, label }))} />
      </label>
      {form.auth.mode === "public_key" && <label className="block">SSH key alias
        <Select aria-label="SSH key alias" value={form.auth.key_alias}
          onChange={value => setForm({ ...form, auth: { mode: "public_key", key_alias: value } })}
          options={aliasOptions("ssh_key", "Select an authorized key")} />
      </label>}
      {form.auth.mode !== "public_key" && <label className="block">Password alias
        <Select aria-label="Password alias" value={form.auth.password_alias ?? ""}
          onChange={value => {
            const auth = form.auth;
            if (auth.mode === "password") setForm({ ...form, auth: { ...auth, password_alias: value } });
            if (auth.mode === "pam") setForm({ ...form, auth: { ...auth, password_alias: value || undefined } });
          }}
          options={aliasOptions("user_password", "No password selected")} />
      </label>}
      {form.auth.mode === "pam" && (() => {
        const auth = form.auth;
        return <>
          <label className="block">TOTP alias
            <Select aria-label="TOTP alias" value={auth.totp_alias ?? ""}
              onChange={value => setForm({ ...form, auth: { ...auth, totp_alias: value || undefined } })}
              options={aliasOptions("totp", "No TOTP selected")} />
          </label>
          <fieldset className="space-y-3">
            <legend>PAM challenges in server order</legend>
            {auth.challenges.map((challenge, index) => <div key={index} className="space-y-2">
              <label className="block">Challenge role
                <Select aria-label="Challenge role" value={challenge.role}
                  onChange={value => setForm({ ...form, auth: { ...auth, challenges: auth.challenges.map((item, pos) =>
                    pos === index ? { ...item, role: value as PamChallengeRole } : item) } })}
                  options={Object.entries(roles).map(([value, label]) => ({ value, label }))} />
              </label>
              <label className="block">Exact prompt label<input className={field} required maxLength={128} value={challenge.label}
                onChange={event => setForm({ ...form, auth: { ...auth, challenges: auth.challenges.map((item, pos) =>
                  pos === index ? { ...item, label: event.target.value } : item) } })} /></label>
              <Button type="button" onClick={() => setForm({ ...form, auth: { ...auth, challenges: auth.challenges.filter((_, pos) => pos !== index) } })}>Remove challenge</Button>
            </div>)}
            <Button type="button" disabled={auth.challenges.length >= 3} onClick={() =>
              setForm({ ...form, auth: { ...auth, challenges: [...auth.challenges, { role: "password", label: "" }] } })}>Add challenge</Button>
          </fieldset>
        </>;
      })()}
      <Button type="submit" disabled={busy}>Save SSH profile</Button>
      <Button type="button" disabled={busy} onClick={() => setForm(undefined)}>Cancel</Button>
    </form>}
    <ul className="space-y-2">
      {rows.flatMap(row => (row.ssh_profiles ?? []).map((profile, index) => <li className="rounded-md border p-3" key={JSON.stringify([row.name, profile.destination])}>
        <p>{profile.destination.username}@{profile.destination.host} · {profile.destination.port}</p>
        <p>{modes[profile.authentication.mode]} · {row.name}</p>
        <Button type="button" disabled={busy} onClick={() => setForm({ destination: profile.destination,
          auth: profile.authentication, previous: { anchor: row.name, index, revision: row.permission_revision } })}>Edit</Button>
        <Button type="button" disabled={busy} onClick={() => void save(row, (row.ssh_profiles ?? []).filter((_, pos) => pos !== index))}>Remove</Button>
      </li>))}
    </ul>
  </section>;
}
