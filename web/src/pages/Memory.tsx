import { useState, useEffect } from 'react';
import {
  Brain,
  Search,
  Plus,
  Trash2,
  X,
  Filter,
} from 'lucide-react';
import type { MemoryEntry } from '@/types/api';
import { getMemory, storeMemory, deleteMemory } from '@/lib/api';
import { t } from '@/lib/i18n';

function truncate(text: string, max: number): string {
  if (text.length <= max) return text;
  return text.slice(0, max) + '...';
}

function formatDate(iso: string): string {
  const d = new Date(iso);
  return d.toLocaleString();
}

export default function Memory() {
  const [entries, setEntries] = useState<MemoryEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState('');
  const [categoryFilter, setCategoryFilter] = useState('');
  const [showForm, setShowForm] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);

  // Form state
  const [formKey, setFormKey] = useState('');
  const [formContent, setFormContent] = useState('');
  const [formCategory, setFormCategory] = useState('');
  const [formError, setFormError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const fetchEntries = (q?: string, cat?: string) => {
    setLoading(true);
    getMemory(q || undefined, cat || undefined)
      .then(setEntries)
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    fetchEntries();
  }, []);

  const handleSearch = () => {
    fetchEntries(search, categoryFilter);
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') handleSearch();
  };

  const categories = Array.from(new Set(entries.map((e) => e.category))).sort();

  const handleAdd = async () => {
    if (!formKey.trim() || !formContent.trim()) {
      setFormError(t('memory.validation_error'));
      return;
    }
    setSubmitting(true);
    setFormError(null);
    try {
      await storeMemory(
        formKey.trim(),
        formContent.trim(),
        formCategory.trim() || undefined,
      );
      fetchEntries(search, categoryFilter);
      setShowForm(false);
      setFormKey('');
      setFormContent('');
      setFormCategory('');
    } catch (err: unknown) {
      setFormError(err instanceof Error ? err.message : t('memory.store_error'));
    } finally {
      setSubmitting(false);
    }
  };

  const handleDelete = async (key: string) => {
    try {
      await deleteMemory(key);
      setEntries((prev) => prev.filter((e) => e.key !== key));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : t('memory.delete_error'));
    } finally {
      setConfirmDelete(null);
    }
  };

  if (error && entries.length === 0) {
    return (
      <div className="p-6 animate-fade-in">
        <div className="rounded-xl bg-[#ff446615] border border-[#ff446630] p-4 text-[#ff6680]">
          {t('memory.load_error')}: {error}
        </div>
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Brain className="h-5 w-5 text-[#0080ff]" />
          <h2 className="text-sm font-semibold text-white uppercase tracking-wider">
            {t('memory.memory_title')} ({entries.length})
          </h2>
        </div>
        <button
          onClick={() => setShowForm(true)}
          className="btn-electric flex items-center gap-2 text-sm px-4 py-2"
        >
          <Plus className="h-4 w-4" />
          {t('memory.add_memory')}
        </button>
      </div>

      {/* Search and Filter */}
      <div className="flex flex-col sm:flex-row gap-3">
        <div className="relative flex-1">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-[#334060]" />
          <input
            type="text"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder={t('memory.search_placeholder')}
            className="input-electric w-full pl-10 pr-4 py-2.5 text-sm"
          />
        </div>
        <div className="relative">
          <Filter className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-[#334060]" />
          <select
            value={categoryFilter}
            onChange={(e) => setCategoryFilter(e.target.value)}
            className="input-electric pl-10 pr-8 py-2.5 text-sm appearance-none cursor-pointer"
          >
            <option value="">{t('memory.all_categories')}</option>
            {categories.map((cat) => (
              <option key={cat} value={cat}>
                {cat}
              </option>
            ))}
          </select>
        </div>
        <button
          onClick={handleSearch}
          className="btn-electric px-4 py-2.5 text-sm"
        >
          {t('memory.search_button')}
        </button>
      </div>

      {/* Error banner (non-fatal) */}
      {error && (
        <div className="rounded-xl bg-[#ff446615] border border-[#ff446630] p-3 text-sm text-[#ff6680] animate-fade-in">
          {error}
        </div>
      )}

      {/* Add Memory Form Modal */}
      {showForm && (
        <div className="fixed inset-0 modal-backdrop flex items-center justify-center z-50">
          <div className="glass-card p-6 w-full max-w-md mx-4 animate-fade-in-scale">
            <div className="flex items-center justify-between mb-4">
              <h3 className="text-lg font-semibold text-white">{t('memory.add_modal_title')}</h3>
              <button
                onClick={() => {
                  setShowForm(false);
                  setFormError(null);
                }}
                className="text-[#556080] hover:text-white transition-colors duration-300"
              >
                <X className="h-5 w-5" />
              </button>
            </div>

            {formError && (
              <div className="mb-4 rounded-xl bg-[#ff446615] border border-[#ff446630] p-3 text-sm text-[#ff6680] animate-fade-in">
                {formError}
              </div>
            )}

            <div className="space-y-4">
              <div>
                <label className="block text-xs font-semibold text-[#8892a8] mb-1.5 uppercase tracking-wider">
                  {t('memory.key_required')} <span className="text-[#ff4466]">*</span>
                </label>
                <input
                  type="text"
                  value={formKey}
                  onChange={(e) => setFormKey(e.target.value)}
                  placeholder="e.g. user_preferences"
                  className="input-electric w-full px-3 py-2.5 text-sm"
                />
              </div>
              <div>
                <label className="block text-xs font-semibold text-[#8892a8] mb-1.5 uppercase tracking-wider">
                  {t('memory.content_required')} <span className="text-[#ff4466]">*</span>
                </label>
                <textarea
                  value={formContent}
                  onChange={(e) => setFormContent(e.target.value)}
                  placeholder="Memory content..."
                  rows={4}
                  className="input-electric w-full px-3 py-2.5 text-sm resize-none"
                />
              </div>
              <div>
                <label className="block text-xs font-semibold text-[#8892a8] mb-1.5 uppercase tracking-wider">
                  {t('memory.category_optional')}
                </label>
                <input
                  type="text"
                  value={formCategory}
                  onChange={(e) => setFormCategory(e.target.value)}
                  placeholder="e.g. preferences, context, facts"
                  className="input-electric w-full px-3 py-2.5 text-sm"
                />
              </div>
            </div>

            <div className="flex justify-end gap-3 mt-6">
              <button
                onClick={() => {
                  setShowForm(false);
                  setFormError(null);
                }}
                className="px-4 py-2 text-sm font-medium text-[#8892a8] hover:text-white border border-[#1a1a3e] rounded-xl hover:bg-[#0080ff08] transition-all duration-300"
              >
                {t('memory.cancel')}
              </button>
              <button
                onClick={handleAdd}
                disabled={submitting}
                className="btn-electric px-4 py-2 text-sm font-medium"
              >
                {submitting ? t('memory.saving') : t('common.save')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Memory Table */}
      {loading ? (
        <div className="flex items-center justify-center h-32">
          <div className="h-8 w-8 border-2 border-[#0080ff30] border-t-[#0080ff] rounded-full animate-spin" />
        </div>
      ) : entries.length === 0 ? (
        <div className="glass-card p-8 text-center">
          <Brain className="h-10 w-10 text-[#1a1a3e] mx-auto mb-3" />
          <p className="text-[#556080]">{t('memory.empty')}</p>
        </div>
      ) : (
        <div className="glass-card overflow-x-auto">
          <table className="table-electric">
            <thead>
              <tr>
                <th className="text-left">{t('memory.key')}</th>
                <th className="text-left">{t('memory.content')}</th>
                <th className="text-left">{t('memory.category')}</th>
                <th className="text-left">{t('memory.timestamp')}</th>
                <th className="text-right">{t('common.actions')}</th>
              </tr>
            </thead>
            <tbody>
              {entries.map((entry) => (
                <tr key={entry.id}>
                  <td className="px-4 py-3 text-white font-medium font-mono text-xs">
                    {entry.key}
                  </td>
                  <td className="px-4 py-3 text-[#8892a8] max-w-[300px] text-sm">
                    <span title={entry.content}>
                      {truncate(entry.content, 80)}
                    </span>
                  </td>
                  <td className="px-4 py-3">
                    <span className="inline-flex items-center px-2.5 py-0.5 rounded-full text-[10px] font-semibold capitalize border border-[#1a1a3e] text-[#8892a8]" style={{ background: 'rgba(0,128,255,0.06)' }}>
                      {entry.category}
                    </span>
                  </td>
                  <td className="px-4 py-3 text-[#556080] text-xs whitespace-nowrap">
                    {formatDate(entry.timestamp)}
                  </td>
                  <td className="px-4 py-3 text-right">
                    {confirmDelete === entry.key ? (
                      <div className="flex items-center justify-end gap-2 animate-fade-in">
                        <span className="text-xs text-[#ff4466]">{t('memory.delete_confirm')}</span>
                        <button
                          onClick={() => handleDelete(entry.key)}
                          className="text-[#ff4466] hover:text-[#ff6680] text-xs font-medium"
                        >
                          {t('memory.yes')}
                        </button>
                        <button
                          onClick={() => setConfirmDelete(null)}
                          className="text-[#556080] hover:text-white text-xs font-medium"
                        >
                          {t('memory.no')}
                        </button>
                      </div>
                    ) : (
                      <button
                        onClick={() => setConfirmDelete(entry.key)}
                        className="text-[#334060] hover:text-[#ff4466] transition-all duration-300"
                      >
                        <Trash2 className="h-4 w-4" />
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
