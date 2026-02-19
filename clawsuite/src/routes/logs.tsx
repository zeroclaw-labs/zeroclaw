import { createFileRoute, redirect } from '@tanstack/react-router'

export const Route = createFileRoute('/logs')({
  beforeLoad: function redirectLegacyLogsRoute() {
    throw redirect({
      to: '/activity',
      replace: true,
    })
  },
  component: function LogsRoute() {
    return null
  },
})
