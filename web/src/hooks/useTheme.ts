import { useContext } from 'react';
import { ThemeContext } from '../contexts/ThemeContextDef';

export const useTheme = () => useContext(ThemeContext);
