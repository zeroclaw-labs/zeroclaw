import {
  Settings,
  Save,
  CheckCircle,
  AlertTriangle,
  ShieldAlert,
  FileText,
  SlidersHorizontal,
} from 'lucide-react';
import { useConfigForm, type EditorMode } from '@/components/config/useConfigForm';
import ConfigFormEditor from '@/components/config/ConfigFormEditor';
import ConfigRawEditor from '@/components/config/ConfigRawEditor';

function ModeTab({
  mode,
  active,
  icon: Icon,
  label,
  onClick,
}: {
  mode: EditorMode;
  active: boolean;
  icon: React.ComponentType<{ className?: string }>;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className={`flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium transition-colors ${
        active
          ? 'bg-blue-600 text-white'
          : 'text-gray-400 hover:text-gray-200 hover:bg-gray-800'
      }`}
      aria-pressed={active}
      data-mode={mode}
    >
      <Icon className="h-3.5 w-3.5" />
      {label}
    </button>
  );
}

export default function Config() {
  const {
    loading,
    saving,
    error,
    success,
    mode,
    rawToml,
    setMode,
    getFieldValue,
    setFieldValue,
    isFieldMasked,
    setRawToml,
    save,
  } = useConfigForm();

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="animate-spin rounded-full h-8 w-8 border-2 border-blue-500 border-t-transparent" />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Settings className="h-5 w-5 text-blue-400" />
          <h2 className="text-base font-semibold text-white">Configuration</h2>
        </div>
        <div className="flex items-center gap-3">
          {/* Mode toggle */}
          <div className="flex items-center gap-1 bg-gray-900 border border-gray-800 rounded-lg p-0.5">
            <ModeTab
              mode="form"
              active={mode === 'form'}
              icon={SlidersHorizontal}
              label="Form"
              onClick={() => setMode('form')}
            />
            <ModeTab
              mode="raw"
              active={mode === 'raw'}
              icon={FileText}
              label="Raw"
              onClick={() => setMode('raw')}
            />
          </div>

          <button
            onClick={save}
            disabled={saving}
            className="flex items-center gap-2 bg-blue-600 hover:bg-blue-700 text-white text-sm font-medium px-4 py-2 rounded-lg transition-colors disabled:opacity-50"
          >
            <Save className="h-4 w-4" />
            {saving ? 'Saving...' : 'Save'}
          </button>
        </div>
      </div>

      {/* Sensitive fields note */}
      <div className="flex items-start gap-3 bg-yellow-900/20 border border-yellow-700/40 rounded-lg p-4">
        <ShieldAlert className="h-5 w-5 text-yellow-400 flex-shrink-0 mt-0.5" />
        <div>
          <p className="text-sm text-yellow-300 font-medium">
            Sensitive fields are masked
          </p>
          <p className="text-sm text-yellow-400/70 mt-0.5">
            {mode === 'form'
              ? 'Masked fields show "Configured (masked)" as a placeholder. Leave them untouched to preserve existing values, or enter a new value to update.'
              : 'API keys, tokens, and passwords are hidden for security. To update a masked field, replace the entire masked value with your new value.'}
          </p>
        </div>
      </div>

      {/* Success message */}
      {success && (
        <div className="flex items-center gap-2 bg-green-900/30 border border-green-700 rounded-lg p-3">
          <CheckCircle className="h-4 w-4 text-green-400 flex-shrink-0" />
          <span className="text-sm text-green-300">{success}</span>
        </div>
      )}

      {/* Error message */}
      {error && (
        <div className="flex items-center gap-2 bg-red-900/30 border border-red-700 rounded-lg p-3">
          <AlertTriangle className="h-4 w-4 text-red-400 flex-shrink-0" />
          <span className="text-sm text-red-300">{error}</span>
        </div>
      )}

      {/* Editor */}
      {mode === 'form' ? (
        <ConfigFormEditor
          getFieldValue={getFieldValue}
          setFieldValue={setFieldValue}
          isFieldMasked={isFieldMasked}
        />
      ) : (
        <ConfigRawEditor
          rawToml={rawToml}
          onChange={setRawToml}
          disabled={saving}
        />
      )}
    </div>
  );
}
