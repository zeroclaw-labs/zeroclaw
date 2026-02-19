import { createFileRoute, redirect } from '@tanstack/react-router'

export const Route = createFileRoute('/new')({
  beforeLoad: function redirectToNewChat() {
    throw redirect({
      to: '/chat/$sessionKey',
      params: { sessionKey: 'new' },
      replace: true,
    })
  },
  component: function NewChatRoute() {
    return null
  },
})
