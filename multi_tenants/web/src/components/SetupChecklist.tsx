import { CheckCircle2, Circle } from 'lucide-react';

interface CheckItem {
  label: string;
  done: boolean;
  detail?: string;
  action?: () => void;
  actionLabel?: string;
}

interface SetupChecklistProps {
  items: CheckItem[];
}

export default function SetupChecklist({ items }: SetupChecklistProps) {
  const doneCount = items.filter(i => i.done).length;
  const total = items.length;
  const pct = total === 0 ? 0 : Math.round((doneCount / total) * 100);

  return (
    <div className="card p-6">
      <h2 className="text-lg font-semibold text-text-primary mb-4">Setup Checklist</h2>
      <ul className="space-y-3 mb-5">
        {items.map((item, idx) => (
          <li key={idx} className="flex items-start gap-3">
            {item.done ? (
              <CheckCircle2 className="h-5 w-5 text-green-400 shrink-0 mt-0.5" />
            ) : (
              <Circle className="h-5 w-5 text-text-muted shrink-0 mt-0.5" />
            )}
            <div className="flex-1 min-w-0">
              <span className={`text-sm font-medium ${item.done ? 'text-text-muted line-through' : 'text-text-primary'}`}>
                {item.label}
              </span>
              {item.done && item.detail && (
                <p className="text-xs text-text-muted mt-0.5">{item.detail}</p>
              )}
              {!item.done && item.action && (
                <button
                  type="button"
                  onClick={item.action}
                  className="mt-1 text-xs text-accent-blue hover:text-accent-blue-hover font-medium transition-colors"
                >
                  {item.actionLabel ?? 'Go'}
                </button>
              )}
            </div>
          </li>
        ))}
      </ul>

      <div>
        <div className="flex justify-between text-xs text-text-muted mb-1">
          <span>{doneCount}/{total} complete</span>
          <span>{pct}%</span>
        </div>
        <div className="w-full bg-gray-800 rounded-full h-2">
          <div
            className="bg-green-500 h-2 rounded-full transition-all"
            style={{ width: `${pct}%` }}
          />
        </div>
      </div>
    </div>
  );
}
