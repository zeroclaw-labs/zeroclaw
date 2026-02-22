import { useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { Shield, Loader2 } from 'lucide-react';
import { requestOtp, verifyOtp } from '../api/auth';
import { useAuth } from '../hooks/useAuth';

export default function Login() {
  const [email, setEmail] = useState('');
  const [code, setCode] = useState('');
  const [step, setStep] = useState<'email' | 'code'>('email');
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  const navigate = useNavigate();
  const { login, isLoggedIn } = useAuth();

  if (isLoggedIn) { navigate('/', { replace: true }); return null; }

  const handleRequestOtp = async () => {
    setLoading(true); setError('');
    try {
      await requestOtp(email);
      setStep('code');
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : 'Failed to send OTP');
    }
    setLoading(false);
  };

  const handleVerify = async () => {
    setLoading(true); setError('');
    try {
      const data = await verifyOtp(email, code);
      login(data.token, email);
      navigate('/');
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : 'Verification failed');
    }
    setLoading(false);
  };

  return (
    <div className="min-h-screen flex items-center justify-center bg-bg-primary">
      <div className="w-full max-w-sm p-8 bg-bg-card border border-border-default rounded-xl">
        <div className="flex items-center justify-center gap-2 mb-6">
          <Shield className="h-7 w-7 text-accent-blue" />
          <h1 className="text-2xl font-bold text-text-primary">ZeroClaw Platform</h1>
        </div>
        {step === 'email' ? (
          <form onSubmit={e => { e.preventDefault(); handleRequestOtp(); }}>
            <label className="block text-sm font-medium text-text-secondary mb-1">Email</label>
            <input type="email" value={email} onChange={e => setEmail(e.target.value)}
              className="w-full px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary placeholder-text-muted focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors"
              placeholder="admin@example.com" required />
            <button type="submit" disabled={loading}
              className="w-full mt-4 px-4 py-2 bg-accent-blue text-white rounded-lg hover:bg-accent-blue-hover disabled:opacity-50 transition-colors flex items-center justify-center gap-2 font-medium">
              {loading && <Loader2 className="h-4 w-4 animate-spin" />}
              {loading ? 'Sending...' : 'Send OTP'}
            </button>
          </form>
        ) : (
          <form onSubmit={e => { e.preventDefault(); handleVerify(); }}>
            <p className="text-sm text-text-secondary mb-4">Check your email for a 6-digit code.</p>
            <label className="block text-sm font-medium text-text-secondary mb-1">OTP Code</label>
            <input type="text" value={code} onChange={e => setCode(e.target.value)}
              className="w-full px-3 py-2 bg-bg-input border border-border-default rounded-lg text-text-primary text-center text-2xl tracking-widest placeholder-text-muted focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors"
              maxLength={6} placeholder="000000" required />
            <button type="submit" disabled={loading}
              className="w-full mt-4 px-4 py-2 bg-accent-blue text-white rounded-lg hover:bg-accent-blue-hover disabled:opacity-50 transition-colors flex items-center justify-center gap-2 font-medium">
              {loading && <Loader2 className="h-4 w-4 animate-spin" />}
              {loading ? 'Verifying...' : 'Verify'}
            </button>
            <button type="button" onClick={() => setStep('email')}
              className="w-full mt-2 px-4 py-2 text-text-muted hover:text-text-secondary text-sm transition-colors">Back</button>
          </form>
        )}
        {error && <p className="mt-4 text-status-error text-sm text-center">{error}</p>}
      </div>
    </div>
  );
}
