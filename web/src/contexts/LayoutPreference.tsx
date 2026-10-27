import { createContext, useContext, useState, type ReactNode } from 'react';

export type LayoutMode = 'classic' | 'focused';
const STORAGE_KEY = 'zeroclaw-layout';

function loadLayout(): LayoutMode {
  try {
    return localStorage.getItem(STORAGE_KEY) === 'focused' ? 'focused' : 'classic';
  } catch {
    return 'classic';
  }
}

const LayoutPreference = createContext<{ layout: LayoutMode; setLayout: (mode: LayoutMode) => void }>({
  layout: 'classic', setLayout: () => {},
});
export const useLayoutPreference = () => useContext(LayoutPreference);

/** Creates only the browser's presentation choice. Runtime state has its existing owners. */
export function LayoutPreferenceProvider({ children }: { children: ReactNode }) {
  const [layout, setValue] = useState(loadLayout);
  const setLayout = (mode: LayoutMode) => {
    setValue(mode);
    try { localStorage.setItem(STORAGE_KEY, mode); } catch { /* Session-only choice when storage is unavailable. */ }
  };
  return <LayoutPreference.Provider value={{ layout, setLayout }}>{children}</LayoutPreference.Provider>;
}
