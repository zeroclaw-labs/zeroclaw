import { useState } from "react";
import { Eye, EyeOff } from "lucide-react";
import type { ChannelFieldSpec } from "@/types/api";
import StringArrayEditor from "./StringArrayEditor";

interface ChannelConfigFormProps {
  fields: ChannelFieldSpec[];
  values: Record<string, unknown>;
  onChange: (values: Record<string, unknown>) => void;
  disabled?: boolean;
}

export default function ChannelConfigForm({
  fields,
  values,
  onChange,
  disabled,
}: ChannelConfigFormProps) {
  const [showPasswords, setShowPasswords] = useState<Record<string, boolean>>(
    {},
  );

  const setValue = (name: string, value: unknown) => {
    onChange({ ...values, [name]: value });
  };

  const togglePassword = (name: string) => {
    setShowPasswords((prev) => ({ ...prev, [name]: !prev[name] }));
  };

  return (
    <div className="space-y-4">
      {fields.map((field) => {
        const val = values[field.name] ?? field.default_value ?? "";

        return (
          <div key={field.name}>
            <label className="block text-xs font-semibold text-[#8090b0] mb-1.5">
              {field.label}
              {field.required && (
                <span className="text-[#ff4466] ml-0.5">*</span>
              )}
            </label>

            {field.type === "text" && (
              <input
                type="text"
                value={(val as string) || ""}
                onChange={(e) => setValue(field.name, e.target.value)}
                placeholder={field.placeholder}
                disabled={disabled}
                className="input-electric w-full px-3 py-2 text-sm"
              />
            )}

            {field.type === "password" && (
              <div className="relative">
                <input
                  type={showPasswords[field.name] ? "text" : "password"}
                  value={(val as string) || ""}
                  onChange={(e) => setValue(field.name, e.target.value)}
                  placeholder={field.placeholder}
                  disabled={disabled}
                  className="input-electric w-full px-3 py-2 pr-10 text-sm"
                />
                <button
                  type="button"
                  onClick={() => togglePassword(field.name)}
                  className="absolute right-2 top-1/2 -translate-y-1/2 text-[#556080] hover:text-white transition-colors"
                >
                  {showPasswords[field.name] ? (
                    <EyeOff className="h-4 w-4" />
                  ) : (
                    <Eye className="h-4 w-4" />
                  )}
                </button>
              </div>
            )}

            {field.type === "number" && (
              <input
                type="number"
                value={val === "" ? "" : Number(val)}
                onChange={(e) =>
                  setValue(
                    field.name,
                    e.target.value === "" ? "" : Number(e.target.value),
                  )
                }
                placeholder={field.placeholder}
                disabled={disabled}
                className="input-electric w-full px-3 py-2 text-sm"
              />
            )}

            {field.type === "boolean" && (
              <button
                type="button"
                onClick={() => setValue(field.name, !val)}
                disabled={disabled}
                className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                  val ? "bg-[#0080ff]" : "bg-[#1a1a3e] border border-[#334060]"
                }`}
              >
                <span
                  className={`inline-block h-4 w-4 rounded-full bg-white transition-transform ${
                    val ? "translate-x-6" : "translate-x-1"
                  }`}
                />
              </button>
            )}

            {field.type === "select" && field.options && (
              <select
                value={(val as string) || ""}
                onChange={(e) => setValue(field.name, e.target.value)}
                disabled={disabled}
                className="input-electric w-full px-3 py-2 text-sm"
              >
                {field.options.map((opt) => (
                  <option key={opt} value={opt}>
                    {opt}
                  </option>
                ))}
              </select>
            )}

            {field.type === "string_array" && (
              <StringArrayEditor
                value={Array.isArray(val) ? (val as string[]) : []}
                onChange={(arr) => setValue(field.name, arr)}
                placeholder={field.placeholder}
                disabled={disabled}
              />
            )}

            {field.help_text && (
              <p className="text-[10px] text-[#556080] mt-1">
                {field.help_text}
              </p>
            )}
          </div>
        );
      })}
    </div>
  );
}
