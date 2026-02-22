import { Loader2 } from 'lucide-react';

const colors: Record<string, string> = {
  running: 'bg-green-900/40 text-green-400 border-green-700/50',
  stopped: 'bg-gray-800 text-gray-400 border-gray-700',
  error: 'bg-red-900/40 text-red-400 border-red-700/50',
  creating: 'bg-yellow-900/40 text-yellow-400 border-yellow-700/50',
  provisioning: 'bg-yellow-900/40 text-yellow-400 border-yellow-700/50',
  starting: 'bg-blue-900/40 text-blue-400 border-blue-700/50',
  draft: 'bg-purple-900/40 text-purple-400 border-purple-700/50',
};

const transitionalStatuses = new Set(['provisioning', 'creating', 'starting'])

export default function StatusBadge({ status }: { status: string }) {
  const cls = colors[status] || 'bg-gray-800 text-gray-400 border-gray-700';
  const isTransitional = transitionalStatuses.has(status);
  return (
    <span className={`inline-flex items-center gap-1.5 px-2.5 py-0.5 rounded-full text-xs font-medium border ${cls}`}>
      {isTransitional && <Loader2 className="h-3 w-3 animate-spin" />}
      {status}
    </span>
  );
}
