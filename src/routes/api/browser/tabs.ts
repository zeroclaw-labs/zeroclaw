import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { getGatewayTabsResponse } from '../../../server/browser-monitor'

export const Route = createFileRoute('/api/browser/tabs')({
  server: {
    handlers: {
      GET: async () => {
        const payload = await getGatewayTabsResponse()
        return json(payload)
      },
    },
  },
})
