'use client'

import { HugeiconsIcon } from '@hugeicons/react'
import { Cancel01Icon, File01Icon } from '@hugeicons/core-free-icons'

import type { AttachmentFile } from './attachment-button'
import { Button } from '@/components/ui/button'
import {
  PreviewCard,
  PreviewCardPopup,
  PreviewCardTrigger,
} from '@/components/ui/preview-card'
import { cn } from '@/lib/utils'

type AttachmentPreviewProps = {
  attachment: AttachmentFile
  onRemove: (id: string) => void
  className?: string
}

function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

function getFileExtension(filename: string): string {
  const parts = filename.split('.')
  return parts.length > 1 ? parts.pop()?.toUpperCase() || '' : ''
}

export function AttachmentPreview({
  attachment,
  onRemove,
  className,
}: AttachmentPreviewProps) {
  const hasError = Boolean(attachment.error)

  return (
    <PreviewCard>
      <PreviewCardTrigger
        render={
          <div
            className={cn(
              'relative flex items-center gap-1 rounded-md border p-1 max-w-[220px]',
              hasError
                ? 'border-red-300 bg-red-50'
                : 'border-primary-200 bg-primary-50',
              className,
            )}
          >
            <div className="relative shrink-0">
              {attachment.preview ? (
                <img
                  src={attachment.preview}
                  alt={attachment.file.name}
                  className="size-7 rounded object-cover"
                />
              ) : (
                <div className="flex size-7 items-center justify-center rounded bg-primary-100">
                  <HugeiconsIcon
                    icon={File01Icon}
                    size={14}
                    className="text-primary-500"
                  />
                </div>
              )}
            </div>

            <div className="min-w-0 flex-1">
              <p className="line-clamp-1 text-[10px] font-medium text-primary-900">
                {attachment.file.name}
              </p>
              {hasError ? (
                <p className="text-[9px] text-red-600">{attachment.error}</p>
              ) : (
                <p className="text-[9px] text-primary-500">
                  {getFileExtension(attachment.file.name)} •{' '}
                  {formatFileSize(attachment.file.size)}
                </p>
              )}
            </div>

            <Button
              variant="ghost"
              size="icon-sm"
              onClick={() => onRemove(attachment.id)}
              className="size-5 shrink-0 rounded-full hover:bg-primary-200"
              aria-label="Remove attachment"
              type="button"
            >
              <HugeiconsIcon icon={Cancel01Icon} size={12} />
            </Button>
          </div>
        }
        className="cursor-default"
      />
      <PreviewCardPopup align="end" sideOffset={8} className="w-52">
        <div className="space-y-2">
          {attachment.preview ? (
            <img
              src={attachment.preview}
              alt={attachment.file.name}
              className="max-h-36 w-full rounded-md object-cover"
            />
          ) : (
            <div className="flex items-center justify-center rounded-md bg-primary-100 p-4">
              <div className="flex items-center gap-2 text-xs text-primary-600">
                <HugeiconsIcon icon={File01Icon} size={20} />
                <span className="tabular-nums">
                  {getFileExtension(attachment.file.name) || 'FILE'}
                </span>
              </div>
            </div>
          )}
          <div className="space-y-0.5 text-xs text-primary-600">
            <div className="line-clamp-2 text-primary-900">
              {attachment.file.name}
            </div>
            <div className="tabular-nums">
              {formatFileSize(attachment.file.size)}
            </div>
          </div>
        </div>
      </PreviewCardPopup>
    </PreviewCard>
  )
}

type AttachmentPreviewListProps = {
  attachments: Array<AttachmentFile>
  onRemove: (id: string) => void
  className?: string
}

export function AttachmentPreviewList({
  attachments,
  onRemove,
  className,
}: AttachmentPreviewListProps) {
  if (attachments.length === 0) return null

  return (
    <div className={cn('flex flex-wrap items-start gap-1.5 px-4', className)}>
      {attachments.map((attachment) => (
        <AttachmentPreview
          key={attachment.id}
          attachment={attachment}
          onRemove={onRemove}
        />
      ))}
    </div>
  )
}
