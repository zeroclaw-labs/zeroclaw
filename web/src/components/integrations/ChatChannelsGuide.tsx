import { useState } from 'react';
import { ChevronDown, MessageCircleMore, Sparkles } from 'lucide-react';
import {
  CHAT_CHANNEL_NOTES,
  CHAT_CHANNEL_SUPPORT,
  type ChatChannelSupportLevel,
} from '@/lib/chatChannels';

const SUPPORT_LEVEL_CLASSES: Record<ChatChannelSupportLevel, string> = {
  'Built-in': 'border-[#2f63c8] bg-[#0a265f]/70 text-[#acd0ff]',
  Plugin: 'border-[#2f5ea0] bg-[#071a41]/80 text-[#8eb8f4]',
  Legacy: 'border-[#5f6080] bg-[#141731]/80 text-[#c2c5e8]',
};

export default function ChatChannelsGuide() {
  const [isOpen, setIsOpen] = useState(true);

  return (
    <section className="electric-card motion-rise">
      <button
        type="button"
        onClick={() => setIsOpen((prev) => !prev)}
        aria-expanded={isOpen}
        className="group flex w-full items-center justify-between gap-4 rounded-xl px-4 py-4 text-left md:px-5"
      >
        <div className="flex items-center gap-3">
          <div className="electric-icon h-10 w-10 rounded-xl">
            <MessageCircleMore className="h-5 w-5" />
          </div>
          <div>
            <h2 className="text-base font-semibold text-white">
              Supported Chat Channels
            </h2>
            <p className="text-xs uppercase tracking-[0.13em] text-[#7ea5eb]">
              {CHAT_CHANNEL_SUPPORT.length} channels listed
            </p>
          </div>
        </div>
        <ChevronDown
          className={[
            'h-5 w-5 text-[#7ea5eb] transition-transform duration-300',
            isOpen ? 'rotate-180' : 'rotate-0',
          ].join(' ')}
        />
      </button>

      {isOpen && (
        <div className="border-t border-[#18356f] px-4 pb-5 pt-4 md:px-5">
          <div className="rounded-xl border border-[#1e3a78] bg-[#07142f]/85 p-3 md:p-4">
            <p className="text-sm leading-relaxed text-[#c8dcff]">
              ZeroClaw can talk to you on the chat apps you already use through
              Gateway. Text is supported across all channels; media and reactions
              vary by channel.
            </p>
          </div>

          <div className="mt-4 grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3">
            {CHAT_CHANNEL_SUPPORT.map((channel) => (
              <article
                key={channel.id}
                className="rounded-xl border border-[#1f3d76] bg-[#060f25]/85 p-3 shadow-[0_0_22px_-15px_rgba(80,176,255,0.9)]"
              >
                <div className="flex items-start justify-between gap-2">
                  <h3 className="text-sm font-semibold text-white">{channel.name}</h3>
                  <span
                    className={[
                      'inline-flex items-center rounded-full border px-2 py-0.5 text-[11px] font-medium',
                      SUPPORT_LEVEL_CLASSES[channel.supportLevel],
                    ].join(' ')}
                  >
                    {channel.supportLevel}
                  </span>
                </div>
                <p className="mt-2 text-xs leading-relaxed text-[#97baee]">
                  {channel.summary}
                </p>
                {channel.details && (
                  <p className="mt-2 text-[11px] leading-relaxed text-[#7ca6de]">
                    {channel.details}
                  </p>
                )}
                {channel.recommended && (
                  <p className="mt-2 inline-flex items-center gap-1 text-[11px] text-[#cfe3ff]">
                    <Sparkles className="h-3 w-3" />
                    Recommended
                  </p>
                )}
              </article>
            ))}
          </div>

          <div className="mt-4 rounded-xl border border-[#1b3770] bg-[#061129]/85 p-3 md:p-4">
            <h3 className="text-sm font-semibold text-white">Channel Notes</h3>
            <ul className="mt-2 space-y-1.5 text-xs leading-relaxed text-[#9bbce8]">
              {CHAT_CHANNEL_NOTES.map((note) => (
                <li key={note}>• {note}</li>
              ))}
            </ul>
          </div>
        </div>
      )}
    </section>
  );
}
