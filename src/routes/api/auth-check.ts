import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import {
  isPasswordProtectionEnabled,
  isAuthenticated,
} from '../../server/auth-middleware'

export const Route = createFileRoute('/api/auth-check')({
  server: {
    handlers: {
      GET: async ({ request }) => {
        const authRequired = isPasswordProtectionEnabled()
        const authenticated = isAuthenticated(request)

        return json({
          authenticated,
          authRequired,
        })
      },
    },
  },
})
