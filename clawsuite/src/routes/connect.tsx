import { createFileRoute } from '@tanstack/react-router'
import { CodeBlock } from '../components/prompt-kit/code-block'

export const Route = createFileRoute('/connect')({
  component: ConnectRoute,
})

function ConnectRoute() {
  return (
    <div className="min-h-screen bg-primary-50 text-primary-900">
      <div className="max-w-2xl mx-auto px-6 py-10 space-y-10">
        <div className="space-y-3">
          <h1 className="text-3xl font-medium tracking-[-0.02em] text-center mb-10">
            Connect to ClawSuite
          </h1>
          <p className="text-primary-700">
            This client needs access to your OpenClaw gateway before you can
            start chatting.
          </p>
        </div>
        <div className="space-y-4 text-primary-700">
          <p>
            At the root of the project, create a new file named{' '}
            <code className="inline-code">.env.local</code>.
          </p>
          <div className="space-y-3">
            <p>Paste this into it:</p>
            <CodeBlock
              content={`CLAWDBOT_GATEWAY_URL=ws://127.0.0.1:18789\nCLAWDBOT_GATEWAY_TOKEN=YOUR_TOKEN_HERE`}
              ariaLabel="Copy gateway token example"
              language="bash"
            />
            <p className="text-primary-600 text-sm">or:</p>
            <CodeBlock
              content="CLAWDBOT_GATEWAY_PASSWORD=YOUR_PASSWORD_HERE"
              ariaLabel="Copy gateway password example"
              language="bash"
            />
          </div>
          <p>
            Environment variables are loaded at startup. Restart your dev
            server:
          </p>
          <CodeBlock
            content="npm run dev"
            ariaLabel="Copy npm run dev"
            language="bash"
          />
          <p>Refresh the page after the restart and you should be connected.</p>
        </div>

        <div className="space-y-3 rounded-lg border border-primary-200 bg-primary-100 px-4 py-3 text-primary-700 text-sm">
          <p className="text-primary-900 font-medium">
            Where to find these values
          </p>
          <div className="space-y-3">
            <p>
              <code className="inline-code">CLAWDBOT_GATEWAY_URL</code>
              <br />
              Your OpenClaw gateway endpoint (default is
              <code className="inline-code">ws://127.0.0.1:18789</code>).
            </p>
            <p>
              <code className="inline-code">CLAWDBOT_GATEWAY_TOKEN</code>{' '}
              (recommended)
              <br />
              Matches your Gateway token (
              <code className="inline-code">gateway.auth.token</code> or
              <code className="inline-code">OPENCLAW_GATEWAY_TOKEN</code>).
            </p>
            <p>
              <code className="inline-code">CLAWDBOT_GATEWAY_PASSWORD</code>{' '}
              (fallback)
              <br />
              Matches your Gateway password (
              <code className="inline-code">gateway.auth.password</code>).
            </p>
          </div>
          <p>
            Gateway docs:{' '}
            <a
              className="text-primary-700 hover:text-primary-900 underline"
              href="https://docs.openclaw.ai/gateway"
              target="_blank"
              rel="noreferrer"
            >
              https://docs.openclaw.ai/gateway
            </a>
          </p>
        </div>
      </div>
    </div>
  )
}
