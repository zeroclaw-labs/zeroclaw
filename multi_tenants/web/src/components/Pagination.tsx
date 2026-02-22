import { ChevronLeft, ChevronRight } from 'lucide-react';

interface Props {
  page: number;
  total: number;
  perPage: number;
  onChange: (page: number) => void;
}

export default function Pagination({ page, total, perPage, onChange }: Props) {
  const totalPages = Math.ceil(total / perPage);
  if (totalPages <= 1) return null;

  return (
    <div className="flex items-center gap-2 mt-4">
      <button
        onClick={() => onChange(page - 1)}
        disabled={page <= 1}
        className="flex items-center gap-1 px-3 py-1.5 text-sm border border-border-default text-text-secondary rounded-lg hover:bg-bg-card-hover disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
      >
        <ChevronLeft className="h-4 w-4" />
        Prev
      </button>
      <span className="text-sm text-text-muted">
        Page {page} of {totalPages}
      </span>
      <button
        onClick={() => onChange(page + 1)}
        disabled={page >= totalPages}
        className="flex items-center gap-1 px-3 py-1.5 text-sm border border-border-default text-text-secondary rounded-lg hover:bg-bg-card-hover disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
      >
        Next
        <ChevronRight className="h-4 w-4" />
      </button>
    </div>
  );
}
