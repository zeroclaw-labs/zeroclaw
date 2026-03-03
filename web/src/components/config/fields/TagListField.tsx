import { useState } from 'react';
import { X } from 'lucide-react';
import type { FieldProps } from '../types';

export default function TagListField({ field, value, onChange }: FieldProps) {
  const [input, setInput] = useState('');
  const tags: string[] = Array.isArray(value) ? value : [];

  const addTag = (tag: string) => {
    const trimmed = tag.trim();
    if (trimmed && !tags.includes(trimmed)) {
      onChange([...tags, trimmed]);
    }
    setInput('');
  };

  const removeTag = (index: number) => {
    onChange(tags.filter((_, i) => i !== index));
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter' || e.key === ',') {
      e.preventDefault();
      addTag(input);
    } else if (e.key === 'Backspace' && input === '' && tags.length > 0) {
      removeTag(tags.length - 1);
    }
  };

  return (
    <div>
      <div className="flex flex-wrap gap-1.5 mb-2">
        {tags.map((tag, i) => (
          <span
            key={tag}
            className="inline-flex items-center gap-1 bg-gray-700 text-gray-200 rounded-full px-2.5 py-0.5 text-xs"
          >
            {tag}
            <button
              type="button"
              onClick={() => removeTag(i)}
              className="text-gray-400 hover:text-white transition-colors"
            >
              <X className="h-3 w-3" />
            </button>
          </span>
        ))}
      </div>
      <input
        type="text"
        value={input}
        onChange={(e) => setInput(e.target.value)}
        onKeyDown={handleKeyDown}
        onBlur={() => { if (input.trim()) addTag(input); }}
        placeholder={field.tagPlaceholder ?? 'Type and press Enter to add'}
        className="w-full bg-gray-800 border border-gray-700 rounded-lg px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500"
      />
    </div>
  );
}
