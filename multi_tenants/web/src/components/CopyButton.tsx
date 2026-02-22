import { useState } from 'react'
import { Copy, Check } from 'lucide-react'

interface CopyButtonProps {
  text: string
}

export default function CopyButton({ text }: CopyButtonProps) {
  const [copied, setCopied] = useState(false)

  function handleClick() {
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 2000)
    })
  }

  return (
    <button
      onClick={handleClick}
      className="inline-flex items-center gap-1.5 text-xs px-2.5 py-1 border border-border-default text-text-secondary rounded-lg hover:bg-bg-card-hover transition-colors"
      type="button"
      aria-label={copied ? 'Copied' : 'Copy to clipboard'}
    >
      {copied ? <Check className="h-3 w-3 text-green-400" /> : <Copy className="h-3 w-3" />}
      {copied ? 'Copied!' : 'Copy'}
    </button>
  )
}
