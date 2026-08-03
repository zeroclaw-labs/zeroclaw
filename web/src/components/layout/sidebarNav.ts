function matchesNavPath(pathname: string, navPath: string): boolean {
  if (navPath === "/") {
    return pathname === "/";
  }

  return pathname === navPath || pathname.startsWith(`${navPath}/`);
}

export function findActiveNavPath(
  pathname: string,
  navPaths: readonly string[],
): string | null {
  const normalizedPathname = pathname.toLowerCase();
  let activePath: string | null = null;

  for (const navPath of navPaths) {
    if (
      matchesNavPath(normalizedPathname, navPath.toLowerCase()) &&
      (activePath === null || navPath.length > activePath.length)
    ) {
      activePath = navPath;
    }
  }

  return activePath;
}
