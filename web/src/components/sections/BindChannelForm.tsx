// Operator-bind form: authorize a user on a pairing channel (Telegram /
// WeChat / LINE) by adding their native id to the channel's allowlist — the
// GUI equivalent of `zeroclaw channel bind-<type> <id> --alias <alias>`. The
// bound user can message the bot immediately, with no `/bind` code round trip.

import { useEffect, useMemo, useState } from "react";
import { Badge, Button, Card, Select } from "@/components/ui";
import {
  bindChannelIdentity,
  getChannels,
  type BindChannelResponse,
} from "@/lib/api";
import type { ChannelDetail } from "@/types/api";

// Only these channel types have a one-id-per-peer operator-bind surface.
const BINDABLE_TYPES = ["telegram", "wechat", "line"];

type Status =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "ok"; msg: string }
  | { kind: "err"; msg: string };

export default function BindChannelForm({
  onBound,
  channelType: fixedType,
  alias: fixedAlias,
}: {
  onBound?: () => void;
  /** When both set, the form is pre-scoped to this channel and hides the picker. */
  channelType?: string;
  alias?: string;
}) {
  const prescoped = Boolean(fixedType && fixedAlias);
  const [channels, setChannels] = useState<ChannelDetail[]>([]);
  const [selected, setSelected] = useState<string>(
    prescoped ? `${fixedType}.${fixedAlias}` : "",
  ); // "<type>.<alias>"
  const [identity, setIdentity] = useState("");
  const [status, setStatus] = useState<Status>({ kind: "idle" });
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (prescoped) return;
    let alive = true;
    getChannels()
      .then((all) => {
        if (!alive) return;
        const bindable = all.filter((c) => BINDABLE_TYPES.includes(c.type));
        setChannels(bindable);
        const first = bindable[0];
        setSelected((prev) =>
          prev || !first ? prev : `${first.type}.${first.alias}`,
        );
      })
      .catch(() => {
        /* leave the empty-state message in place */
      });
    return () => {
      alive = false;
    };
  }, [prescoped]);

  const options = useMemo(
    () =>
      channels.map((c) => ({
        value: `${c.type}.${c.alias}`,
        label: `${c.type}.${c.alias}${
          c.owning_agent ? ` → ${c.owning_agent}` : ""
        }`,
      })),
    [channels],
  );

  const [channelType, alias] = useMemo<[string, string]>(() => {
    const dot = selected.indexOf(".");
    return dot === -1
      ? ["", ""]
      : [selected.slice(0, dot), selected.slice(dot + 1)];
  }, [selected]);

  // Only Telegram has a CLI bind verb today, so only show the parity command
  // there; for WeChat/LINE the button is the bind surface.
  const cliCommand =
    channelType === "telegram" && identity.trim()
      ? `zeroclaw channel bind-telegram ${identity.trim()}${
          alias === "default" ? "" : ` --alias ${alias}`
        }`
      : "";

  const canBind =
    !!selected && identity.trim().length > 0 && status.kind !== "loading";

  async function onSubmit() {
    if (!canBind) return;
    setStatus({ kind: "loading" });
    const id = identity.trim();
    try {
      const res: BindChannelResponse = await bindChannelIdentity({
        channel_type: channelType,
        alias,
        identity: id,
      });
      setIdentity("");
      setStatus({
        kind: "ok",
        msg: res.already_bound
          ? `${id} was already authorized on ${channelType}.${alias}.`
          : `Authorized ${id} on ${channelType}.${alias}. They can message the bot now — no /bind needed.`,
      });
      onBound?.();
    } catch (e) {
      setStatus({
        kind: "err",
        msg: e instanceof Error ? e.message : "Bind failed.",
      });
    }
  }

  function copyCli() {
    if (!cliCommand) return;
    const done = () => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    };
    if (navigator.clipboard?.writeText) {
      navigator.clipboard.writeText(cliCommand).then(done).catch(done);
    } else {
      done();
    }
  }

  return (
    <Card className="max-w-2xl space-y-4 p-4">
      <div className="space-y-1">
        <h3 className="text-sm font-semibold text-pc-text">
          Authorize a user (no /bind message)
        </h3>
        <p className="text-xs text-pc-text-muted">
          Add someone&rsquo;s id to a Telegram, WeChat, or LINE channel&rsquo;s
          allowlist. They can message the bot immediately — no pairing code, no
          /bind.
        </p>
      </div>

      {!prescoped && channels.length === 0 ? (
        <p className="text-xs text-pc-text-faint">
          No Telegram, WeChat, or LINE channels are configured.
        </p>
      ) : (
        <>
          {!prescoped && (
            <label className="block space-y-1">
              <span className="text-xs text-pc-text-muted">Channel</span>
              <Select
                value={selected}
                onChange={setSelected}
                options={options}
              />
            </label>
          )}

          <label className="block space-y-1">
            <span className="text-xs text-pc-text-muted">
              Identity (e.g. Telegram numeric user id, or @username)
            </span>
            <input
              type="text"
              className="input-electric w-full px-3 py-1.5 text-sm"
              placeholder="123456789"
              value={identity}
              onChange={(e) => setIdentity(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void onSubmit();
              }}
            />
          </label>

          {cliCommand ? (
            <div className="space-y-1">
              <span className="text-xs text-pc-text-muted">
                Equivalent CLI command
              </span>
              <div className="flex items-center gap-2">
                <code className="flex-1 overflow-x-auto rounded border border-pc-border bg-pc-base px-2 py-1.5 font-mono text-xs text-pc-text-muted">
                  {cliCommand}
                </code>
                <Button variant="ghost" size="sm" onClick={copyCli}>
                  {copied ? "Copied" : "Copy"}
                </Button>
              </div>
            </div>
          ) : null}

          <div className="flex items-center gap-3">
            <Button
              variant="primary"
              size="md"
              onClick={() => void onSubmit()}
              disabled={!canBind}
            >
              {status.kind === "loading" ? "Binding…" : "Bind"}
            </Button>
            {status.kind === "ok" ? <Badge tone="ok">Done</Badge> : null}
            {status.kind === "err" ? <Badge tone="error">Error</Badge> : null}
          </div>

          {status.kind === "ok" || status.kind === "err" ? (
            <p
              className={`text-xs ${
                status.kind === "ok" ? "text-pc-text-muted" : "text-pc-text"
              }`}
            >
              {status.msg}
            </p>
          ) : null}
        </>
      )}
    </Card>
  );
}
