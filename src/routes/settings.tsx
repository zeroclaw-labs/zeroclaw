import { Outlet, createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/settings')({
  component: function SettingsLayoutRoute() {
    return <Outlet />
  },
})
