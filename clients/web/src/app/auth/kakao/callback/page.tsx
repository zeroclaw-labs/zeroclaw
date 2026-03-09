'use client';

import { Suspense, useEffect, useState } from 'react';
import { useRouter, useSearchParams } from 'next/navigation';
import { authKakaoCallback } from '@/lib/gateway-api';
import { useAuth } from '@/hooks/useAuth';

function KakaoCallbackInner() {
  const router = useRouter();
  const searchParams = useSearchParams();
  const { refreshAuth } = useAuth();
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const code = searchParams.get('code');
    if (!code) {
      setError('No authorization code received from Kakao.');
      return;
    }

    authKakaoCallback(code)
      .then(() => {
        refreshAuth();
        router.replace('/workspace/dashboard');
      })
      .catch((err) => {
        setError(err instanceof Error ? err.message : 'Kakao login failed');
      });
  }, [searchParams, router, refreshAuth]);

  if (error) {
    return (
      <div className="min-h-screen bg-gray-950 flex items-center justify-center p-4">
        <div className="bg-gray-900 rounded-2xl p-8 w-full max-w-md border border-gray-800 text-center">
          <div className="mb-4 p-3 bg-red-900/30 border border-red-700 rounded-lg text-red-300 text-sm">
            {error}
          </div>
          <button
            onClick={() => router.push('/auth')}
            className="text-blue-400 hover:text-blue-300 text-sm font-medium"
          >
            Back to Login
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="min-h-screen bg-gray-950 flex items-center justify-center">
      <div className="text-center">
        <div className="animate-spin rounded-full h-10 w-10 border-2 border-blue-500 border-t-transparent mx-auto mb-4" />
        <p className="text-gray-400">Processing Kakao login...</p>
      </div>
    </div>
  );
}

export default function KakaoCallbackPage() {
  return (
    <Suspense
      fallback={
        <div className="min-h-screen bg-gray-950 flex items-center justify-center">
          <div className="text-center">
            <div className="animate-spin rounded-full h-10 w-10 border-2 border-blue-500 border-t-transparent mx-auto mb-4" />
            <p className="text-gray-400">Loading...</p>
          </div>
        </div>
      }
    >
      <KakaoCallbackInner />
    </Suspense>
  );
}
