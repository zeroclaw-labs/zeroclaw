import { useState } from "react";
import type { AcpRequest } from "@/lib/acp";
import { t } from "@/lib/i18n";

export default function CodeQuestion({
  request,
  onRespond,
}: {
  request: AcpRequest;
  onRespond: (result: unknown) => void;
}) {
  const params = request.params as {
    message?: string;
    requestedSchema?: {
      properties?: Record<
        string,
        {
          type?: string;
          minItems?: number;
          maxItems?: number;
          oneOf?: { const: string; title: string }[];
          items?: { anyOf?: { const: string; title: string }[] };
        }
      >;
    };
  };
  const [values, setValues] = useState<Record<string, string[]>>({});
  const properties = Object.entries(params.requestedSchema?.properties ?? {});
  const invalid =
    properties.length === 0 ||
    properties.some(([name, property]) => {
      const count = values[name]?.length ?? 0;
      return (
        !(property.oneOf ?? property.items?.anyOf)?.length ||
        count < (property.minItems ?? 1) ||
        count >
          (property.maxItems ?? (property.type === "array" ? Infinity : 1))
      );
    });
  return (
    <form
      className="border-t border-pc-border p-4 space-y-3"
      onSubmit={(event) => {
        event.preventDefault();
        if (invalid) return;
        onRespond({
          action: "accept",
          content: Object.fromEntries(
            properties.map(([name, property]) => [
              name,
              property.type === "array"
                ? (values[name] ?? [])
                : values[name]?.[0],
            ]),
          ),
        });
      }}
    >
      <p className="text-sm font-medium">{params.message}</p>
      {properties.map(([name, property]) => (
        <fieldset key={name} className="space-y-2">
          <legend className="sr-only">{name}</legend>
          {(property.oneOf ?? property.items?.anyOf ?? []).map((option) => (
            <label
              key={option.const}
              className="flex items-center gap-2 text-sm"
            >
              <input
                type={property.type === "array" ? "checkbox" : "radio"}
                name={name}
                value={option.const}
                checked={values[name]?.includes(option.const) ?? false}
                onChange={(event) =>
                  setValues((current) => ({
                    ...current,
                    [name]:
                      property.type === "array"
                        ? event.target.checked
                          ? [...(current[name] ?? []), option.const]
                          : (current[name] ?? []).filter(
                              (value) => value !== option.const,
                            )
                        : [option.const],
                  }))
                }
              />
              {option.title}
            </label>
          ))}
        </fieldset>
      ))}
      <div className="flex gap-3">
        <button
          type="button"
          onClick={() => onRespond({ action: "decline" })}
          className="btn-secondary px-3 py-2 text-sm"
        >
          {t("common.cancel")}
        </button>
        <button
          disabled={invalid}
          className="btn-primary px-3 py-2 text-sm disabled:opacity-40"
        >
          {t("common.confirm")}
        </button>
      </div>
    </form>
  );
}
