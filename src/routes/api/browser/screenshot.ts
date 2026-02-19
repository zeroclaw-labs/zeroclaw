import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { getGatewayScreenshotResponse } from '../../../server/browser-monitor'

export const Route = createFileRoute('/api/browser/screenshot')({
  server: {
    handlers: {
      GET: async ({ request }) => {
        const url = new URL(request.url)
        const tabId = url.searchParams.get('tabId')
        const payload = await getGatewayScreenshotResponse(tabId)
        return json(payload)
      },
    },
  },
})
