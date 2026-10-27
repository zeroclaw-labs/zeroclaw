import { createContext, useContext, type ReactNode } from 'react';
import {
  useLocation,
  useNavigate,
  type Location,
  type NavigateFunction,
} from 'react-router-dom';

// The modal owns only its presentation route. Field values and unsaved edits
// still belong to the normal gateway schema and ConfigDraftProvider.
const ConfigLocation = createContext<{
  location: Location;
  navigate: NavigateFunction;
} | null>(null);

export function ConfigLocationProvider({
  location,
  navigate,
  children,
}: {
  location: Location;
  navigate: NavigateFunction;
  children: ReactNode;
}) {
  return (
    <ConfigLocation.Provider value={{ location, navigate }}>
      {children}
    </ConfigLocation.Provider>
  );
}

export function useConfigLocation() {
  const location = useLocation();
  return useContext(ConfigLocation)?.location ?? location;
}

export function useConfigNavigate() {
  const navigate = useNavigate();
  return useContext(ConfigLocation)?.navigate ?? navigate;
}
