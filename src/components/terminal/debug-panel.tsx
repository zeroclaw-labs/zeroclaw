import { Cancel01Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { motion } from 'motion/react'
import { BrailleSpinner } from '@/components/ui/braille-spinner'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

export type DebugAnalysis = {
  summary: string
  rootCause: string
  suggestedCommands: Array<{ command: string; description: string }>
  docsLink?: string
}

type DebugPanelProps = {
  analysis: DebugAnalysis | null
  isLoading: boolean
  onClose: () => void
  onRunCommand: (cmd: string) => void
}

export function DebugPanel({
  analysis,
  isLoading,
  onRunCommand,
  onClose,
}: DebugPanelProps) {
  return (
    <motion.aside
      initial={{ x: 400, opacity: 0.92 }}
      animate={{ x: 0, opacity: 1 }}
      transition={{ duration: 0.18, ease: [0.22, 1, 0.36, 1] }}
      className={cn(
        'absolute inset-y-0 right-0 z-40 flex h-full w-[400px] max-w-full translate-x-0 flex-col border-l border-primary-700/40 bg-[#0d0d0d] text-primary-100 shadow-2xl transition-transform duration-200',
      )}
      role="complementary"
      aria-label="Debug analyzer"
    >
      <div className="flex items-center gap-2 border-b border-primary-700/40 px-4 py-3">
        <div className="min-w-0 flex-1">
          <h3 className="text-sm font-medium text-primary-100 text-balance">
            Debug Analyzer
          </h3>
          <p className="text-xs text-primary-400 text-pretty">
            AI-assisted issue diagnosis for the active terminal
          </p>
        </div>
        <Button
          size="icon-sm"
          variant="ghost"
          className="text-primary-300 hover:bg-primary-900 hover:text-primary-100"
          onClick={onClose}
          aria-label="Close debug analyzer panel"
        >
          <HugeiconsIcon icon={Cancel01Icon} size={20} strokeWidth={1.5} />
        </Button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto p-4">
        {isLoading ? (
          <div className="flex items-center gap-2 rounded-lg border border-primary-700/50 bg-primary-900/40 px-3 py-2 text-sm text-primary-300">
            <BrailleSpinner
              preset="claw"
              size={18}
              speed={100}
              className="text-primary-400"
            />
            <span className="text-pretty">Analyzing...</span>
          </div>
        ) : null}

        {!isLoading && analysis ? (
          <div className="space-y-4">
            <section className="rounded-lg border border-accent-500/40 bg-accent-500/10 p-3">
              <h4 className="text-xs font-medium text-accent-200 text-balance">
                Summary
              </h4>
              <p className="mt-1 text-sm text-accent-100 text-pretty">
                {analysis.summary}
              </p>
            </section>

            <section className="rounded-lg border border-primary-700/50 bg-primary-900/40 p-3">
              <h4 className="text-xs font-medium text-primary-300 text-balance">
                Root Cause
              </h4>
              <p className="mt-1 text-sm text-primary-100 text-pretty">
                {analysis.rootCause}
              </p>
            </section>

            <section>
              <h4 className="text-xs font-medium text-primary-300 text-balance">
                Suggested Commands
              </h4>
              {analysis.suggestedCommands.length > 0 ? (
                <ul className="mt-2 space-y-2">
                  {analysis.suggestedCommands.map(
                    function mapCommand(item, index) {
                      return (
                        <li
                          key={`${item.command}-${index}`}
                          className="rounded-lg border border-primary-700/50 bg-primary-900/40 p-3"
                        >
                          <div className="flex items-start gap-2">
                            <code className="min-w-0 flex-1 truncate text-xs text-primary-100 tabular-nums">
                              {item.command}
                            </code>
                            <Button
                              size="sm"
                              variant="ghost"
                              className="h-7 shrink-0 border border-primary-600 px-2 text-xs text-primary-200 hover:bg-primary-800 hover:text-primary-100"
                              onClick={function runCommand() {
                                onRunCommand(item.command)
                              }}
                            >
                              ▶ Run
                            </Button>
                          </div>
                          <p className="mt-2 text-xs text-primary-400 text-pretty">
                            {item.description}
                          </p>
                        </li>
                      )
                    },
                  )}
                </ul>
              ) : (
                <p className="mt-2 text-xs text-primary-500 text-pretty">
                  No command suggestions were returned.
                </p>
              )}
            </section>

            {analysis.docsLink ? (
              <section className="rounded-lg border border-primary-700/50 bg-primary-900/30 p-3">
                <h4 className="text-xs font-medium text-primary-300 text-balance">
                  Documentation
                </h4>
                <a
                  href={analysis.docsLink}
                  target="_blank"
                  rel="noreferrer"
                  className="mt-1 inline-block text-xs text-primary-200 underline decoration-primary-500/60 underline-offset-2"
                >
                  {analysis.docsLink}
                </a>
              </section>
            ) : null}
          </div>
        ) : null}

        {!isLoading && !analysis ? (
          <p className="text-sm text-primary-500 text-pretty">
            Click Debug to analyze the most recent terminal output.
          </p>
        ) : null}
      </div>
    </motion.aside>
  )
}
