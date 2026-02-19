import { lazy, Suspense } from 'react'
import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { usePageTitle } from '@/hooks/use-page-title'

const TerminalWorkspace = lazy(() =>
  import('@/components/terminal/terminal-workspace').then((m) => ({
    default: m.TerminalWorkspace,
  })),
)

export const Route = createFileRoute('/terminal')({
  component: TerminalRoute,
  errorComponent: function TerminalError({ error }) {
    return (
      <div className="flex flex-col items-center justify-center h-full p-6 text-center bg-primary-50">
        <h2 className="text-xl font-semibold text-primary-900 mb-3">
          Terminal Error
        </h2>
        <p className="text-sm text-primary-600 mb-4 max-w-md">
          {error instanceof Error
            ? error.message
            : 'Failed to initialize terminal'}
        </p>
        <button
          onClick={() => window.location.reload()}
          className="px-4 py-2 bg-accent-500 text-white rounded-lg hover:bg-accent-600 transition-colors"
        >
          Reload Terminal
        </button>
      </div>
    )
  },
})

function TerminalRoute() {
  usePageTitle('Terminal')
  const navigate = useNavigate()

  function handleBack() {
    if (window.history.length > 1) {
      window.history.back()
      return
    }
    navigate({
      to: '/chat/$sessionKey',
      params: { sessionKey: 'main' },
      replace: true,
    })
  }

  return (
    <div className="box-border h-full min-h-0 overflow-hidden bg-surface pb-24 text-primary-900 md:pb-0">
      <Suspense
        fallback={
          <div className="flex h-full min-h-0 items-center justify-center text-xs text-primary-500">
            Loading terminal…
          </div>
        }
      >
        <TerminalWorkspace mode="fullscreen" onBack={handleBack} />
      </Suspense>
    </div>
  )
}
