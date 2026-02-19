import { Step } from 'react-joyride'
import { OpenClawStudioIcon } from '@/components/icons/clawsuite'

export const tourSteps: Step[] = [
  // Step 1: Welcome
  {
    target: 'body',
    placement: 'center',
    title: 'Welcome to ClawSuite! 👋',
    content: (
      <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: '12px' }}>
        <OpenClawStudioIcon className="size-12 rounded-xl shadow-sm" />
        <p style={{ textAlign: 'center', margin: 0 }}>
          Your AI-powered command center for managing agents, chats, files, and more. Let&apos;s take a quick tour!
        </p>
      </div>
    ),
    disableBeacon: true,
  },
  // Step 2: Sidebar
  {
    target: '[data-tour="sidebar-container"]',
    placement: 'right',
    title: 'Sidebar Navigation',
    content:
      'Navigate between all your tools here. Collapse/expand sections to customize your workspace.',
  },
  // Step 3: New Session
  {
    target: '[data-tour="new-session"]',
    placement: 'right',
    title: 'Start a New Chat',
    content:
      'Click here to start a new AI chat session. Each conversation is saved automatically.',
  },
  // Step 4: Dashboard
  {
    target: '[data-tour="dashboard"]',
    placement: 'right',
    title: 'Your Dashboard',
    content:
      'Your overview of sessions, usage, and activity. See everything at a glance.',
  },
  // Step 5: Agent Hub
  {
    target: '[data-tour="agent-hub"]',
    placement: 'right',
    title: 'Agent Hub',
    content:
      'Manage your AI agents and configurations. Create custom agents with specialized behaviors.',
  },
  // Step 7: Skills
  {
    target: '[data-tour="skills"]',
    placement: 'right',
    title: 'Skills Library',
    content:
      'Browse and install agent skills to extend capabilities. Add new tools and abilities to your agents.',
  },
  // Step 8: Terminal
  {
    target: '[data-tour="terminal"]',
    placement: 'right',
    title: 'Built-in Terminal',
    content:
      'Built-in terminal for quick commands. Execute shell commands without leaving ClawSuite.',
  },
  // Step 9: Usage Meter (in header)
  {
    target: '[data-tour="usage-meter"]',
    placement: 'bottom',
    title: 'Usage Monitor',
    content:
      'Monitor your AI provider usage in real-time. Track costs and API consumption.',
  },
  // Step 10: Settings
  {
    target: '[data-tour="settings"]',
    placement: 'right',
    title: 'Settings & Customization',
    content:
      'Configure providers, themes, accent colors, and more. Make ClawSuite yours.',
  },
  // Step 11: Finish
  {
    target: 'body',
    placement: 'center',
    title: "You're all set! 🎉",
    content:
      'Start chatting with your AI, explore the tools, and customize ClawSuite to fit your workflow. Need help? Press ? to see all keyboard shortcuts.',
  },
]
