import type { HugeiconsIcon } from '@hugeicons/react'
import {
  Home01Icon,
  Message01Icon,
  DashboardSquare01Icon,
  BrowserIcon,
  Rocket01Icon,
  Folder01Icon,
  Settings01Icon,
} from '@hugeicons/core-free-icons'
import type * as React from 'react'

type IconType = React.ComponentProps<typeof HugeiconsIcon>['icon']

export type OnboardingStep = {
  id: string
  title: string
  description: string
  icon: IconType
  iconBg: string
}

export const ONBOARDING_STEPS: OnboardingStep[] = [
  {
    id: 'welcome',
    title: 'Welcome to ClawSuite',
    description:
      "Your intelligent workspace for AI-powered automation. Let's take a quick tour of what you can do.",
    icon: Home01Icon,
    iconBg: 'bg-orange-500',
  },
  {
    id: 'chat',
    title: 'AI Chat Interface',
    description:
      'Have natural conversations with powerful AI models. Create multiple sessions, search with ⌘K, and let AI handle complex tasks.',
    icon: Message01Icon,
    iconBg: 'bg-blue-500',
  },
  {
    id: 'dashboard',
    title: 'Dashboard & Widgets',
    description:
      'Track your usage, monitor active tasks, and customize your workspace with interactive widgets.',
    icon: DashboardSquare01Icon,
    iconBg: 'bg-emerald-500',
  },
  {
    id: 'browser-terminal',
    title: 'Browser & Terminal',
    description:
      'Built-in browser automation and terminal access. Let AI browse the web and execute commands on your behalf.',
    icon: BrowserIcon,
    iconBg: 'bg-purple-500',
  },
  {
    id: 'agent-swarm',
    title: 'Agent Swarm',
    description:
      'Multi-agent orchestration for complex workflows. Spawn specialized agents that work together. Coming soon.',
    icon: Rocket01Icon,
    iconBg: 'bg-pink-500',
  },
  {
    id: 'files-memory',
    title: 'Files & Memory',
    description:
      'Browse workspace files and access agent memory. Your AI assistant remembers context across sessions.',
    icon: Folder01Icon,
    iconBg: 'bg-amber-500',
  },
  {
    id: 'providers',
    title: 'Models & Providers',
    description:
      'Configure AI providers like OpenAI, Anthropic, and more. Choose the perfect model for each task.',
    icon: Settings01Icon,
    iconBg: 'bg-cyan-500',
  },
]

export const STORAGE_KEY = 'openclaw-onboarding-complete'
