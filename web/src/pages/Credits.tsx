import { useState, useEffect } from 'react';
import {
  CreditCard,
  Coins,
  ShoppingCart,
  History,
  Zap,
} from 'lucide-react';
import type {
  CreditBalance,
  UsdCreditPackage,
  CreditHistory,
} from '@/types/api';
import {
  getCreditBalance,
  getCreditPackages,
  createCheckout,
  getCreditHistory,
} from '@/lib/api';

export default function Credits() {
  const [balance, setBalance] = useState<CreditBalance | null>(null);
  const [packages, setPackages] = useState<UsdCreditPackage[]>([]);
  const [providers, setProviders] = useState<{ stripe: boolean; toss: boolean }>({
    stripe: false,
    toss: false,
  });
  const [history, setHistory] = useState<CreditHistory[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [purchasing, setPurchasing] = useState<string | null>(null);

  useEffect(() => {
    Promise.all([getCreditBalance(), getCreditPackages(), getCreditHistory()])
      .then(([bal, pkgs, hist]) => {
        setBalance(bal);
        setPackages(pkgs.packages);
        setProviders(pkgs.providers);
        setHistory(hist);
      })
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  const handlePurchase = async (pkg: UsdCreditPackage, provider: 'stripe' | 'toss') => {
    setPurchasing(`${pkg.id}-${provider}`);
    try {
      const resp = await createCheckout(pkg.id, provider, 'local_user');
      if (resp.checkout_url) {
        window.open(resp.checkout_url, '_blank');
      }
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Purchase failed');
    } finally {
      setPurchasing(null);
    }
  };

  if (error) {
    return (
      <div className="p-6">
        <div className="rounded-lg bg-red-900/30 border border-red-700 p-4 text-red-300">
          Failed to load credits: {error}
        </div>
      </div>
    );
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="animate-spin rounded-full h-8 w-8 border-2 border-blue-500 border-t-transparent" />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6">
      {/* Balance Card */}
      <div className="bg-gradient-to-r from-blue-900/50 to-purple-900/50 rounded-xl p-6 border border-blue-700/50">
        <div className="flex items-center gap-3 mb-2">
          <div className="p-2 bg-blue-600/30 rounded-lg">
            <Coins className="h-6 w-6 text-blue-300" />
          </div>
          <span className="text-sm text-blue-200">Credit Balance</span>
        </div>
        <p className="text-4xl font-bold text-white">
          {balance?.balance?.toLocaleString() ?? 0}
          <span className="text-lg text-blue-300 ml-2">credits</span>
        </p>
        {!balance?.enabled && (
          <p className="text-sm text-yellow-400 mt-2">
            Payment system is not configured on the server.
          </p>
        )}
      </div>

      {/* Credit Packages */}
      <div>
        <h2 className="text-lg font-semibold text-white mb-4 flex items-center gap-2">
          <ShoppingCart className="h-5 w-5" />
          Purchase Credits
        </h2>
        <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
          {packages.map((pkg) => (
            <div
              key={pkg.id}
              className="bg-gray-900 rounded-xl border border-gray-800 p-5 hover:border-blue-600/50 transition-colors"
            >
              <div className="flex items-center justify-between mb-3">
                <h3 className="text-lg font-semibold text-white">{pkg.name}</h3>
                <span className="text-2xl font-bold text-blue-400">{pkg.price_usd}</span>
              </div>
              <p className="text-3xl font-bold text-white mb-1">
                {pkg.credits.toLocaleString()}
              </p>
              <p className="text-sm text-gray-400 mb-4">credits</p>

              <div className="space-y-2">
                {providers.stripe && (
                  <button
                    type="button"
                    onClick={() => handlePurchase(pkg, 'stripe')}
                    disabled={purchasing !== null}
                    className="w-full py-2.5 bg-indigo-600 hover:bg-indigo-700 disabled:bg-gray-700 disabled:text-gray-500 text-white rounded-lg text-sm font-medium transition-colors flex items-center justify-center gap-2"
                  >
                    <CreditCard className="h-4 w-4" />
                    {purchasing === `${pkg.id}-stripe` ? 'Processing...' : 'Pay with Card (Stripe)'}
                  </button>
                )}
                {providers.toss && (
                  <button
                    type="button"
                    onClick={() => handlePurchase(pkg, 'toss')}
                    disabled={purchasing !== null}
                    className="w-full py-2.5 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-700 disabled:text-gray-500 text-white rounded-lg text-sm font-medium transition-colors flex items-center justify-center gap-2"
                  >
                    <Zap className="h-4 w-4" />
                    {purchasing === `${pkg.id}-toss`
                      ? 'Processing...'
                      : `Pay with Toss (\u20A9${pkg.price_krw.toLocaleString()})`}
                  </button>
                )}
                {!providers.stripe && !providers.toss && (
                  <p className="text-sm text-gray-500 text-center">
                    No payment provider configured
                  </p>
                )}
              </div>
            </div>
          ))}
        </div>
      </div>

      {/* Payment History */}
      <div className="bg-gray-900 rounded-xl border border-gray-800 overflow-hidden">
        <div className="px-5 py-4 border-b border-gray-800 flex items-center gap-2">
          <History className="h-5 w-5 text-gray-400" />
          <h3 className="text-base font-semibold text-white">Payment History</h3>
        </div>
        {history.length === 0 ? (
          <div className="p-8 text-center text-gray-500">No payment history yet.</div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-gray-800">
                  <th className="text-left px-5 py-3 text-gray-400 font-medium">Date</th>
                  <th className="text-left px-5 py-3 text-gray-400 font-medium">Package</th>
                  <th className="text-right px-5 py-3 text-gray-400 font-medium">Credits</th>
                  <th className="text-right px-5 py-3 text-gray-400 font-medium">Status</th>
                </tr>
              </thead>
              <tbody>
                {history.map((h) => (
                  <tr
                    key={h.transaction_id}
                    className="border-b border-gray-800/50 hover:bg-gray-800/30 transition-colors"
                  >
                    <td className="px-5 py-3 text-gray-300">
                      {new Date(h.created_at).toLocaleDateString()}
                    </td>
                    <td className="px-5 py-3 text-white font-medium">{h.package_id}</td>
                    <td className="px-5 py-3 text-gray-300 text-right">
                      +{h.credits.toLocaleString()}
                    </td>
                    <td className="px-5 py-3 text-right">
                      <span
                        className={`inline-block px-2 py-0.5 rounded-full text-xs font-medium ${
                          h.status === 'completed'
                            ? 'bg-green-900/50 text-green-300'
                            : h.status === 'pending'
                              ? 'bg-yellow-900/50 text-yellow-300'
                              : 'bg-red-900/50 text-red-300'
                        }`}
                      >
                        {h.status}
                      </span>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  );
}
