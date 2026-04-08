'use client';

import { Suspense, useState, useEffect, useCallback, useRef } from 'react';
import { useRouter, useSearchParams } from 'next/navigation';
import Link from 'next/link';
import { isAuthenticated, setToken } from '@/lib/auth';
import {
  authLogin,
  authRegister,
  remoteLogin,
  verifyRemoteEmail,
  getRemoteDevices,
  type UserDevice,
  type AuthLoginResponse,
} from '@/lib/gateway-api';

type AuthStep =
  | 'login'
  | 'signup'
  | 'device-select'
  | 'pairing-code'
  | 'email-verify';

const KAKAO_REST_KEY = process.env.NEXT_PUBLIC_KAKAO_REST_API_KEY || '';

function getKakaoRedirectUri(): string {
  return `${window.location.origin}/api/auth/kakao/redirect`;
}

export default function AuthPage() {
  return (
    <Suspense fallback={
      <div className="min-h-screen bg-dark-950 flex items-center justify-center">
        <div className="h-8 w-8 border-2 border-primary-500 border-t-transparent rounded-full animate-spin" />
      </div>
    }>
      <AuthPageInner />
    </Suspense>
  );
}

function AuthPageInner() {
  const router = useRouter();
  const searchParams = useSearchParams();
  const redirectTo = searchParams.get('redirect') || '/chat';

  const [step, setStep] = useState<AuthStep>('login');

  // Login/Signup fields
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [signupEmail, setSignupEmail] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  const [successMessage, setSuccessMessage] = useState('');
  const [showAppGuideModal, setShowAppGuideModal] = useState(false);

  // Post-login state
  const [loginToken, setLoginToken] = useState('');
  const [userId, setUserId] = useState('');
  const [userDevices, setUserDevices] = useState<UserDevice[]>([]);
  const [selectedDeviceId, setSelectedDeviceId] = useState('');
  const [selectedDeviceName, setSelectedDeviceName] = useState('');
  const [emailHint, setEmailHint] = useState('');

  // Pairing code
  const [pairingCode, setPairingCode] = useState('');

  // Email verification
  const [verificationCode, setVerificationCode] = useState('');
  const [verificationExpiresIn, setVerificationExpiresIn] = useState(0);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Redirect if already authenticated
  useEffect(() => {
    if (isAuthenticated()) {
      router.replace(redirectTo);
    }
  }, [router, redirectTo]);

  // Countdown timer for email verification
  useEffect(() => {
    if (verificationExpiresIn > 0) {
      timerRef.current = setInterval(() => {
        setVerificationExpiresIn((prev) => {
          if (prev <= 1) {
            if (timerRef.current) clearInterval(timerRef.current);
            return 0;
          }
          return prev - 1;
        });
      }, 1000);
      return () => {
        if (timerRef.current) clearInterval(timerRef.current);
      };
    }
  }, [verificationExpiresIn]);

  const handleLogin = useCallback(async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError('');
    try {
      const result: AuthLoginResponse = await authLogin(username, password);
      setLoginToken(result.token);
      setUserId(result.user_id);

      // Fetch device list from server
      let devices: UserDevice[] = result.devices || [];
      if (devices.length === 0 && result.token) {
        try {
          devices = await getRemoteDevices(result.token);
        } catch {
          // Device fetch failed - proceed without device selection
        }
      }
      setUserDevices(devices);

      if (devices.length === 0) {
        // No devices registered - set token and go to chat directly
        setToken(result.token);
        router.push(redirectTo);
      } else if (devices.length === 1) {
        // Single device - try connecting without pairing code first
        setSelectedDeviceId(devices[0].device_id);
        setSelectedDeviceName(devices[0].device_name);
        await tryRemoteLoginWithoutPairingCode(devices[0].device_id);
      } else {
        // Multiple devices - show device selection
        setStep('device-select');
      }
    } catch (err: unknown) {
      const errMsg = err instanceof Error ? err.message : 'Login failed';
      const lower = errMsg.toLowerCase();
      if (lower.includes('not found') || lower.includes('no user')) {
        setError('등록되지 않은 아이디입니다. 아래에서 회원가입해 주세요.');
      } else if (lower.includes('invalid') || lower.includes('password') || lower.includes('unauthorized')) {
        setError('비밀번호가 올바르지 않습니다. 다시 확인해 주세요.');
      } else {
        setError(errMsg);
      }
    } finally {
      setLoading(false);
    }
  }, [username, password, router, redirectTo]);

  const handleSignup = useCallback(async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError('');
    setSuccessMessage('');

    if (password !== confirmPassword) {
      setError('비밀번호가 일치하지 않습니다.');
      setLoading(false);
      return;
    }
    if (password.length < 4) {
      setError('비밀번호는 4자 이상이어야 합니다.');
      setLoading(false);
      return;
    }

    try {
      await authRegister(username, password, signupEmail || undefined);
      setSuccessMessage('회원가입이 완료되었습니다! 로그인해 주세요.');
      setStep('login');
      setConfirmPassword('');
      setSignupEmail('');
    } catch (err: unknown) {
      const errMsg = err instanceof Error ? err.message : 'Registration failed';
      if (errMsg.toLowerCase().includes('already') || errMsg.toLowerCase().includes('taken')) {
        setError('이미 사용 중인 아이디입니다. 다른 아이디를 입력해 주세요.');
      } else {
        setError(errMsg);
      }
    } finally {
      setLoading(false);
    }
  }, [username, password, confirmPassword, signupEmail]);

  // Try remote login without pairing code — if server says code is required, show the step
  const tryRemoteLoginWithoutPairingCode = useCallback(async (deviceId: string) => {
    setLoading(true);
    setError('');
    try {
      const result = await remoteLogin(username, password, deviceId, '');
      if (result.requires_email_verification) {
        setEmailHint(result.email_hint || '');
        setVerificationExpiresIn(300);
        setStep('email-verify');
      } else if (result.session_token) {
        setToken(result.session_token);
        router.push(redirectTo);
      }
    } catch (err: unknown) {
      const errMsg = err instanceof Error ? err.message : '';
      // Server requires pairing code for this device → show pairing code step
      if (errMsg.includes('pairing') || errMsg.includes('페어링')) {
        setStep('pairing-code');
      } else {
        setError(errMsg || '연결에 실패했습니다.');
      }
    } finally {
      setLoading(false);
    }
  }, [username, password, router, redirectTo]);

  const handleDeviceSelect = useCallback(async () => {
    if (!selectedDeviceId) {
      setError('디바이스를 선택해 주세요');
      return;
    }
    setError('');
    const device = userDevices.find((d) => d.device_id === selectedDeviceId);
    setSelectedDeviceName(device?.device_name || '');
    // Try without pairing code first
    await tryRemoteLoginWithoutPairingCode(selectedDeviceId);
  }, [selectedDeviceId, userDevices, tryRemoteLoginWithoutPairingCode]);

  const handlePairingCode = useCallback(async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError('');

    try {
      // Use remote login endpoint: validates credentials + device + pairing code
      // and triggers email verification if configured
      const result = await remoteLogin(
        username,
        password,
        selectedDeviceId,
        pairingCode.trim(),
      );

      setUserId(result.user_id);

      if (result.requires_email_verification) {
        // Email verification required - show code input
        setEmailHint(result.email_hint || '');
        setVerificationExpiresIn(300); // 5 minutes
        setStep('email-verify');
      } else if (result.token) {
        // No email verification needed - token returned directly
        setToken(result.token);
        router.push(redirectTo);
      }
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Pairing failed');
    } finally {
      setLoading(false);
    }
  }, [pairingCode, username, password, selectedDeviceId, router, redirectTo]);

  const handleEmailVerify = useCallback(async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError('');

    if (!verificationCode.trim()) {
      setError('Please enter the verification code');
      setLoading(false);
      return;
    }

    try {
      await verifyRemoteEmail(userId, verificationCode.trim());
      // Token is set by verifyRemoteEmail on success
      router.push(redirectTo);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Verification failed');
    } finally {
      setLoading(false);
    }
  }, [verificationCode, userId, router, redirectTo]);

  const handleResendCode = useCallback(async () => {
    setError('');
    try {
      // Re-trigger remote login to resend the email verification code
      await remoteLogin(username, password, selectedDeviceId, pairingCode.trim());
      setVerificationExpiresIn(300);
      setVerificationCode('');
      setSuccessMessage('Verification code resent to your email.');
      setTimeout(() => setSuccessMessage(''), 3000);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to resend code');
    }
  }, [username, password, selectedDeviceId, pairingCode]);

  const handleKakaoLogin = () => {
    if (!KAKAO_REST_KEY) {
      setError('Kakao login is not configured.');
      return;
    }
    const redirectUri = getKakaoRedirectUri();
    const kakaoAuthUrl = `https://kauth.kakao.com/oauth/authorize?client_id=${KAKAO_REST_KEY}&redirect_uri=${encodeURIComponent(redirectUri)}&response_type=code`;
    window.location.href = kakaoAuthUrl;
  };

  const handleBack = () => {
    setError('');
    if (step === 'email-verify') {
      setStep('pairing-code');
      setVerificationCode('');
    } else if (step === 'pairing-code') {
      if (userDevices.length > 1) {
        setStep('device-select');
      } else {
        setStep('login');
      }
      setPairingCode('');
    } else if (step === 'device-select') {
      setStep('login');
    }
  };

  const formatTime = (seconds: number) => {
    const m = Math.floor(seconds / 60);
    const s = seconds % 60;
    return `${m}:${s.toString().padStart(2, '0')}`;
  };

  return (
    <div className="min-h-screen bg-dark-950 flex items-center justify-center p-4">
      <div className="bg-dark-900 rounded-2xl p-8 w-full max-w-md border border-dark-800 shadow-2xl">
        {/* Header */}
        <div className="text-center mb-8">
          <Link href="/" className="inline-flex items-center gap-2 group mb-4">
            <div className="flex h-12 w-12 items-center justify-center rounded-xl bg-primary-500/10 border border-primary-500/20 transition-all group-hover:bg-primary-500/20">
              <span className="text-2xl font-bold text-primary-400">M</span>
            </div>
          </Link>
          <h1 className="text-2xl font-bold text-dark-50">MoA</h1>
          <p className="text-dark-400 text-sm mt-1">
            {step === 'login' && 'MoA 로그인'}
            {step === 'signup' && 'MoA 회원가입'}
            {step === 'device-select' && 'Select your device'}
            {step === 'pairing-code' && `Enter pairing code for ${selectedDeviceName || 'device'}`}
            {step === 'email-verify' && 'Enter email verification code'}
          </p>
        </div>

        {/* Messages */}
        {successMessage && (
          <div className="mb-4 p-3 bg-green-900/30 border border-green-700/50 rounded-lg text-green-300 text-sm text-center">
            {successMessage}
          </div>
        )}
        {error && (
          <div className="mb-4 p-3 bg-red-900/30 border border-red-700/50 rounded-lg text-red-300 text-sm text-center">
            {error}
          </div>
        )}

        {/* Back button for multi-step */}
        {step !== 'login' && step !== 'signup' && (
          <button
            type="button"
            onClick={handleBack}
            className="mb-4 flex items-center gap-1 text-sm text-dark-400 hover:text-dark-200 transition-colors"
          >
            <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor">
              <path strokeLinecap="round" strokeLinejoin="round" d="M15.75 19.5L8.25 12l7.5-7.5" />
            </svg>
            Back
          </button>
        )}

        {/* Step: Login */}
        {step === 'login' && (
          <form onSubmit={handleLogin} className="space-y-4">
            <div>
              <label htmlFor="login-username" className="block text-sm font-medium text-dark-300 mb-1">Username</label>
              <input
                id="login-username"
                type="text"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                className="w-full px-4 py-3 bg-dark-800 border border-dark-700 rounded-lg text-dark-100 focus:outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500/30 transition-all"
                placeholder="Enter username"
                autoFocus
                required
              />
            </div>
            <div>
              <label htmlFor="login-password" className="block text-sm font-medium text-dark-300 mb-1">Password</label>
              <input
                id="login-password"
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                className="w-full px-4 py-3 bg-dark-800 border border-dark-700 rounded-lg text-dark-100 focus:outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500/30 transition-all"
                placeholder="Enter password"
                required
              />
            </div>
            <button
              type="submit"
              disabled={loading}
              className="w-full py-3 bg-primary-500 hover:bg-primary-600 disabled:bg-dark-700 disabled:text-dark-500 text-white rounded-lg font-medium transition-colors"
            >
              {loading ? '로그인 중...' : '로그인'}
            </button>

            <p className="text-center text-sm text-dark-400 mt-4">
              계정이 없으신가요?{' '}
              <button
                type="button"
                onClick={() => { setStep('signup'); setError(''); setSuccessMessage(''); }}
                className="text-primary-400 hover:text-primary-300 font-medium"
              >
                회원가입
              </button>
            </p>

            {KAKAO_REST_KEY && (
              <>
                <div className="relative my-4">
                  <div className="absolute inset-0 flex items-center"><div className="w-full border-t border-dark-700" /></div>
                  <div className="relative flex justify-center text-sm"><span className="px-3 bg-dark-900 text-dark-500">or</span></div>
                </div>
                <button
                  type="button"
                  onClick={handleKakaoLogin}
                  className="w-full py-3 bg-[#FEE500] hover:bg-[#F5DC00] text-[#000000D9] rounded-lg font-medium transition-colors flex items-center justify-center gap-2"
                >
                  <svg width="18" height="18" viewBox="0 0 18 18" fill="none">
                    <path d="M9 0.5C4.03 0.5 0 3.72 0 7.71C0 10.25 1.56 12.5 3.93 13.82L2.93 17.18C2.87 17.4 2.95 17.48 3.14 17.36L7.07 14.83C7.69 14.92 8.33 14.97 9 14.97C13.97 14.97 18 11.7 18 7.71C18 3.72 13.97 0.5 9 0.5Z" fill="#000000D9"/>
                  </svg>
                  Kakao Login
                </button>
              </>
            )}
          </form>
        )}

        {/* Step: Signup */}
        {step === 'signup' && (
          <form onSubmit={handleSignup} className="space-y-4">
            <div>
              <label htmlFor="signup-username" className="block text-sm font-medium text-dark-300 mb-1">아이디</label>
              <input
                id="signup-username"
                type="text"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                className="w-full px-4 py-3 bg-dark-800 border border-dark-700 rounded-lg text-dark-100 focus:outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500/30 transition-all"
                placeholder="사용할 아이디를 입력하세요"
                autoFocus
                required
              />
            </div>
            <div>
              <label htmlFor="signup-email" className="block text-sm font-medium text-dark-300 mb-1">이메일 (선택)</label>
              <input
                id="signup-email"
                type="email"
                value={signupEmail}
                onChange={(e) => setSignupEmail(e.target.value)}
                className="w-full px-4 py-3 bg-dark-800 border border-dark-700 rounded-lg text-dark-100 focus:outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500/30 transition-all"
                placeholder="example@email.com"
              />
            </div>
            <div>
              <label htmlFor="signup-password" className="block text-sm font-medium text-dark-300 mb-1">비밀번호</label>
              <input
                id="signup-password"
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                className="w-full px-4 py-3 bg-dark-800 border border-dark-700 rounded-lg text-dark-100 focus:outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500/30 transition-all"
                placeholder="4자 이상"
                required
              />
            </div>
            <div>
              <label htmlFor="signup-confirm" className="block text-sm font-medium text-dark-300 mb-1">비밀번호 확인</label>
              <input
                id="signup-confirm"
                type="password"
                value={confirmPassword}
                onChange={(e) => setConfirmPassword(e.target.value)}
                className="w-full px-4 py-3 bg-dark-800 border border-dark-700 rounded-lg text-dark-100 focus:outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500/30 transition-all"
                placeholder="비밀번호를 다시 입력하세요"
                required
              />
            </div>
            <button
              type="submit"
              disabled={loading}
              className="w-full py-3 bg-primary-500 hover:bg-primary-600 disabled:bg-dark-700 disabled:text-dark-500 text-white rounded-lg font-medium transition-colors"
            >
              {loading ? '가입 중...' : '회원가입'}
            </button>
            <p className="text-center text-sm text-dark-400 mt-4">
              이미 계정이 있으신가요?{' '}
              <button
                type="button"
                onClick={() => { setStep('login'); setError(''); setSuccessMessage(''); }}
                className="text-primary-400 hover:text-primary-300 font-medium"
              >
                로그인
              </button>
            </p>
          </form>
        )}

        {/* Step: Device Selection */}
        {step === 'device-select' && (
          <div className="space-y-4">
            <p className="text-sm text-dark-400 text-center mb-2">
              Select a device to connect to via web chat
            </p>
            <div className="space-y-2 max-h-64 overflow-y-auto custom-scrollbar">
              {userDevices.map((device) => (
                <label
                  key={device.device_id}
                  className={`flex items-center gap-3 p-3 rounded-lg border cursor-pointer transition-all ${
                    selectedDeviceId === device.device_id
                      ? 'border-primary-500 bg-primary-500/10'
                      : 'border-dark-700 bg-dark-800/50 hover:border-dark-600'
                  }`}
                >
                  <input
                    type="radio"
                    name="device"
                    value={device.device_id}
                    checked={selectedDeviceId === device.device_id}
                    onChange={() => setSelectedDeviceId(device.device_id)}
                    className="sr-only"
                  />
                  <div className={`flex h-5 w-5 items-center justify-center rounded-full border-2 flex-shrink-0 ${
                    selectedDeviceId === device.device_id
                      ? 'border-primary-500 bg-primary-500'
                      : 'border-dark-600'
                  }`}>
                    {selectedDeviceId === device.device_id && (
                      <div className="h-2 w-2 rounded-full bg-white" />
                    )}
                  </div>
                  <div className="flex-1 min-w-0">
                    <p className="text-sm font-medium text-dark-100 truncate">
                      {device.device_name}
                    </p>
                    <div className="flex items-center gap-2 mt-0.5">
                      <span className="text-xs text-dark-500">{device.platform}</span>
                      <div className={`h-1.5 w-1.5 rounded-full ${device.is_online ? 'bg-green-400' : 'bg-dark-600'}`} />
                      <span className="text-xs text-dark-500">
                        {device.is_online ? 'Online' : 'Offline'}
                      </span>
                    </div>
                  </div>
                </label>
              ))}
            </div>
            <button
              onClick={handleDeviceSelect}
              disabled={!selectedDeviceId}
              className="w-full py-3 bg-primary-500 hover:bg-primary-600 disabled:bg-dark-700 disabled:text-dark-500 text-white rounded-lg font-medium transition-colors"
            >
              Continue
            </button>
          </div>
        )}

        {/* Step: Pairing Code */}
        {step === 'pairing-code' && (
          <form onSubmit={handlePairingCode} className="space-y-4">
            <div className="text-center mb-2">
              <div className="inline-flex items-center gap-2 rounded-full bg-dark-800 px-3 py-1 mb-3">
                <div className="h-2 w-2 rounded-full bg-primary-400" />
                <span className="text-xs text-dark-300">{selectedDeviceName}</span>
              </div>
              <p className="text-sm text-dark-400">
                Enter the pairing code set on your MoA app device
              </p>
            </div>
            <div>
              <label htmlFor="pairing-code" className="block text-sm font-medium text-dark-300 mb-1">
                Pairing Code
              </label>
              <input
                id="pairing-code"
                type="text"
                value={pairingCode}
                onChange={(e) => setPairingCode(e.target.value)}
                placeholder="Enter device pairing code"
                className="w-full px-4 py-3 bg-dark-800 border border-dark-700 rounded-lg text-dark-100 text-center text-lg tracking-widest focus:outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500/30 transition-all"
                autoFocus
                required
              />
            </div>
            <button
              type="submit"
              disabled={loading || !pairingCode.trim()}
              className="w-full py-3 bg-primary-500 hover:bg-primary-600 disabled:bg-dark-700 disabled:text-dark-500 text-white rounded-lg font-medium transition-colors"
            >
              {loading ? 'Verifying...' : 'Verify & Send Email Code'}
            </button>
          </form>
        )}

        {/* Step: Email Verification */}
        {step === 'email-verify' && (
          <form onSubmit={handleEmailVerify} className="space-y-4">
            <div className="text-center mb-2">
              <p className="text-sm text-dark-400">
                A verification code has been sent to{' '}
                {emailHint ? (
                  <span className="font-medium text-dark-200">{emailHint}</span>
                ) : (
                  'your registered email'
                )}
                .
              </p>
              <p className="text-sm text-dark-400 mt-1">
                Please enter it within{' '}
                <span className={`font-mono font-semibold ${verificationExpiresIn <= 60 ? 'text-red-400' : 'text-primary-400'}`}>
                  {formatTime(verificationExpiresIn)}
                </span>
              </p>
            </div>
            <div>
              <label htmlFor="verify-code" className="block text-sm font-medium text-dark-300 mb-1">
                Verification Code
              </label>
              <input
                id="verify-code"
                type="text"
                value={verificationCode}
                onChange={(e) => setVerificationCode(e.target.value)}
                placeholder="Enter 6-digit code"
                maxLength={6}
                className="w-full px-4 py-3 bg-dark-800 border border-dark-700 rounded-lg text-dark-100 text-center text-2xl tracking-[0.5em] focus:outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500/30 transition-all"
                autoFocus
                required
              />
            </div>
            <button
              type="submit"
              disabled={loading || verificationCode.length < 6 || verificationExpiresIn === 0}
              className="w-full py-3 bg-primary-500 hover:bg-primary-600 disabled:bg-dark-700 disabled:text-dark-500 text-white rounded-lg font-medium transition-colors"
            >
              {loading ? 'Verifying...' : 'Complete Verification'}
            </button>

            {verificationExpiresIn === 0 ? (
              <button
                type="button"
                onClick={handleResendCode}
                className="w-full py-2 text-sm text-primary-400 hover:text-primary-300 transition-colors"
              >
                Code expired. Resend verification code
              </button>
            ) : (
              <button
                type="button"
                onClick={handleResendCode}
                className="w-full py-2 text-sm text-dark-500 hover:text-dark-400 transition-colors"
              >
                Resend code
              </button>
            )}
          </form>
        )}

        {/* Footer link */}
        <div className="mt-6 text-center">
          <Link href="/" className="text-xs text-dark-500 hover:text-dark-400 transition-colors">
            Back to home
          </Link>
        </div>
      </div>

      {/* App Guide Modal */}
      {showAppGuideModal && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-dark-950/80 backdrop-blur-sm animate-fade-in">
          <div className="w-full max-w-md mx-4 rounded-2xl border border-dark-700 bg-dark-900 p-6 shadow-2xl">
            <div className="text-center mb-6">
              <div className="flex h-14 w-14 mx-auto items-center justify-center rounded-2xl bg-yellow-500/10 border border-yellow-500/20 mb-4">
                <svg className="h-7 w-7 text-yellow-400" fill="none" viewBox="0 0 24 24" strokeWidth={1.5} stroke="currentColor">
                  <path strokeLinecap="round" strokeLinejoin="round" d="M12 9v3.75m-9.303 3.376c-.866 1.5.217 3.374 1.948 3.374h14.71c1.73 0 2.813-1.874 1.948-3.374L13.949 3.378c-.866-1.5-3.032-1.5-3.898 0L2.697 16.126zM12 15.75h.007v.008H12v-.008z" />
                </svg>
              </div>
              <h3 className="text-lg font-semibold text-dark-50 mb-2">
                {"회원정보를 찾을 수 없습니다"}
              </h3>
              <p className="text-sm text-dark-400 leading-relaxed">
                {"먼저 MoA 앱을 다운로드 받아 설치한 후 회원가입을 해주신 후에 로그인이 가능합니다."}
              </p>
            </div>

            <div className="flex flex-col gap-3">
              <Link
                href="/download"
                className="w-full py-3 bg-primary-500 hover:bg-primary-600 text-white rounded-lg font-medium transition-colors text-center text-sm flex items-center justify-center gap-2"
              >
                <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor">
                  <path strokeLinecap="round" strokeLinejoin="round" d="M3 16.5v2.25A2.25 2.25 0 005.25 21h13.5A2.25 2.25 0 0021 18.75V16.5M16.5 12L12 16.5m0 0L7.5 12m4.5 4.5V3" />
                </svg>
                {"MoA 앱 다운로드"}
              </Link>
              <button
                type="button"
                onClick={() => { setShowAppGuideModal(false); setError(''); }}
                className="w-full py-3 border border-dark-600 bg-dark-800 text-dark-200 hover:border-dark-500 hover:bg-dark-700 rounded-lg font-medium transition-colors text-sm"
              >
                {"닫기"}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
