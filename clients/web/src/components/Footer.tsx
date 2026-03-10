import Link from "next/link";

export default function Footer() {
  return (
    <footer className="border-t border-dark-800/50 bg-dark-950">
      <div className="mx-auto max-w-7xl px-4 py-12 sm:px-6 lg:px-8">
        <div className="grid grid-cols-1 gap-8 sm:grid-cols-2 lg:grid-cols-4">
          {/* Brand */}
          <div className="sm:col-span-2 lg:col-span-1">
            <Link href="/" className="flex items-center gap-2">
              <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-primary-500/10 border border-primary-500/20">
                <span className="text-base font-bold text-primary-400">Z</span>
              </div>
              <span className="text-base font-bold text-dark-50 tracking-tight">
                Zero<span className="text-primary-400">Claw</span>
              </span>
            </Link>
            <p className="mt-3 text-sm text-dark-400 leading-relaxed max-w-xs">
              ZeroClaw {"\uB7F0\uD0C0\uC784 \uAE30\uBC18\uC758 \uC790\uC728 AI \uC5D0\uC774\uC804\uD2B8"}
            </p>
          </div>

          {/* Product */}
          <div>
            <h3 className="text-sm font-semibold text-dark-100 mb-3">
              {"\uC81C\uD488"} <span className="text-dark-500 font-normal">Product</span>
            </h3>
            <ul className="space-y-2">
              <li>
                <Link
                  href="/chat"
                  className="text-sm text-dark-400 hover:text-dark-200 transition-colors"
                >
                  {"\uC6F9 \uCC44\uD305"} Web Chat
                </Link>
              </li>
              <li>
                <Link
                  href="/download"
                  className="text-sm text-dark-400 hover:text-dark-200 transition-colors"
                >
                  {"\uB2E4\uC6B4\uB85C\uB4DC"} Download
                </Link>
              </li>
              <li>
                <a
                  href="https://docs.moa-agent.com"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-sm text-dark-400 hover:text-dark-200 transition-colors"
                >
                  {"\uBB38\uC11C"} Documentation
                </a>
              </li>
            </ul>
          </div>

          {/* Resources */}
          <div>
            <h3 className="text-sm font-semibold text-dark-100 mb-3">
              {"\uB9AC\uC18C\uC2A4"} <span className="text-dark-500 font-normal">Resources</span>
            </h3>
            <ul className="space-y-2">
              <li>
                <a
                  href="https://github.com/AiFlowTools/MoA"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-sm text-dark-400 hover:text-dark-200 transition-colors"
                >
                  GitHub
                </a>
              </li>
              <li>
                <a
                  href="https://github.com/AiFlowTools/MoA/issues"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-sm text-dark-400 hover:text-dark-200 transition-colors"
                >
                  {"\uBC84\uADF8 \uBCF4\uACE0"} Bug Reports
                </a>
              </li>
              <li>
                <a
                  href="https://github.com/AiFlowTools/MoA/blob/main/CONTRIBUTING.md"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-sm text-dark-400 hover:text-dark-200 transition-colors"
                >
                  {"\uAE30\uC5EC"} Contributing
                </a>
              </li>
            </ul>
          </div>

          {/* Contact */}
          <div>
            <h3 className="text-sm font-semibold text-dark-100 mb-3">
              {"\uC5F0\uB77D"} <span className="text-dark-500 font-normal">Contact</span>
            </h3>
            <ul className="space-y-2">
              <li>
                <a
                  href="mailto:contact@moa-agent.com"
                  className="text-sm text-dark-400 hover:text-dark-200 transition-colors"
                >
                  contact@moa-agent.com
                </a>
              </li>
              <li>
                <a
                  href="https://github.com/AiFlowTools/MoA/discussions"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-sm text-dark-400 hover:text-dark-200 transition-colors"
                >
                  {"\uCEE4\uBBA4\uB2C8\uD2F0"} Community
                </a>
              </li>
            </ul>
          </div>
        </div>

        {/* Bottom bar */}
        <div className="mt-10 flex flex-col items-center justify-between gap-4 border-t border-dark-800/50 pt-8 sm:flex-row">
          <p className="text-xs text-dark-500">
            &copy; {new Date().getFullYear()} ZeroClaw. All rights reserved.
          </p>
          <p className="text-xs text-dark-600">
            Built with{" "}
            <a
              href="https://github.com/AiFlowTools/MoA"
              target="_blank"
              rel="noopener noreferrer"
              className="text-primary-500/60 hover:text-primary-400 transition-colors"
            >
              ZeroClaw
            </a>
          </p>
        </div>
      </div>
    </footer>
  );
}
