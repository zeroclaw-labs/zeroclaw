import { useState } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { sopProposalSource } from '@/lib/sopDraft';
import { t } from '@/lib/i18n';

export default function SopProposal({
  message,
  onApply,
}: {
  message: string;
  onApply: (source: string) => Promise<void>;
}) {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  const [applied, setApplied] = useState(false);
  const source = sopProposalSource(message);
  if (!source) return null;
  return (
    <div className="mt-3 space-y-2">
      <button
        type="button"
        disabled={pending}
        className="btn-secondary px-3 py-2 text-xs disabled:opacity-40"
        onClick={() => {
          setPending(true);
          setError('');
          setApplied(false);
          void onApply(source)
            .then(() => setApplied(true))
            .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
            .finally(() => setPending(false));
        }}
      >
        {t(pending ? 'common.loading' : 'sop_workspace.apply_proposal')}
      </button>
      {applied && (
        <p role="status" className="text-xs text-pc-text-muted">
          {t('sop_workspace.proposal_applied')}
        </p>
      )}
      {error && (
        <p role="alert" className="whitespace-pre-wrap text-xs text-status-error">
          {error}
        </p>
      )}
    </div>
  );
}

/** Keep the exact sent context available without drowning out the conversation. */
export function SopPromptMessage({ message }: { message: string }) {
  const separator = `\n\n${t('sop_workspace.assistant_instructions')}\n\n`;
  const boundary = message.lastIndexOf(separator);
  if (boundary < 0) return <ReactMarkdown remarkPlugins={[remarkGfm]}>{message}</ReactMarkdown>;
  return (
    <>
      <ReactMarkdown remarkPlugins={[remarkGfm]}>{message.slice(0, boundary)}</ReactMarkdown>
      <details className="mt-3 whitespace-normal text-xs text-pc-text-muted">
        <summary className="cursor-pointer">{t('sop_workspace.context_sent')}</summary>
        <pre className="mt-2 max-h-40 overflow-auto whitespace-pre-wrap break-words">
          {message.slice(boundary + 2)}
        </pre>
      </details>
    </>
  );
}

export function SopAssistantMessage({
  message,
  onApply,
}: {
  message: string;
  onApply: (source: string) => Promise<void>;
}) {
  const source = sopProposalSource(message);
  return (
    <>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={
          source
            ? {
                pre: ({ children }) => (
                  <details className="my-3 rounded-lg border border-pc-border p-3">
                    <summary className="cursor-pointer text-xs text-pc-text-muted">
                      {t('sop_workspace.proposal_source')}
                    </summary>
                    <pre>{children}</pre>
                  </details>
                ),
              }
            : undefined
        }
      >
        {message}
      </ReactMarkdown>
      <SopProposal message={message} onApply={onApply} />
    </>
  );
}
