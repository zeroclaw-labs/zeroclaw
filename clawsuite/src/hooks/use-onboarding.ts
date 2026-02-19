import { create } from 'zustand'
import {
  STORAGE_KEY,
  ONBOARDING_STEPS,
} from '@/components/onboarding/onboarding-steps'

type OnboardingState = {
  isOpen: boolean
  currentStep: number
  totalSteps: number
  _initialized: boolean
  /** Check localStorage (client-only, runs once) and open wizard if not completed */
  initialize: () => void
  /** Go to next step */
  nextStep: () => void
  /** Go to previous step */
  prevStep: () => void
  /** Go to specific step */
  goToStep: (step: number) => void
  /** Complete onboarding, set flag, and close */
  complete: () => void
  /** Skip onboarding immediately */
  skip: () => void
  /** Reset onboarding (for testing) */
  reset: () => void
}

export const useOnboardingStore = create<OnboardingState>((set, get) => ({
  // Start closed — initialize() opens it on client if not completed
  isOpen: false,
  currentStep: 0,
  totalSteps: ONBOARDING_STEPS.length,
  _initialized: false,

  initialize: () => {
    // Only run once per app lifetime, only on client
    if (get()._initialized) return
    set({ _initialized: true })
    if (typeof window === 'undefined') return
    try {
      const completed = localStorage.getItem(STORAGE_KEY) === 'true'
      if (!completed) {
        set({ isOpen: true, currentStep: 0 })
      }
    } catch {
      // ignore localStorage errors (SSR, privacy mode, etc.)
    }
  },

  nextStep: () => {
    const { currentStep, totalSteps } = get()
    if (currentStep < totalSteps - 1) {
      set({ currentStep: currentStep + 1 })
    }
  },

  prevStep: () => {
    const { currentStep } = get()
    if (currentStep > 0) {
      set({ currentStep: currentStep - 1 })
    }
  },

  goToStep: (step: number) => {
    const { totalSteps } = get()
    if (step >= 0 && step < totalSteps) {
      set({ currentStep: step })
    }
  },

  complete: () => {
    localStorage.setItem(STORAGE_KEY, 'true')
    set({ isOpen: false })
  },

  skip: () => {
    localStorage.setItem(STORAGE_KEY, 'true')
    set({ isOpen: false })
  },

  reset: () => {
    localStorage.removeItem(STORAGE_KEY)
    set({ isOpen: true, currentStep: 0 })
  },
}))
