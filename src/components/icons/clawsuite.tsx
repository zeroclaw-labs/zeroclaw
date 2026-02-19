import { cn } from '@/lib/utils'

export type OpenClawStudioIconProps = {
  className?: string
  animateDots?: boolean
  dotClassName?: string
}

export function OpenClawStudioIcon({
  className,
  animateDots = false,
  dotClassName,
}: OpenClawStudioIconProps) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 100 95"
      fill="none"
      className={className}
    >
      <defs>
        <linearGradient id="orangeBgFinal" x1="0%" y1="0%" x2="100%" y2="100%">
          <stop offset="0%" style={{ stopColor: '#ea580c', stopOpacity: 1 }} />
          <stop offset="50%" style={{ stopColor: '#f97316', stopOpacity: 1 }} />
          <stop
            offset="100%"
            style={{ stopColor: '#fb923c', stopOpacity: 1 }}
          />
        </linearGradient>
      </defs>

      {/* Orange background */}
      <rect
        x="5"
        y="5"
        width="90"
        height="90"
        rx="16"
        fill="url(#orangeBgFinal)"
      />

      {/* Terminal window frame - dark outline, no fill */}
      <rect
        x="20"
        y="25"
        width="60"
        height="50"
        rx="4"
        stroke="#1e293b"
        strokeWidth="3"
        fill="none"
      />

      {/* Terminal header dots - dark fill */}
      <circle
        cx="28"
        cy="32"
        r="2.5"
        fill="#1e293b"
        className={cn(
          animateDots ? 'logo-loader-dot' : undefined,
          dotClassName,
        )}
        style={animateDots ? { animationDelay: '0s' } : undefined}
      />
      <circle
        cx="37"
        cy="32"
        r="2.5"
        fill="#1e293b"
        className={cn(
          animateDots ? 'logo-loader-dot' : undefined,
          dotClassName,
        )}
        style={animateDots ? { animationDelay: '0.2s' } : undefined}
      />
      <circle
        cx="46"
        cy="32"
        r="2.5"
        fill="#1e293b"
        className={cn(
          animateDots ? 'logo-loader-dot' : undefined,
          dotClassName,
        )}
        style={animateDots ? { animationDelay: '0.4s' } : undefined}
      />

      {/* Left claw bracket - dark */}
      <path
        d="M 38 45 L 32 50 L 38 55"
        stroke="#1e293b"
        strokeWidth="4"
        strokeLinecap="round"
        strokeLinejoin="round"
        fill="none"
      />

      {/* Right claw bracket - dark */}
      <path
        d="M 62 45 L 68 50 L 62 55"
        stroke="#1e293b"
        strokeWidth="4"
        strokeLinecap="round"
        strokeLinejoin="round"
        fill="none"
      />

      {/* Center cursor bar - dark fill */}
      <rect x="47" y="46" width="4" height="10" rx="2" fill="#1e293b">
        <animate
          attributeName="opacity"
          values="1;0.4;1"
          dur="1.5s"
          repeatCount="indefinite"
        />
      </rect>
    </svg>
  )
}
