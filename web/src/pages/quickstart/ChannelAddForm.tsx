import { useEffect, useReducer, useState } from "react";
import { Plus } from "lucide-react";
import type {
  QuickstartFieldDescriptor,
  QuickstartState,
} from "../../lib/api";
import { quickstartFields } from "../../lib/api";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { t } from "../../lib/i18n";
import {
  channelFieldStateReducer,
  initialChannelFieldState,
} from "./channel-fields";
import { LabeledInput } from "./quickstart-form-controls";

const INPUT_CLASS =
  "w-full h-9 px-3 rounded-[var(--radius-md)] border border-pc-border bg-pc-input text-sm text-pc-text placeholder:text-pc-text-faint focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-pc-accent/40 focus-visible:border-pc-accent/40";
const MUTED = { color: "var(--pc-text-muted)" } as const;
const ERROR = { color: "var(--color-status-error)" } as const;

export interface StagedChannel {
  mode: "fresh" | "existing";
  channel_type: string;
  alias: string;
  fields: Record<string, string>;
}

export type ChannelFieldsLoader = (
  type: string,
) => Promise<{ fields: QuickstartFieldDescriptor[] }>;

export interface ChannelAddFormProps {
  state: QuickstartState | null;
  inConfig: Set<string>;
  inFlight: Set<string>;
  reusable: string[];
  onAdd: (c: StagedChannel) => void;
  onCancel: () => void;
  loadFields?: ChannelFieldsLoader;
}

const defaultLoadFields: ChannelFieldsLoader = (type) =>
  quickstartFields({ section: "channel", type_key: type });

export function ChannelAddForm({
  state,
  inConfig,
  inFlight,
  reusable,
  onAdd,
  onCancel,
  loadFields = defaultLoadFields,
}: ChannelAddFormProps) {
  const [channelFields, dispatchChannelFields] = useReducer(
    channelFieldStateReducer,
    initialChannelFieldState(reusable.length > 0 ? "existing" : "fresh"),
  );
  const [existingRef, setExistingRef] = useState(reusable[0] ?? "");
  const [alias, setAlias] = useState("");
  const { mode, type, descriptors, fields } = channelFields;

  useEffect(() => {
    if (!type) return;
    let cancelled = false;
    void (async () => {
      try {
        const f = await loadFields(type);
        if (!cancelled) {
          dispatchChannelFields({
            kind: "descriptors-loaded",
            channelType: type,
            descriptors: f.fields,
          });
        }
      } catch {
        // Keep the current field state so a transient descriptor failure does
        // not discard values the user has already entered.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [type, loadFields]);

  const freshRef = type && alias.trim() ? `${type}.${alias.trim()}` : "";
  const conflict =
    freshRef !== "" && (inConfig.has(freshRef) || inFlight.has(freshRef));
  const canAdd =
    mode === "existing"
      ? existingRef !== ""
      : type !== "" && alias.trim() !== "" && !conflict;

  const submit = () => {
    if (mode === "existing") {
      const [t, a] = existingRef.split(".");
      if (!t || !a) return;
      onAdd({ mode: "existing", channel_type: t, alias: a, fields: {} });
    } else {
      onAdd({
        mode: "fresh",
        channel_type: type,
        alias: alias.trim(),
        fields,
      });
    }
  };

  return (
    <Card className="p-4 space-y-3 bg-pc-elevated">
      <div className="flex gap-2">
        <Button
          variant={mode === "existing" ? "primary" : "ghost"}
          size="sm"
          disabled={reusable.length === 0}
          onClick={() =>
            dispatchChannelFields({ kind: "mode-changed", mode: "existing" })
          }
        >
          {t("quickstart.use_existing")}
        </Button>
        <Button
          variant={mode === "fresh" ? "primary" : "ghost"}
          size="sm"
          onClick={() =>
            dispatchChannelFields({ kind: "mode-changed", mode: "fresh" })
          }
        >
          {t("quickstart.create_new")}
        </Button>
        <div className="flex-1" />
        <Button variant="ghost" size="sm" onClick={onCancel}>
          {t("common.cancel")}
        </Button>
      </div>

      {mode === "existing" ? (
        reusable.length === 0 ? (
          <div className="text-xs" style={MUTED}>
            {t("quickstart.no_unassigned_channels")}
          </div>
        ) : (
          <select
            className={INPUT_CLASS}
            value={existingRef}
            onChange={(e) => setExistingRef(e.target.value)}
          >
            {reusable.map((r) => (
              <option key={r} value={r}>
                {r}
              </option>
            ))}
          </select>
        )
      ) : (
        <>
          <label className="block">
            <div className="text-xs uppercase tracking-wider mb-1" style={MUTED}>
              {t("quickstart.channel_type")}
            </div>
            <select
              className={INPUT_CLASS}
              value={type}
              onChange={(e) => {
                const next = e.target.value;
                dispatchChannelFields({
                  kind: "channel-type-changed",
                  channelType: next,
                });
                setAlias((prev) => (prev === "" || prev === type ? next : prev));
              }}
            >
              <option value="" disabled>
                {t("quickstart.pick_channel_type")}
              </option>
              {state?.channel_types.map((opt) => (
                <option key={opt.kind} value={opt.kind}>
                  {opt.display_name}
                </option>
              ))}
            </select>
          </label>

          <LabeledInput label={t("quickstart.alias_label")} value={alias} onChange={setAlias} />
          {conflict && (
            <div className="text-xs" style={ERROR}>
              <code>{freshRef}</code> {t("quickstart.already_exists")}
            </div>
          )}

          {descriptors.map((d) => (
            <LabeledInput
              key={d.key}
              label={d.label}
              type={d.is_secret ? "password" : "text"}
              value={fields[d.key] ?? ""}
              onChange={(v) =>
                dispatchChannelFields({
                  kind: "field-changed",
                  key: d.key,
                  value: v,
                })
              }
              placeholder={d.help}
              required={d.required}
            />
          ))}
        </>
      )}

      <div className="flex justify-end">
        <Button size="sm" disabled={!canAdd} onClick={submit}>
          <Plus className="h-3.5 w-3.5" />
          {t("quickstart.add")}
        </Button>
      </div>
    </Card>
  );
}
