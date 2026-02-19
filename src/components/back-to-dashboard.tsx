import { ArrowLeft01Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useNavigate } from '@tanstack/react-router'
import { Button } from '@/components/ui/button'

export function BackToDashboard() {
  const navigate = useNavigate()

  function handleBackToDashboard() {
    void navigate({ to: '/dashboard' })
  }

  return (
    <Button
      variant="ghost"
      size="sm"
      onClick={handleBackToDashboard}
      className="w-fit gap-1.5"
    >
      <HugeiconsIcon icon={ArrowLeft01Icon} size={20} strokeWidth={1.5} />
      Back to Dashboard
    </Button>
  )
}
