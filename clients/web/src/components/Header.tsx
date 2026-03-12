"use client";

import { useState, useEffect } from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";

export default function Header() {
  const pathname = usePathname();
  const [isScrolled, setIsScrolled] = useState(false);
  const [isMobileMenuOpen, setIsMobileMenuOpen] = useState(false);
  const [lang, setLang] = useState<"ko" | "en">("ko");

  // Hide header on full-screen chat page
  const isFullScreenRoute = pathname === "/chat" || pathname.startsWith("/chat/");
  if (isFullScreenRoute) return null;

  useEffect(() => {
    const handleScroll = () => {
      setIsScrolled(window.scrollY > 20);
    };
    window.addEventListener("scroll", handleScroll);
    return () => window.removeEventListener("scroll", handleScroll);
  }, []);

  useEffect(() => {
    setIsMobileMenuOpen(false);
  }, [pathname]);

  const navItems = [
    { href: "/", label: lang === "ko" ? "\uD648" : "Home" },
    { href: "/workspace", label: lang === "ko" ? "\uC6CC\uD06C\uC2A4\uD398\uC774\uC2A4" : "Workspace" },
    { href: "/chat", label: lang === "ko" ? "\uCC44\uD305" : "Chat" },
    { href: "/download", label: lang === "ko" ? "\uB2E4\uC6B4\uB85C\uB4DC" : "Download" },
  ];

  const isActive = (href: string) => {
    if (href === "/") return pathname === "/";
    return pathname.startsWith(href);
  };

  return (
    <header
      className={`fixed top-0 left-0 right-0 z-50 transition-all duration-300 ${
        isScrolled
          ? "bg-dark-950/80 backdrop-blur-xl border-b border-dark-800/50"
          : "bg-transparent"
      }`}
    >
      <nav className="mx-auto flex max-w-7xl items-center justify-between px-4 py-4 sm:px-6 lg:px-8">
        {/* Logo */}
        <Link href="/" className="flex items-center gap-2 group">
          <div className="flex h-9 w-9 items-center justify-center rounded-lg bg-primary-500/10 border border-primary-500/20 transition-all group-hover:bg-primary-500/20 group-hover:border-primary-500/30">
            <span className="text-lg font-bold text-primary-400">Z</span>
          </div>
          <span className="text-lg font-bold text-dark-50 tracking-tight">
            Zero<span className="text-primary-400">Claw</span>
          </span>
        </Link>

        {/* Desktop Nav */}
        <div className="hidden items-center gap-1 md:flex">
          {navItems.map((item) => (
            <Link
              key={item.href}
              href={item.href}
              className={`rounded-lg px-4 py-2 text-sm font-medium transition-all duration-200 ${
                isActive(item.href)
                  ? "bg-primary-500/10 text-primary-400"
                  : "text-dark-300 hover:bg-dark-800/50 hover:text-dark-100"
              }`}
            >
              {item.label}
            </Link>
          ))}
        </div>

        {/* Right section */}
        <div className="hidden items-center gap-3 md:flex">
          {/* Language toggle */}
          <button
            onClick={() => setLang(lang === "ko" ? "en" : "ko")}
            className="rounded-lg px-3 py-1.5 text-xs font-medium text-dark-400 border border-dark-700 hover:border-dark-500 hover:text-dark-200 transition-all"
          >
            {lang === "ko" ? "EN" : "\uD55C"}
          </button>

          {/* GitHub */}
          <a
            href="https://github.com/AiFlowTools/MoA"
            target="_blank"
            rel="noopener noreferrer"
            className="flex h-9 w-9 items-center justify-center rounded-lg text-dark-400 hover:bg-dark-800 hover:text-dark-100 transition-all"
            aria-label="GitHub"
          >
            <svg
              className="h-5 w-5"
              fill="currentColor"
              viewBox="0 0 24 24"
              aria-hidden="true"
            >
              <path
                fillRule="evenodd"
                d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0022 12.017C22 6.484 17.522 2 12 2z"
                clipRule="evenodd"
              />
            </svg>
          </a>
        </div>

        {/* Mobile menu button */}
        <button
          onClick={() => setIsMobileMenuOpen(!isMobileMenuOpen)}
          className="flex h-9 w-9 items-center justify-center rounded-lg text-dark-400 hover:bg-dark-800 hover:text-dark-100 transition-all md:hidden"
          aria-label="Menu"
        >
          {isMobileMenuOpen ? (
            <svg
              className="h-5 w-5"
              fill="none"
              viewBox="0 0 24 24"
              strokeWidth={2}
              stroke="currentColor"
            >
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                d="M6 18L18 6M6 6l12 12"
              />
            </svg>
          ) : (
            <svg
              className="h-5 w-5"
              fill="none"
              viewBox="0 0 24 24"
              strokeWidth={2}
              stroke="currentColor"
            >
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                d="M3.75 6.75h16.5M3.75 12h16.5m-16.5 5.25h16.5"
              />
            </svg>
          )}
        </button>
      </nav>

      {/* Mobile menu */}
      {isMobileMenuOpen && (
        <div className="border-t border-dark-800/50 bg-dark-950/95 backdrop-blur-xl md:hidden animate-fade-in">
          <div className="space-y-1 px-4 py-3">
            {navItems.map((item) => (
              <Link
                key={item.href}
                href={item.href}
                className={`block rounded-lg px-4 py-2.5 text-sm font-medium transition-all ${
                  isActive(item.href)
                    ? "bg-primary-500/10 text-primary-400"
                    : "text-dark-300 hover:bg-dark-800/50 hover:text-dark-100"
                }`}
              >
                {item.label}
              </Link>
            ))}
            <div className="flex items-center gap-3 px-4 pt-2 pb-1">
              <button
                onClick={() => setLang(lang === "ko" ? "en" : "ko")}
                className="rounded-lg px-3 py-1.5 text-xs font-medium text-dark-400 border border-dark-700 hover:border-dark-500 hover:text-dark-200 transition-all"
              >
                {lang === "ko" ? "English" : "\uD55C\uAD6D\uC5B4"}
              </button>
              <a
                href="https://github.com/AiFlowTools/MoA"
                target="_blank"
                rel="noopener noreferrer"
                className="text-xs text-dark-400 hover:text-dark-200 transition-all"
              >
                GitHub
              </a>
            </div>
          </div>
        </div>
      )}
    </header>
  );
}
