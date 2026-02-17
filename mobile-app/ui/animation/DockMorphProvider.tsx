import React, { createContext, useCallback, useContext } from "react";
import { useSharedValue, withSpring, type SharedValue } from "react-native-reanimated";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface DockMorphState {
  /** 0 = morph start, 1 = morph complete / idle */
  morphProgress: SharedValue<number>;
  /** Tab count of the outgoing dock */
  prevTabCount: SharedValue<number>;
  /** Active index of the outgoing dock */
  prevActive: SharedValue<number>;
  /** Measured width of the outgoing dock */
  prevWidth: SharedValue<number>;
  /** Capture outgoing dock state before it unmounts */
  captureState: (tabCount: number, active: number, width: number) => void;
  /** Start morph from captured state (called by incoming dock) */
  startMorph: () => void;
}

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

const DockMorphContext = createContext<DockMorphState | null>(null);

const MORPH_SPRING = { damping: 16, stiffness: 140, mass: 1 };

export function DockMorphProvider({ children }: { children: React.ReactNode }) {
  const morphProgress = useSharedValue(1);
  const prevTabCount = useSharedValue(0);
  const prevActive = useSharedValue(0);
  const prevWidth = useSharedValue(0);

  const captureState = useCallback(
    (tabCount: number, active: number, width: number) => {
      prevTabCount.value = tabCount;
      prevActive.value = active;
      prevWidth.value = width;
    },
    [prevTabCount, prevActive, prevWidth],
  );

  const startMorph = useCallback(() => {
    morphProgress.value = 0;
    morphProgress.value = withSpring(1, MORPH_SPRING);
  }, [morphProgress]);

  return (
    <DockMorphContext.Provider
      value={{ morphProgress, prevTabCount, prevActive, prevWidth, captureState, startMorph }}
    >
      {children}
    </DockMorphContext.Provider>
  );
}

export function useDockMorph() {
  return useContext(DockMorphContext);
}
