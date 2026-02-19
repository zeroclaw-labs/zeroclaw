import { createFileRoute, Link } from '@tanstack/react-router'
import { usePageTitle } from '@/hooks/use-page-title'
import { HugeiconsIcon } from '@hugeicons/react'
import { Home01Icon, ArrowLeft01Icon } from '@hugeicons/core-free-icons'
import { buttonVariants } from '@/components/ui/button'

export const Route = createFileRoute('/$')({
  component: NotFoundPage,
})

function NotFoundPage() {
  usePageTitle('404 — Not Found')

  return (
    <div className="flex flex-col items-center justify-center min-h-screen p-6 text-center bg-primary-50">
      <div className="max-w-md">
        {/* 404 Icon */}
        <div className="mb-6 flex items-center justify-center">
          <div className="relative">
            <div className="text-8xl font-bold text-accent-500/20 select-none">
              404
            </div>
            <div className="absolute inset-0 flex items-center justify-center">
              <div className="h-16 w-16 rounded-full bg-accent-500/10 flex items-center justify-center">
                <span className="text-4xl">🔍</span>
              </div>
            </div>
          </div>
        </div>

        {/* Message */}
        <h1 className="text-2xl font-semibold text-primary-900 mb-2">
          Page Not Found
        </h1>
        <p className="text-primary-600 mb-8">
          The page you're looking for doesn't exist or has been moved.
        </p>

        {/* Actions */}
        <div className="flex flex-col sm:flex-row items-center justify-center gap-3">
          <button
            onClick={() => window.history.back()}
            className={buttonVariants({ variant: 'outline', size: 'default' })}
          >
            <HugeiconsIcon icon={ArrowLeft01Icon} size={18} strokeWidth={1.5} />
            Go Back
          </button>
          <Link
            to="/dashboard"
            className={buttonVariants({ variant: 'default', size: 'default' })}
          >
            <HugeiconsIcon icon={Home01Icon} size={18} strokeWidth={1.5} />
            Dashboard
          </Link>
        </div>

        {/* Helpful Links */}
        <div className="mt-12 pt-8 border-t border-primary-200">
          <p className="text-sm text-primary-500 mb-3">Quick Links</p>
          <div className="flex flex-wrap items-center justify-center gap-4 text-sm">
            <Link
              to={'/chat/main' as string}
              className="text-accent-500 hover:text-accent-600 hover:underline"
            >
              Chat
            </Link>
            <Link
              to="/dashboard"
              className="text-accent-500 hover:text-accent-600 hover:underline"
            >
              Dashboard
            </Link>
            <Link
              to="/files"
              className="text-accent-500 hover:text-accent-600 hover:underline"
            >
              Files
            </Link>
            <Link
              to="/terminal"
              className="text-accent-500 hover:text-accent-600 hover:underline"
            >
              Terminal
            </Link>
          </div>
        </div>
      </div>
    </div>
  )
}
