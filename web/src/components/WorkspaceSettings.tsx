import {
  createContext,
  lazy,
  Suspense,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import {
  createPath,
  parsePath,
  useNavigate,
  type Location,
  type NavigateFunction,
  type To,
  type NavigateOptions,
} from 'react-router-dom';
import { X } from 'lucide-react';
import { ConfigLocationProvider } from '@/lib/configLocation';
import { basePath } from '@/lib/basePath';
import { t } from '@/lib/i18n';

const Config = lazy(() => import('@/pages/Config'));
const SettingsContext = createContext<(path: string) => void>(() => {});
export const useWorkspaceSettings = () => useContext(SettingsContext);

export default function WorkspaceSettings({
  children,
}: {
  children: ReactNode;
}) {
  const [location, setLocation] = useState<Location | null>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  const navigate = useNavigate();
  const focused =
    location !== null && new URLSearchParams(location.search).has('field');
  const close = useCallback(() => setLocation(null), []);
  const open = useCallback((path: string) => {
    setLocation({
      pathname: '/config',
      search: '',
      hash: '',
      state: null,
      key: 'settings',
      ...parsePath(path),
    });
  }, []);
  const modalNavigate: NavigateFunction = useCallback(
    (to: To | number, options?: NavigateOptions) => {
      if (typeof to === 'number') {
        open('/config');
        return;
      }
      const path = typeof to === 'string' ? to : createPath(to);
      if (!path.startsWith('/config')) {
        close();
        void navigate(to);
        return;
      }
      setLocation({
        pathname: '/config',
        search: '',
        hash: '',
        state: options?.state ?? null,
        key: 'settings',
        ...parsePath(path),
      });
    },
    [open, close, navigate],
  );
  useEffect(() => {
    if (location && !dialog.current?.open) dialog.current?.showModal();
    if (!location) dialog.current?.close();
  }, [location]);

  return (
    <SettingsContext.Provider value={open}>
      {children}
      <dialog
        ref={dialog}
        onCancel={close}
        onClose={close}
        aria-label={t('workspace.settings')}
        className={`workspace-settings m-auto max-h-[calc(100dvh-2rem)] max-w-none overflow-hidden rounded-2xl border border-pc-border bg-pc-surface p-0 text-pc-text shadow-2xl backdrop:bg-black/50 backdrop:backdrop-blur-sm ${focused ? 'w-[min(40rem,calc(100%-2rem))]' : 'w-[min(62rem,calc(100%-2rem))] h-[min(48rem,calc(100dvh-2rem))]'}`}
        onClickCapture={(event) => {
          const anchor = (
            event.target as HTMLElement
          ).closest<HTMLAnchorElement>('a[href]');
          if (
            !anchor ||
            event.defaultPrevented ||
            event.button !== 0 ||
            event.metaKey ||
            event.ctrlKey ||
            event.shiftKey ||
            event.altKey
          )
            return;
          const url = new URL(anchor.href);
          if (url.origin !== window.location.origin) return;
          event.preventDefault();
          void modalNavigate(
            `${url.pathname.slice(basePath.length)}${url.search}`,
          );
        }}
      >
        {location && (
          <div
            className={`flex min-h-0 flex-col ${focused ? 'max-h-[calc(100dvh-2rem)]' : 'h-full'}`}
          >
            <div className="flex items-center justify-between border-b border-pc-border px-5 py-3">
              <h2 className="text-sm font-medium">{t('workspace.settings')}</h2>
              <button
                type="button"
                onClick={close}
                aria-label={t('common.close')}
                className="rounded-md p-2 hover:bg-pc-elevated"
              >
                <X className="h-4 w-4" />
              </button>
            </div>
            <div className="min-h-0 flex-1 overflow-y-auto">
              <ConfigLocationProvider
                location={location}
                navigate={modalNavigate}
              >
                <Suspense
                  fallback={
                    <p className="p-6 text-sm text-pc-text-muted">
                      {t('common.loading')}
                    </p>
                  }
                >
                  <Config compact />
                </Suspense>
              </ConfigLocationProvider>
            </div>
          </div>
        )}
      </dialog>
    </SettingsContext.Provider>
  );
}
