import { Loader2 } from 'lucide-react';
import Modal from './Modal';

interface Props {
  open: boolean;
  onClose: () => void;
  onConfirm: () => void;
  title: string;
  message: string;
  confirmLabel?: string;
  danger?: boolean;
  loading?: boolean;
}

export default function ConfirmModal({
  open, onClose, onConfirm, title, message,
  confirmLabel = 'Confirm', danger = false, loading = false,
}: Props) {
  return (
    <Modal open={open} onClose={onClose} title={title}>
      <p className="text-sm text-text-secondary mb-6">{message}</p>
      <div className="flex justify-end gap-2">
        <button
          onClick={onClose}
          className="px-4 py-2 text-sm border border-border-default text-text-secondary rounded-lg hover:bg-bg-card-hover transition-colors"
        >
          Cancel
        </button>
        <button
          onClick={onConfirm}
          disabled={loading}
          className={`px-4 py-2 text-sm rounded-lg text-white disabled:opacity-50 transition-colors flex items-center gap-2 ${
            danger ? 'bg-red-600 hover:bg-red-700' : 'bg-accent-blue hover:bg-accent-blue-hover'
          }`}
        >
          {loading && <Loader2 className="h-4 w-4 animate-spin" />}
          {loading ? 'Processing...' : confirmLabel}
        </button>
      </div>
    </Modal>
  );
}
