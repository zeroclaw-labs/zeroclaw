import { useState, useMemo } from 'react';
import { Search } from 'lucide-react';
import { CONFIG_SECTIONS } from './configSections';
import ConfigSection from './ConfigSection';
import type { FieldDef } from './types';

const CATEGORY_ORDER = [
  { key: 'all', label: 'All' },
  { key: 'general', label: 'General' },
  { key: 'security', label: 'Security' },
  { key: 'channels', label: 'Channels' },
  { key: 'runtime', label: 'Runtime' },
  { key: 'tools', label: 'Tools' },
  { key: 'memory', label: 'Memory' },
  { key: 'network', label: 'Network' },
  { key: 'advanced', label: 'Advanced' },
] as const;

interface Props {
  getFieldValue: (sectionPath: string, fieldKey: string) => unknown;
  setFieldValue: (sectionPath: string, fieldKey: string, value: unknown) => void;
  isFieldMasked: (sectionPath: string, fieldKey: string) => boolean;
}

export default function ConfigFormEditor({
  getFieldValue,
  setFieldValue,
  isFieldMasked,
}: Props) {
  const [search, setSearch] = useState('');
  const [activeCategory, setActiveCategory] = useState('all');

  const isSearching = search.trim().length > 0;

  const filteredSections = useMemo(() => {
    if (isSearching) {
      const q = search.toLowerCase();
      return CONFIG_SECTIONS.map((section) => {
        const titleMatch = section.title.toLowerCase().includes(q);
        const descMatch = section.description?.toLowerCase().includes(q);

        if (titleMatch || descMatch) {
          return { section, fields: undefined };
        }

        const matchingFields = section.fields.filter(
          (f: FieldDef) =>
            f.label.toLowerCase().includes(q) ||
            f.key.toLowerCase().includes(q) ||
            f.description?.toLowerCase().includes(q),
        );

        if (matchingFields.length > 0) {
          return { section, fields: matchingFields };
        }

        return null;
      }).filter(Boolean) as { section: (typeof CONFIG_SECTIONS)[0]; fields: FieldDef[] | undefined }[];
    }

    // Category filter
    const sections = activeCategory === 'all'
      ? CONFIG_SECTIONS
      : CONFIG_SECTIONS.filter((s) => s.category === activeCategory);

    return sections.map((s) => ({ section: s, fields: undefined }));
  }, [search, isSearching, activeCategory]);

  return (
    <div className="space-y-3">
      {/* Search */}
      <div className="relative">
        <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-gray-500" />
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search config fields..."
          className="w-full bg-gray-800 border border-gray-700 rounded-lg pl-9 pr-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500"
        />
      </div>

      {/* Category pills — hidden during search */}
      {!isSearching && (
        <div className="flex flex-wrap gap-2">
          {CATEGORY_ORDER.map(({ key, label }) => (
            <button
              key={key}
              onClick={() => setActiveCategory(key)}
              className={`px-3 py-1 rounded-lg text-sm font-medium transition-colors ${
                activeCategory === key
                  ? 'bg-blue-600 text-white'
                  : 'bg-gray-900 text-gray-400 border border-gray-700 hover:bg-gray-800 hover:text-gray-200'
              }`}
            >
              {label}
            </button>
          ))}
        </div>
      )}

      {/* Sections */}
      {filteredSections.length === 0 ? (
        <div className="text-center py-12 text-gray-500 text-sm">
          No matching config fields found.
        </div>
      ) : (
        filteredSections.map(({ section, fields }) => (
          <ConfigSection
            key={section.path || '_root'}
            section={fields ? { ...section, defaultCollapsed: false } : section}
            getFieldValue={getFieldValue}
            setFieldValue={setFieldValue}
            isFieldMasked={isFieldMasked}
            visibleFields={fields}
          />
        ))
      )}
    </div>
  );
}
