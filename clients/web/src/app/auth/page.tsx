'use client';

import { Suspense, useState, useEffect, useCallback, useRef } from 'react';
import { useRouter, useSearchParams } from 'next/navigation';
import Link from 'next/link';
import { isAuthenticated, setToken } from '@/lib/auth';
import {
  authRegister,
  authLogin,
  remoteLogin,
  verifyRemoteEmail,
  getRemoteDevices,
  type UserDevice,
  type AuthLoginResponse,
} from '@/lib/gateway-api';

type AuthStep =
  | 'login'
  | 'register'
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

  // Login/Register fields
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [email, setEmail] = useState('');
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  const [successMessage, setSuccessMessage] = useState('');

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
        // Single device - skip device selection, go to pairing code
        setSelectedDeviceId(devices[0].device_id);
        setSelectedDeviceName(devices[0].device_name);
        setStep('pairing-code');
      } else {
        // Multiple devices - show device selection
        setStep('device-select');
      }
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Login failed');
    } finally {
      setLoading(false);
    }
  }, [username, password, router, redirectTo]);

  const handleRegister = useCallback(async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError('');
    setSuccessMessage('');

    if (password !== confirmPassword) {
      setError('Passwords do not match');
      setLoading(false);
      return;
    }

    if (password.length < 8) {
      setError('Password must be at least 8 characters');
      setLoading(false);
      return;
    }

    if (!email || !email.includes('@')) {
      setError('Please enter a valid email address');
      setLoading(false);
      return;
    }

    try {
      await authRegister(username, password, email);
      setSuccessMessage('Registration successful! Please log in.');
      setStep('login');
      setConfirmPassword('');
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Registration failed');
    } finally {
      setLoading(false);
    }
  }, [username, password, confirmPassword, email]);

  const handleDeviceSelect = useCallback(() => {
    if (!selectedDeviceId) {
      setError('Please select a device');
      return;
    }
    setError('');
    const device = userDevices.find((d) => d.device_id === selectedDeviceId);
    setSelectedDeviceName(device?.device_name || '');
    setStep('pairing-code');
  }, [selectedDeviceId, userDevices]);

  const handlePairingCode = useCallback(async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError('');

    if (!pairingCode.trim()) {
      setError('Please enter the pairing code');
      setLoading(false);
      return;
    }

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
    } else if (step === 'register') {
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
              <span className="text-2xl font-bold text-primary-400">Z</span>
            </div>
          </Link>
          <h1 className="text-2xl font-bold text-dark-50">ZeroClaw</h1>
          <p className="text-dark-400 text-sm mt-1">
            {step === 'login' && 'Sign in to your account'}
            {step === 'register' && 'Create your account'}
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
        {step !== 'login' && step !== 'register' && (
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

        {/* Step: Login / Register tabs */}
        {(step === 'login' || step === 'register') && (
          <>
            <div className="flex rounded-lg bg-dark-800 p-1 mb-6">
              <button
                type="button"
                onClick={() => { setStep('login'); setError(''); }}
                className={`flex-1 py-2 text-sm font-medium rounded-md transition-colors ${step === 'login' ? 'bg-primary-500 text-white' : 'text-dark-400 hover:text-white'}`}
              >
                Login
              </button>
              <button
                type="button"
                onClick={() => { setStep('register'); setError(''); }}
                className={`flex-1 py-2 text-sm font-medium rounded-md transition-colors ${step === 'register' ? 'bg-primary-500 text-white' : 'text-dark-400 hover:text-white'}`}
              >
                Sign Up
              </button>
            </div>

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
                  {loading ? 'Logging in...' : 'Log In'}
                </button>

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

            {step === 'register' && (
              <form onSubmit={handleRegister} className="space-y-4">
                <div>
                  <label htmlFor="reg-username" className="block text-sm font-medium text-dark-300 mb-1">Username</label>
                  <input
                    id="reg-username"
                    type="text"
                    value={username}
                    onChange={(e) => setUsername(e.target.value)}
                    className="w-full px-4 py-3 bg-dark-800 border border-dark-700 rounded-lg text-dark-100 focus:outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500/30 transition-all"
                    placeholder="Choose a username"
                    autoFocus
                    required
                  />
                </div>
                <div>
                  <label htmlFor="reg-email" className="block text-sm font-medium text-dark-300 mb-1">
                    Email <span className="text-red-400">*</span>
                  </label>
                  <input
                    id="reg-email"
                    type="email"
                    value={email}
                    onChange={(e) => setEmail(e.target.value)}
                    className="w-full px-4 py-3 bg-dark-800 border border-dark-700 rounded-lg text-dark-100 focus:outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500/30 transition-all"
                    placeholder="your@email.com"
                    required
                  />
                  <p className="text-xs text-dark-500 mt-1">Used for email verification during web chat login</p>
                </div>
                <div>
                  <label htmlFor="reg-password" className="block text-sm font-medium text-dark-300 mb-1">Password</label>
                  <input
                    id="reg-password"
                    type="password"
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                    className="w-full px-4 py-3 bg-dark-800 border border-dark-700 rounded-lg text-dark-100 focus:outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500/30 transition-all"
                    placeholder="At least 8 characters"
                    minLength={8}
                    required
                  />
                </div>
                <div>
                  <label htmlFor="reg-confirm" className="block text-sm font-medium text-dark-300 mb-1">Confirm Password</label>
                  <input
                    id="reg-confirm"
                    type="password"
                    value={confirmPassword}
                    onChange={(e) => setConfirmPassword(e.target.value)}
                    className="w-full px-4 py-3 bg-dark-800 border border-dark-700 rounded-lg text-dark-100 focus:outline-none focus:border-primary-500 focus:ring-1 focus:ring-primary-500/30 transition-all"
                    placeholder="Re-enter password"
                    minLength={8}
                    required
                  />
                </div>
                <button
                  type="submit"
                  disabled={loading}
                  className="w-full py-3 bg-primary-500 hover:bg-primary-600 disabled:bg-dark-700 disabled:text-dark-500 text-white rounded-lg font-medium transition-colors"
                >
                  {loading ? 'Creating account...' : 'Create Account'}
                </button>
              </form>
            )}
          </>
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
    </div>
  );
}
