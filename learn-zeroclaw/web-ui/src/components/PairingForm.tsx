import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { pair } from "@/lib/api";

const PAIRING_CMD = "docker logs zeroclaw-learn 2>&1 | grep -A3 'one-time code'";

interface Props {
  onPaired: (token: string) => void;
}

export function PairingForm({ onPaired }: Props) {
  const [code, setCode] = useState("");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [copied, setCopied] = useState(false);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError("");
    setLoading(true);
    try {
      const result = await pair(code);
      if (result.paired && result.token) {
        localStorage.setItem("zeroclaw_token", result.token);
        onPaired(result.token);
      } else {
        setError(result.error ?? "Pairing failed");
      }
    } catch {
      setError("Cannot connect to ZeroClaw gateway");
    } finally {
      setLoading(false);
    }
  };

  const copyCommand = async () => {
    await navigator.clipboard.writeText(PAIRING_CMD);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <div className="flex h-screen items-center justify-center bg-background">
      <Card className="w-full max-w-md">
        <CardHeader className="text-center">
          <CardTitle className="text-2xl">ZeroClaw</CardTitle>
          <CardDescription>
            Enter the 6-digit pairing code from the gateway logs
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <form onSubmit={handleSubmit} className="space-y-4">
            <Input
              type="text"
              inputMode="numeric"
              maxLength={6}
              placeholder="000000"
              className="text-center text-2xl tracking-[0.5em] font-mono"
              value={code}
              onChange={(e) => setCode(e.target.value.replace(/\D/g, ""))}
              autoFocus
            />
            {error && <p className="text-sm text-destructive text-center">{error}</p>}
            <Button type="submit" className="w-full" disabled={code.length !== 6 || loading}>
              {loading ? "Pairing..." : "Connect"}
            </Button>
          </form>

          <div className="border-t pt-4">
            <p className="text-xs text-muted-foreground mb-2 text-center">
              Run this command in your terminal to get the pairing code:
            </p>
            <button
              type="button"
              onClick={copyCommand}
              className="w-full rounded-md bg-muted px-3 py-2 text-left font-mono text-xs text-muted-foreground hover:bg-muted/80 transition-colors cursor-pointer relative group"
            >
              <code className="break-all">{PAIRING_CMD}</code>
              <span className="absolute right-2 top-1/2 -translate-y-1/2 text-[10px] opacity-0 group-hover:opacity-100 transition-opacity">
                {copied ? "Copied!" : "Click to copy"}
              </span>
            </button>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}
