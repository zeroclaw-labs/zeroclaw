import { useState, useCallback, useEffect } from 'react'

const TOUR_STORAGE_KEY = 'clawsuite-onboarding-completed'

export function useOnboardingTour() {
  // Always initialize as false to match server render — read localStorage in useEffect
  const [tourCompleted, setTourCompleted] = useState(false)

  useEffect(() => {
    try {
      if (localStorage.getItem(TOUR_STORAGE_KEY) === 'true') {
        setTourCompleted(true)
      }
    } catch {
      // ignore
    }
  }, [])

  const resetTour = useCallback(() => {
    try {
      localStorage.removeItem(TOUR_STORAGE_KEY)
      setTourCompleted(false)
      // Reload to restart tour
      if (typeof window !== 'undefined') {
        window.location.reload()
      }
    } catch {
      // ignore
    }
  }, [])

  const completeTour = useCallback(() => {
    try {
      localStorage.setItem(TOUR_STORAGE_KEY, 'true')
      setTourCompleted(true)
    } catch {
      // ignore
    }
  }, [])

  return {
    tourCompleted,
    resetTour,
    completeTour,
  }
}
