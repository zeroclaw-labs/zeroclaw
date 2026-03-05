import { useState } from 'react';
import { authRegister, authLogin, authKakaoCallback } from '@/lib/api';

type AuthMode = 'login' | 'register' | 'pairing';

interface AuthPageProps {
  onAuthSuccess: (token: string) => void;
  onPair: (code: string) => Promise<void>;
  showPairing: boolean;
}

const KAKAO_JS_KEY = import.meta.env.VITE_KAKAO_JS_KEY || '';
const KAKAO_REST_KEY = import.meta.env.VITE_KAKAO_REST_API_KEY || '';

function getKakaoRedirectUri(): string {
  return `${window.location.origin}/api/auth/kakao/redirect`;
}

export default function AuthPage({ onAuthSuccess, onPair, showPairing }: AuthPageProps) {
  const [mode, setMode] = useState<AuthMode>(showPairing ? 'pairing' : 'login');
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [pairingCode, setPairingCode] = useState('');
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  const [successMessage, setSuccessMessage] = useState('');

  const handleLogin = async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError('');
    try {
      const result = await authLogin(username, password);
      onAuthSuccess(result.token);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Login failed');
    } finally {
      setLoading(false);
    }
  };

  const handleRegister = async (e: React.FormEvent) => {
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

    try {
      await authRegister(username, password);
      setSuccessMessage('Registration successful! Please log in.');
      setMode('login');
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Registration failed');
    } finally {
      setLoading(false);
    }
  };

  const handlePairing = async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError('');
    try {
      await onPair(pairingCode);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Pairing failed');
    } finally {
      setLoading(false);
    }
  };

  const handleKakaoLogin = () => {
    if (!KAKAO_REST_KEY) {
      setError('Kakao login is not configured. Set VITE_KAKAO_REST_API_KEY.');
      return;
    }
    const redirectUri = getKakaoRedirectUri();
    const kakaoAuthUrl = `https://kauth.kakao.com/oauth/authorize?client_id=${KAKAO_REST_KEY}&redirect_uri=${encodeURIComponent(redirectUri)}&response_type=code`;
    window.location.href = kakaoAuthUrl;
  };

  return (
    <div className="min-h-screen bg-gray-950 flex items-center justify-center p-4">
      <div className="bg-gray-900 rounded-2xl p-8 w-full max-w-md border border-gray-800 shadow-2xl">
        {/* Logo and Title */}
        <div className="text-center mb-8">
          <img
            src={`${import.meta.env.BASE_URL}MoA_icon_128px.png`}
            alt="MoA"
            className="h-20 w-20 rounded-2xl object-cover mx-auto mb-4 shadow-lg"
          />
          <h1 className="text-3xl font-bold text-white mb-1">MoA</h1>
          <p className="text-gray-400 text-sm">Mixture of Agents</p>
        </div>

        {/* Success message */}
        {successMessage && (
          <div className="mb-4 p-3 bg-green-900/30 border border-green-700 rounded-lg text-green-300 text-sm text-center">
            {successMessage}
          </div>
        )}

        {/* Error message */}
        {error && (
          <div className="mb-4 p-3 bg-red-900/30 border border-red-700 rounded-lg text-red-300 text-sm text-center">
            {error}
          </div>
        )}

        {/* Mode tabs */}
        <div className="flex rounded-lg bg-gray-800 p-1 mb-6">
          <button
            type="button"
            onClick={() => { setMode('login'); setError(''); }}
            className={`flex-1 py-2 text-sm font-medium rounded-md transition-colors ${
              mode === 'login'
                ? 'bg-blue-600 text-white'
                : 'text-gray-400 hover:text-white'
            }`}
          >
            Login
          </button>
          <button
            type="button"
            onClick={() => { setMode('register'); setError(''); }}
            className={`flex-1 py-2 text-sm font-medium rounded-md transition-colors ${
              mode === 'register'
                ? 'bg-blue-600 text-white'
                : 'text-gray-400 hover:text-white'
            }`}
          >
            Sign Up
          </button>
          {showPairing && (
            <button
              type="button"
              onClick={() => { setMode('pairing'); setError(''); }}
              className={`flex-1 py-2 text-sm font-medium rounded-md transition-colors ${
                mode === 'pairing'
                  ? 'bg-blue-600 text-white'
                  : 'text-gray-400 hover:text-white'
              }`}
            >
              Pairing
            </button>
          )}
        </div>

        {/* Login Form */}
        {mode === 'login' && (
          <form onSubmit={handleLogin} className="space-y-4">
            <div>
              <label htmlFor="login-username" className="block text-sm font-medium text-gray-300 mb-1">
                Username
              </label>
              <input
                id="login-username"
                type="text"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                className="w-full px-4 py-3 bg-gray-800 border border-gray-700 rounded-lg text-white focus:outline-none focus:border-blue-500"
                placeholder="Enter username"
                autoFocus
                required
              />
            </div>
            <div>
              <label htmlFor="login-password" className="block text-sm font-medium text-gray-300 mb-1">
                Password
              </label>
              <input
                id="login-password"
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                className="w-full px-4 py-3 bg-gray-800 border border-gray-700 rounded-lg text-white focus:outline-none focus:border-blue-500"
                placeholder="Enter password"
                required
              />
            </div>
            <button
              type="submit"
              disabled={loading}
              className="w-full py-3 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-700 disabled:text-gray-500 text-white rounded-lg font-medium transition-colors"
            >
              {loading ? 'Logging in...' : 'Log In'}
            </button>

            {/* Kakao Login */}
            {KAKAO_REST_KEY && (
              <>
                <div className="relative my-4">
                  <div className="absolute inset-0 flex items-center">
                    <div className="w-full border-t border-gray-700" />
                  </div>
                  <div className="relative flex justify-center text-sm">
                    <span className="px-3 bg-gray-900 text-gray-500">or</span>
                  </div>
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

        {/* Register Form */}
        {mode === 'register' && (
          <form onSubmit={handleRegister} className="space-y-4">
            <div>
              <label htmlFor="reg-username" className="block text-sm font-medium text-gray-300 mb-1">
                Username
              </label>
              <input
                id="reg-username"
                type="text"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                className="w-full px-4 py-3 bg-gray-800 border border-gray-700 rounded-lg text-white focus:outline-none focus:border-blue-500"
                placeholder="Choose a username"
                autoFocus
                required
              />
            </div>
            <div>
              <label htmlFor="reg-password" className="block text-sm font-medium text-gray-300 mb-1">
                Password
              </label>
              <input
                id="reg-password"
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                className="w-full px-4 py-3 bg-gray-800 border border-gray-700 rounded-lg text-white focus:outline-none focus:border-blue-500"
                placeholder="At least 8 characters"
                minLength={8}
                required
              />
            </div>
            <div>
              <label htmlFor="reg-confirm" className="block text-sm font-medium text-gray-300 mb-1">
                Confirm Password
              </label>
              <input
                id="reg-confirm"
                type="password"
                value={confirmPassword}
                onChange={(e) => setConfirmPassword(e.target.value)}
                className="w-full px-4 py-3 bg-gray-800 border border-gray-700 rounded-lg text-white focus:outline-none focus:border-blue-500"
                placeholder="Re-enter password"
                minLength={8}
                required
              />
            </div>
            <button
              type="submit"
              disabled={loading}
              className="w-full py-3 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-700 disabled:text-gray-500 text-white rounded-lg font-medium transition-colors"
            >
              {loading ? 'Creating account...' : 'Create Account'}
            </button>
          </form>
        )}

        {/* Pairing Form */}
        {mode === 'pairing' && (
          <form onSubmit={handlePairing} className="space-y-4">
            <p className="text-gray-400 text-sm text-center mb-4">
              Enter the 6-digit pairing code from your terminal
            </p>
            <input
              type="text"
              value={pairingCode}
              onChange={(e) => setPairingCode(e.target.value)}
              placeholder="000000"
              className="w-full px-4 py-3 bg-gray-800 border border-gray-700 rounded-lg text-white text-center text-2xl tracking-widest focus:outline-none focus:border-blue-500"
              maxLength={6}
              autoFocus
            />
            <button
              type="submit"
              disabled={loading || pairingCode.length < 6}
              className="w-full py-3 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-700 disabled:text-gray-500 text-white rounded-lg font-medium transition-colors"
            >
              {loading ? 'Pairing...' : 'Pair Device'}
            </button>
          </form>
        )}
      </div>
    </div>
  );
}
