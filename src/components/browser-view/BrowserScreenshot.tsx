import { HugeiconsIcon } from '@hugeicons/react'
import { Loading03Icon, ViewIcon } from '@hugeicons/core-free-icons'
import { AnimatePresence, motion } from 'motion/react'
import { useState } from 'react'

type BrowserScreenshotProps = {
  imageDataUrl: string
  loading: boolean
  capturedAt: string
}

function BrowserScreenshot({
  imageDataUrl,
  loading,
  capturedAt,
}: BrowserScreenshotProps) {
  const [failedImageUrl, setFailedImageUrl] = useState<string | null>(null)
  const imageReady = Boolean(imageDataUrl) && failedImageUrl !== imageDataUrl
  const capturedAtDate = capturedAt ? new Date(capturedAt) : null
  const capturedAtLabel =
    capturedAtDate && Number.isFinite(capturedAtDate.getTime())
      ? capturedAtDate.toLocaleTimeString(undefined, {
          hour: '2-digit',
          minute: '2-digit',
          second: '2-digit',
          hour12: false,
        })
      : '--:--:--'

  return (
    <section className="relative min-h-[320px] overflow-hidden rounded-2xl border border-primary-200 bg-primary-100/45 shadow-sm backdrop-blur-xl lg:min-h-[560px]">
      <div className="absolute top-3 right-3 z-10 inline-flex items-center gap-1.5 rounded-full border border-primary-200 bg-primary-50/80 px-2.5 py-1 text-xs text-primary-500 tabular-nums">
        <HugeiconsIcon icon={ViewIcon} size={20} strokeWidth={1.5} />
        <span>{capturedAtLabel}</span>
      </div>

      <AnimatePresence initial={false} mode="wait">
        {loading ? (
          <motion.div
            key="loading"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            className="flex h-full min-h-[320px] items-center justify-center bg-primary-100/35 text-primary-500"
          >
            <HugeiconsIcon
              icon={Loading03Icon}
              size={20}
              strokeWidth={1.5}
              className="animate-spin"
            />
          </motion.div>
        ) : !imageReady ? (
          <motion.div
            key="missing-image"
            initial={{ opacity: 0.25 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0.25 }}
            transition={{ duration: 0.2 }}
            className="flex h-full min-h-[320px] flex-col items-center justify-center gap-2 px-6 text-center"
          >
            <h3 className="text-base font-medium text-primary-900 text-balance">
              Screenshot failed to render
            </h3>
            <p className="max-w-sm text-sm text-primary-600 text-pretty">
              The gateway returned an image that could not be displayed.
            </p>
          </motion.div>
        ) : (
          <motion.div
            key={imageDataUrl}
            initial={{ opacity: 0.25 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0.25 }}
            transition={{ duration: 0.25 }}
            className="h-full min-h-[320px]"
          >
            <img
              src={imageDataUrl}
              alt="Live browser screenshot"
              className="h-full w-full object-contain"
              onError={function onImageError() {
                setFailedImageUrl(imageDataUrl)
              }}
            />
          </motion.div>
        )}
      </AnimatePresence>
    </section>
  )
}

export { BrowserScreenshot }
