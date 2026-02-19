import { HugeiconsIcon } from '@hugeicons/react'
import {
  BrainIcon,
  CodeIcon,
  Edit02Icon,
  Search01Icon,
} from '@hugeicons/core-free-icons'
import { motion } from 'motion/react'
import { OpenClawStudioIcon } from '@/components/icons/clawsuite'

type SuggestionChip = {
  label: string
  prompt: string
  icon: unknown
}

const SUGGESTIONS: SuggestionChip[] = [
  {
    label: 'Write code',
    prompt: 'Help me write a function that',
    icon: CodeIcon,
  },
  {
    label: 'Research',
    prompt: 'Search the web for',
    icon: Search01Icon,
  },
  {
    label: 'Analyze',
    prompt: 'Analyze this and give me insights:',
    icon: BrainIcon,
  },
  {
    label: 'Draft',
    prompt: 'Help me draft a',
    icon: Edit02Icon,
  },
]

type ChatEmptyStateProps = {
  onSuggestionClick?: (prompt: string) => void
  compact?: boolean
}

export function ChatEmptyState({
  onSuggestionClick,
  compact,
}: ChatEmptyStateProps) {
  return (
    <motion.div
      initial={{ opacity: 0, y: 8 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: 0.3, ease: 'easeOut' }}
      className="flex h-full flex-col items-center justify-center px-3 py-4 md:px-4 md:py-0"
    >
      <div className="flex flex-col items-center text-center">
        <motion.div
          initial={{ scale: 0.9 }}
          animate={{ scale: 1 }}
          transition={{ type: 'spring', damping: 20, stiffness: 300 }}
        >
          <OpenClawStudioIcon className="mb-3 size-10 rounded-xl opacity-60 md:mb-4 md:size-12" />
        </motion.div>

        {!compact && (
          <>
            <h2 className="mb-1 text-base font-medium text-primary-900 text-balance md:text-lg">
              How can I help?
            </h2>
            <p className="mb-4 max-w-sm px-1 text-xs text-primary-500 text-pretty md:mb-6 md:px-0 md:text-sm">
              Ask me anything — write code, research topics, analyze data, or
              automate tasks.
            </p>
          </>
        )}

        {compact && (
          <p className="mb-3 text-xs text-primary-500 text-pretty md:mb-4 md:text-sm">
            How can I help?
          </p>
        )}

        <div className="flex flex-wrap justify-center gap-1.5 md:gap-2">
          {SUGGESTIONS.map((suggestion) => (
            <motion.button
              key={suggestion.label}
              whileHover={{ scale: 1.02 }}
              whileTap={{ scale: 0.98 }}
              onClick={() => onSuggestionClick?.(suggestion.prompt)}
              className="flex items-center gap-1.5 rounded-full border border-primary-200 bg-primary-50 px-3 py-1.5 text-xs text-primary-700 transition-colors hover:border-primary-300 hover:bg-primary-100 md:gap-2 md:px-3.5 md:py-2 md:text-sm"
            >
              <HugeiconsIcon
                icon={suggestion.icon as any}
                size={14}
                strokeWidth={1.5}
                className="text-primary-500"
              />
              {suggestion.label}
            </motion.button>
          ))}
        </div>
      </div>
    </motion.div>
  )
}
