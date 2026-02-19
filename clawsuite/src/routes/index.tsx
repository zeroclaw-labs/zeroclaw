import { createFileRoute, redirect } from '@tanstack/react-router'

export const Route = createFileRoute('/')({
  beforeLoad: function redirectToWorkspace() {
    const isMobile =
      typeof window !== 'undefined' && window.innerWidth < 768
    throw redirect({
      to: (isMobile ? '/chat/main' : '/dashboard') as string,
      replace: true,
    })
  },
  component: function IndexRoute() {
    return null
  },
})
