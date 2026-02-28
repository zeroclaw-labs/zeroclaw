import { useEffect, useMemo, useState } from 'react';
import { ChevronRight, ChevronDown } from 'lucide-react';
import type { SectionDef, FieldDef } from './types';
import TextField from './fields/TextField';
import NumberField from './fields/NumberField';
import ToggleField from './fields/ToggleField';
import SelectField from './fields/SelectField';
import TagListField from './fields/TagListField';

interface Props {
  section: SectionDef;
  getFieldValue: (sectionPath: string, fieldKey: string) => unknown;
  setFieldValue: (sectionPath: string, fieldKey: string, value: unknown) => void;
  isFieldMasked: (sectionPath: string, fieldKey: string) => boolean;
  visibleFields?: FieldDef[];
}

function renderField(
  field: FieldDef,
  value: unknown,
  onChange: (v: unknown) => void,
  isMasked: boolean,
) {
  const props = { field, value, onChange, isMasked };
  switch (field.type) {
    case 'text':
    case 'password':
      return <TextField {...props} />;
    case 'number':
      return <NumberField {...props} />;
    case 'toggle':
      return <ToggleField {...props} />;
    case 'select':
      return <SelectField {...props} />;
    case 'tag-list':
      return <TagListField {...props} />;
    default:
      return <TextField {...props} />;
  }
}

export default function ConfigSection({
  section,
  getFieldValue,
  setFieldValue,
  isFieldMasked,
  visibleFields,
}: Props) {
  const [collapsed, setCollapsed] = useState(section.defaultCollapsed ?? false);
  const sectionPanelId = useMemo(
    () =>
      `config-section-${(section.path || 'root').replace(/[^a-zA-Z0-9_-]/g, '-')}`,
    [section.path],
  );
  const Icon = section.icon;
  const fields = visibleFields ?? section.fields;

  useEffect(() => {
    setCollapsed(section.defaultCollapsed ?? false);
  }, [section.path, section.defaultCollapsed]);

  return (
    <div className="bg-gray-900 rounded-xl border border-gray-800">
      <button
        type="button"
        onClick={() => setCollapsed(!collapsed)}
        aria-expanded={!collapsed}
        aria-controls={sectionPanelId}
        className="w-full flex items-center gap-3 px-4 py-3 hover:bg-gray-800/30 transition-colors rounded-t-xl"
      >
        {collapsed ? (
          <ChevronRight className="h-4 w-4 text-gray-500 flex-shrink-0" />
        ) : (
          <ChevronDown className="h-4 w-4 text-gray-500 flex-shrink-0" />
        )}
        <Icon className="h-4 w-4 text-blue-400 flex-shrink-0" />
        <span className="text-sm font-medium text-white">{section.title}</span>
        {section.description && (
          <span className="text-xs text-gray-500 hidden sm:inline">
            — {section.description}
          </span>
        )}
        <span className="ml-auto text-xs text-gray-600">
          {fields.length} {fields.length === 1 ? 'field' : 'fields'}
        </span>
      </button>

      {!collapsed && (
        <div
          id={sectionPanelId}
          className="border-t border-gray-800 px-4 py-4 grid grid-cols-1 sm:grid-cols-2 gap-x-4 gap-y-4"
        >
          {fields.map((field) => {
            const value = getFieldValue(section.path, field.key);
            const masked = isFieldMasked(section.path, field.key);
            const spanFull = field.type === 'tag-list';

            return (
              <div key={field.key} className={`flex flex-col${spanFull ? ' sm:col-span-2' : ''}`}>
                <label className="flex items-center gap-2 text-sm font-medium text-gray-300 mb-1.5">
                  <span>{field.label}</span>
                  {field.sensitive && (
                    <span className="text-[10px] text-yellow-400 bg-yellow-900/30 border border-yellow-800/50 px-1.5 py-0.5 rounded">
                      sensitive
                    </span>
                  )}
                  {masked && (
                    <span className="text-[10px] text-blue-400 bg-blue-900/30 border border-blue-800/50 px-1.5 py-0.5 rounded">
                      masked
                    </span>
                  )}
                </label>
                {field.description && field.type !== 'text' && field.type !== 'password' && field.type !== 'number' && (
                  <p className="text-xs text-gray-500 mb-1.5">{field.description}</p>
                )}
                <div className="mt-auto">
                  {renderField(
                    field,
                    value,
                    (v) => setFieldValue(section.path, field.key, v),
                    masked,
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
