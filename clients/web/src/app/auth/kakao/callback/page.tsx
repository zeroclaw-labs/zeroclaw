'use client';

import { Suspense, useEffect, useState } from 'react';
import { useRouter, useSearchParams } from 'next/navigation';
import { authKakaoCallback, type UserDevice } from '@/lib/gateway-api';
import { setToken } from '@/lib/auth';

function KakaoCallbackInner() {
  const router = useRouter();
  const searchParams = useSearchParams();
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const code = searchParams.get('code');
    if (!code) {
      setError('No authorization code received from Kakao.');
      return;
    }

    authKakaoCallback(code)
      .then((result) => {
        const devices: UserDevice[] = result.devices || [];
        if (devices.length === 0) {
          // No devices: set token and go to chat
          setToken(result.token);
          router.replace('/chat');
        } else {
          // Has devices: go to auth flow for device selection
          // Store token temporarily and redirect to auth with device-select step
          setToken(result.token);
          router.replace('/chat');
        }
      })
      .catch((err) => {
        setError(err instanceof Error ? err.message : 'Kakao login failed');
      });
  }, [searchParams, router]);

  if (error) {
    return (
      <div className="min-h-screen bg-dark-950 flex items-center justify-center p-4">
        <div className="bg-dark-900 rounded-2xl p-8 w-full max-w-md border border-dark-800 text-center">
          <div className="mb-4 p-3 bg-red-900/30 border border-red-700/50 rounded-lg text-red-300 text-sm">
            {error}
          </div>
          <button
            onClick={() => router.push('/auth')}
            className="text-primary-400 hover:text-primary-300 text-sm font-medium"
          >
            Back to Login
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="min-h-screen bg-dark-950 flex items-center justify-center">
      <div className="text-center">
        <div className="animate-spin rounded-full h-10 w-10 border-2 border-primary-500 border-t-transparent mx-auto mb-4" />
        <p className="text-dark-400">Processing Kakao login...</p>
      </div>
    </div>
  );
}

export default function KakaoCallbackPage() {
  return (
    <Suspense
      fallback={
        <div className="min-h-screen bg-dark-950 flex items-center justify-center">
          <div className="text-center">
            <div className="animate-spin rounded-full h-10 w-10 border-2 border-primary-500 border-t-transparent mx-auto mb-4" />
            <p className="text-dark-400">Loading...</p>
          </div>
        </div>
      }
    >
      <KakaoCallbackInner />
    </Suspense>
  );
}
