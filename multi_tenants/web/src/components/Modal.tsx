import { useEffect, useRef } from 'react';
import { X } from 'lucide-react';

interface Props {
  open: boolean;
  onClose: () => void;
  title: string;
  children: React.ReactNode;
}

export default function Modal({ open, onClose, title, children }: Props) {
  const ref = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    if (open) ref.current?.showModal();
    else ref.current?.close();
  }, [open]);

  return (
    <dialog
      ref={ref}
      onClose={onClose}
      className="fixed inset-0 m-auto bg-bg-card border border-border-default rounded-xl shadow-2xl p-0 backdrop:bg-black/60 backdrop:backdrop-blur-sm max-w-lg w-full max-h-[85vh] overflow-y-auto text-text-primary"
    >
      <div className="p-6">
        <div className="flex justify-between items-center mb-4">
          <h3 className="text-lg font-semibold">{title}</h3>
          <button onClick={onClose} className="text-text-muted hover:text-text-primary transition-colors p-1 rounded-lg hover:bg-bg-card-hover">
            <X className="h-5 w-5" />
          </button>
        </div>
        {children}
      </div>
    </dialog>
  );
}
