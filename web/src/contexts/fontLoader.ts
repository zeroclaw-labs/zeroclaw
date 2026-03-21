const loaded: Set<string> = new Set();

export function loadGoogleFont(family: string, weights: string = '400;500;600') {
  const id = `gfont-${family.replace(/\s+/g, '-').toLowerCase()}`;
  if (loaded.has(id)) return;
  loaded.add(id);

  const link = document.createElement('link');
  link.id = id;
  link.rel = 'stylesheet';
  link.href = `https://fonts.googleapis.com/css2?family=${encodeURIComponent(family)}:wght@${weights}&display=swap`;
  document.head.appendChild(link);
}

export function loadUiFont(font: string) {
  if (font === 'inter') loadGoogleFont('Inter');
  if (font === 'segoe') loadGoogleFont('Segoe UI');
  if (font === 'sf') loadGoogleFont('SF Pro Text');
}

export function loadMonoFont(font: string) {
  if (font === 'jetbrains') loadGoogleFont('JetBrains Mono');
  if (font === 'fira') loadGoogleFont('Fira Code');
  if (font === 'cascadia') loadGoogleFont('Cascadia Code');
}
