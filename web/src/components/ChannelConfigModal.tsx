import { useState, useEffect } from "react";
import { X, Save, Trash2, Loader2 } from "lucide-react";
import type { ChannelSchema } from "@/types/api";
import {
  getChannelConfig,
  putChannelConfig,
  deleteChannelConfig,
} from "@/lib/api";
import ChannelConfigForm from "./ChannelConfigForm";

interface ChannelConfigModalProps {
  schema: ChannelSchema;
  onClose: () => void;
  onSaved: () => void;
}

export default function ChannelConfigModal({
  schema,
  onClose,
  onSaved,
}: ChannelConfigModalProps) {
  const [values, setValues] = useState<Record<string, unknown>>({});
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [configured, setConfigured] = useState(false);

  useEffect(() => {
    getChannelConfig(schema.channel_key)
      .then((res) => {
        setConfigured(res.configured);
        if (res.config) {
          setValues(res.config);
        } else {
          // Set defaults from schema
          const defaults: Record<string, unknown> = {};
          for (const field of schema.fields) {
            if (field.default_value !== undefined) {
              defaults[field.name] = field.default_value;
            }
          }
          setValues(defaults);
        }
      })
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, [schema]);

  const handleSave = async () => {
    setSaving(true);
    setError(null);
    try {
      await putChannelConfig(schema.channel_key, values);
      onSaved();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "Save failed");
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
    setDeleting(true);
    setError(null);
    try {
      await deleteChannelConfig(schema.channel_key);
      onSaved();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "Delete failed");
    } finally {
      setDeleting(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center animate-fade-in">
      {/* Backdrop */}
      <div
        className="absolute inset-0 bg-black/60 backdrop-blur-sm"
        onClick={onClose}
      />

      {/* Modal */}
      <div className="relative glass-card w-full max-w-lg max-h-[85vh] flex flex-col animate-fade-in-scale mx-4">
        {/* Top glow */}
        <div
          className="absolute -top-px left-1/4 right-1/4 h-px"
          style={{
            background:
              "linear-gradient(90deg, transparent, #0080ff, transparent)",
          }}
        />

        {/* Header */}
        <div className="flex items-center justify-between p-5 border-b border-[#1a1a3e]/40">
          <div>
            <h3 className="text-sm font-semibold text-white">
              {schema.display_name}
            </h3>
            {schema.description && (
              <p className="text-[10px] text-[#556080] mt-0.5">
                {schema.description}
              </p>
            )}
          </div>
          <button
            onClick={onClose}
            className="p-1.5 rounded-lg text-[#556080] hover:text-white hover:bg-[#1a1a3e] transition-colors"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        {/* Body */}
        <div className="flex-1 overflow-y-auto p-5">
          {loading ? (
            <div className="flex items-center justify-center py-12">
              <Loader2 className="h-6 w-6 text-[#0080ff] animate-spin" />
            </div>
          ) : (
            <ChannelConfigForm
              fields={schema.fields}
              values={values}
              onChange={setValues}
              disabled={saving || deleting}
            />
          )}

          {error && (
            <div className="mt-4 rounded-lg bg-[#ff446615] border border-[#ff446630] p-3 text-xs text-[#ff6680] animate-fade-in">
              {error}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between p-5 border-t border-[#1a1a3e]/40">
          <div>
            {configured && (
              <button
                onClick={handleDelete}
                disabled={saving || deleting}
                className="flex items-center gap-1.5 px-3 py-2 rounded-xl text-xs font-medium text-[#ff4466] border border-[#ff446630] hover:bg-[#ff446615] transition-all disabled:opacity-40"
              >
                {deleting ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <Trash2 className="h-3.5 w-3.5" />
                )}
                Remove
              </button>
            )}
          </div>
          <div className="flex items-center gap-2">
            <button
              onClick={onClose}
              className="px-4 py-2 rounded-xl text-xs font-medium text-[#556080] border border-[#1a1a3e] hover:text-white hover:border-[#0080ff40] transition-all"
            >
              Cancel
            </button>
            <button
              onClick={handleSave}
              disabled={saving || deleting || loading}
              className="btn-electric flex items-center gap-1.5 px-4 py-2 rounded-xl text-xs font-semibold"
            >
              {saving ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Save className="h-3.5 w-3.5" />
              )}
              Save
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
