import { randomUUID, generateKeyPairSync, createPrivateKey, createPublicKey, createHash, sign as cryptoSign } from 'node:crypto'
import * as fs from 'node:fs'
import * as path from 'node:path'
import * as os from 'node:os'
import WebSocket from 'ws'
import type { RawData } from 'ws'

export type GatewayFrame =
  | { type: 'req'; id: string; method: string; params?: unknown }
  | {
      type: 'res'
      id: string
      ok: boolean
      payload?: unknown
      error?: { code: string; message: string; details?: unknown }
    }
  | { type: 'event'; event: string; payload?: unknown; seq?: number }
  | {
      type: 'evt'
      event: string
      payload?: unknown
      payloadJSON?: string
      seq?: number
    }

type ConnectParams = {
  minProtocol: number
  maxProtocol: number
  client: {
    id: string
    displayName?: string
    version: string
    platform: string
    mode: string
    instanceId?: string
  }
  auth?: { token?: string; password?: string }
  role?: 'operator' | 'node'
  scopes?: Array<string>
  device?: { id: string; publicKey: string; signature: string; signedAt: number; nonce?: string }
}

type PendingRequest = {
  id: string
  method: string
  params?: unknown
  resolve: (value: unknown) => void
  reject: (reason?: unknown) => void
}

type InflightRequest = {
  resolve: (value: unknown) => void
  reject: (reason?: unknown) => void
}

// ── Device Identity (Ed25519) ─────────────────────────────────────
const ED25519_SPKI_PREFIX = Buffer.from('302a300506032b6570032100', 'hex')

function base64UrlEncode(buf: Buffer): string {
  return buf.toString('base64').replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '')
}

function derivePublicKeyRaw(pem: string): Buffer {
  const spki = createPublicKey(pem).export({ type: 'spki', format: 'der' })
  if (spki.length === ED25519_SPKI_PREFIX.length + 32 &&
      spki.subarray(0, ED25519_SPKI_PREFIX.length).equals(ED25519_SPKI_PREFIX))
    return spki.subarray(ED25519_SPKI_PREFIX.length)
  return spki
}

type DeviceIdentity = { deviceId: string; publicKeyPem: string; privateKeyPem: string }

let _identity: DeviceIdentity | null = null
function getDeviceIdentity(): DeviceIdentity {
  if (_identity) return _identity
  const idPath = path.join(
    process.env.OPENCLAW_STATE_DIR || path.join(os.homedir(), '.openclaw', 'state'),
    'identity', 'clawsuite-device.json')
  try {
    if (fs.existsSync(idPath)) {
      const p = JSON.parse(fs.readFileSync(idPath, 'utf8'))
      if (p?.version === 1 && p.deviceId && p.publicKeyPem && p.privateKeyPem) {
        _identity = { deviceId: p.deviceId, publicKeyPem: p.publicKeyPem, privateKeyPem: p.privateKeyPem }
        return _identity
      }
    }
  } catch { /* regenerate */ }
  const { publicKey, privateKey } = generateKeyPairSync('ed25519')
  const pubPem = publicKey.export({ type: 'spki', format: 'pem' }).toString()
  const privPem = privateKey.export({ type: 'pkcs8', format: 'pem' }).toString()
  const deviceId = createHash('sha256').update(derivePublicKeyRaw(pubPem)).digest('hex')
  fs.mkdirSync(path.dirname(idPath), { recursive: true })
  fs.writeFileSync(idPath, JSON.stringify({ version: 1, deviceId, publicKeyPem: pubPem, privateKeyPem: privPem, createdAtMs: Date.now() }, null, 2) + '\n', { mode: 0o600 })
  _identity = { deviceId, publicKeyPem: pubPem, privateKeyPem: privPem }
  return _identity
}

function signPayload(privPem: string, payload: string): string {
  return base64UrlEncode(cryptoSign(null, Buffer.from(payload, 'utf8'), createPrivateKey(privPem)) as unknown as Buffer)
}

// ── Constants ─────────────────────────────────────────────────────
const RECONNECT_DELAYS_MS = [1000, 2000, 4000]
const MAX_RECONNECT_DELAY_MS = 30000
const HEARTBEAT_INTERVAL_MS = 30000
const HEARTBEAT_TIMEOUT_MS = 20000
const HANDSHAKE_TIMEOUT_MS = 15000

export function getGatewayConfig() {
  // Check if browser set a custom gateway URL (for network/mobile access)
  const browserUrl = typeof window !== 'undefined' ? (window as any).__GATEWAY_URL__ : undefined
  const url = browserUrl || process.env.CLAWDBOT_GATEWAY_URL?.trim() || 'ws://127.0.0.1:18789'
  const token = process.env.CLAWDBOT_GATEWAY_TOKEN?.trim() || ''
  const password = process.env.CLAWDBOT_GATEWAY_PASSWORD?.trim() || ''

  // Allow connecting without shared auth — device identity signature handles authentication.
  // Some gateways (e.g. nanobot) run without a token by default.

  return { url, token, password }
}

export function buildConnectParams(
  token: string,
  password: string,
  nonce?: string,
): ConnectParams {
  const identity = getDeviceIdentity()
  const role = 'operator'
  const scopes = ['operator.admin']
  const signedAtMs = Date.now()
  const clientId = 'openclaw-control-ui'
  const clientMode = 'ui'
  const version = nonce ? 'v2' : 'v1'
  const parts = [version, identity.deviceId, clientId, clientMode, role, scopes.join(','), String(signedAtMs), token || '']
  if (version === 'v2') parts.push(nonce || '')
  const signature = signPayload(identity.privateKeyPem, parts.join('|'))

  return {
    minProtocol: 3,
    maxProtocol: 3,
    client: {
      id: clientId,
      displayName: 'clawsuite',
      version: 'dev',
      platform: process.platform,
      mode: clientMode,
      instanceId: randomUUID(),
    },
    auth: {
      token: token || undefined,
      password: password || undefined,
    },
    role,
    scopes,
    device: {
      id: identity.deviceId,
      publicKey: base64UrlEncode(derivePublicKeyRaw(identity.publicKeyPem)),
      signature,
      signedAt: signedAtMs,
      nonce,
    },
  }
}

export type GatewayEventHandler = (frame: GatewayFrame) => void

class GatewayClient {
  private ws: WebSocket | null = null
  private connectPromise: Promise<void> | null = null
  private reconnectTimer: NodeJS.Timeout | null = null
  private heartbeatInterval: NodeJS.Timeout | null = null
  private heartbeatTimeout: NodeJS.Timeout | null = null
  private reconnectAttempts = 0
  private authenticated = false
  private destroyed = false

  private requestQueue: Array<PendingRequest> = []
  private inflight = new Map<string, InflightRequest>()
  private eventListeners = new Set<GatewayEventHandler>()

  onEvent(handler: GatewayEventHandler): () => void {
    this.eventListeners.add(handler)
    return () => {
      this.eventListeners.delete(handler)
    }
  }

  async request<TPayload = unknown>(
    method: string,
    params?: unknown,
  ): Promise<TPayload> {
    if (this.destroyed) {
      throw new Error('Gateway client is shut down')
    }

    return new Promise<TPayload>((resolve, reject) => {
      const request: PendingRequest = {
        id: randomUUID(),
        method,
        params,
        resolve: resolve as (value: unknown) => void,
        reject,
      }

      this.requestQueue.push(request)
      this.ensureConnected().catch(() => {
        // keep requests queued; reconnect loop will flush after reconnect
      })
      this.flushQueue()
    })
  }

  async ensureConnected(): Promise<void> {
    if (this.destroyed) {
      throw new Error('Gateway client is shut down')
    }
    if (this.authenticated && this.ws?.readyState === WebSocket.OPEN) {
      return
    }
    if (this.connectPromise) {
      return this.connectPromise
    }

    this.connectPromise = this.openAndHandshake()
      .then(() => {
        this.reconnectAttempts = 0
      })
      .catch((error: unknown) => {
        const err = error instanceof Error ? error : new Error(String(error))
        this.scheduleReconnect()
        throw err
      })
      .finally(() => {
        this.connectPromise = null
      })

    return this.connectPromise
  }

  async shutdown(): Promise<void> {
    this.destroyed = true
    this.clearReconnectTimer()
    this.stopHeartbeat()

    const ws = this.ws
    this.ws = null
    this.authenticated = false

    const closePromise = ws ? this.closeSocket(ws) : Promise.resolve()

    this.rejectQueuedRequests(new Error('Gateway client is shut down'))
    this.rejectInflightRequests(new Error('Gateway client is shut down'))

    await closePromise.catch(() => {
      // ignore
    })
  }

  private async openAndHandshake(): Promise<void> {
    let lastError: Error | null = null
    const maxRetries = 2

    for (let attempt = 0; attempt <= maxRetries; attempt++) {
      try {
        if (attempt > 0) {
          // Wait a bit before retry (WebSocket Race Condition mitigation)
          await new Promise((resolve) => setTimeout(resolve, 500 * attempt))
        }

        const { url, token, password } = getGatewayConfig()
        const ws = new WebSocket(url, { origin: 'http://localhost:3000', headers: { Origin: 'http://localhost:3000' } })

        this.clearReconnectTimer()
        this.attachSocket(ws)

        await this.waitForOpen(ws, HANDSHAKE_TIMEOUT_MS)

        if (this.destroyed) {
          ws.terminate()
          throw new Error('Gateway client is shut down')
        }

        this.ws = ws
        this.authenticated = false

        // Wait for connect.challenge to get nonce
        const nonce = await new Promise<string | undefined>((resolve) => {
          const challengeHandler = (data: RawData) => {
            try {
              const f = JSON.parse(rawDataToString(data))
              if ((f.type === 'event' || f.type === 'evt') && f.event === 'connect.challenge') {
                ws.removeListener('message', challengeHandler)
                resolve(f.payload?.nonce || undefined)
                return
              }
            } catch { /* ignore */ }
          }
          ws.removeAllListeners('message')
          ws.on('message', challengeHandler)
          // Fallback if no challenge (older gateway)
          setTimeout(() => {
            ws.removeListener('message', challengeHandler)
            resolve(undefined)
          }, 3000)
        })
        // Re-attach the normal message handler
        ws.removeAllListeners('message')
        ws.on('message', (data: RawData) => { this.handleMessage(data) })

        const connectId = randomUUID()
        const connectReq: GatewayFrame = {
          type: 'req',
          id: connectId,
          method: 'connect',
          params: buildConnectParams(token, password, nonce),
        }

        await new Promise<void>((resolve, reject) => {
          const timeout = setTimeout(() => {
            this.inflight.delete(connectId)
            reject(new Error('Gateway handshake timed out'))
          }, HANDSHAKE_TIMEOUT_MS)

          this.inflight.set(connectId, {
            resolve: () => {
              clearTimeout(timeout)
              resolve()
            },
            reject: (err) => {
              clearTimeout(timeout)
              reject(err)
            },
          })

          this.sendFrame(connectReq).catch((error: unknown) => {
            this.inflight.delete(connectId)
            clearTimeout(timeout)
            reject(error)
          })
        })

        this.authenticated = true
        this.startHeartbeat()
        this.flushQueue()
        return // Success
      } catch (error) {
        lastError = error instanceof Error ? error : new Error(String(error))
        if (this.ws) {
          this.ws.terminate()
          this.ws = null
        }
        if (this.destroyed) break
      }
    }

    throw lastError || new Error('Failed to connect to gateway after retries')
  }

  private attachSocket(ws: WebSocket) {
    ws.on('message', (data) => {
      this.handleMessage(data)
    })

    ws.on('pong', () => {
      if (this.heartbeatTimeout) {
        clearTimeout(this.heartbeatTimeout)
        this.heartbeatTimeout = null
      }
    })

    ws.on('close', (code, reason) => {
      console.log(`[gateway] WebSocket closed: code=${code} reason=${reason?.toString() || 'n/a'}`)
      this.handleDisconnect(new Error(`Gateway connection closed (code=${code})`))
    })

    ws.on('error', (error) => {
      const err = error instanceof Error ? error : new Error(String(error))
      this.handleDisconnect(err)
    })
  }

  private handleMessage(data: RawData) {
    let frame: GatewayFrame

    try {
      frame = JSON.parse(rawDataToString(data)) as GatewayFrame
    } catch {
      return
    }

    if (frame.type === 'event' || frame.type === 'evt') {
      for (const listener of this.eventListeners) {
        try {
          listener(frame)
        } catch {
          // ignore listener errors
        }
      }
      return
    }

    if (frame.type !== 'res') return

    const pending = this.inflight.get(frame.id)
    if (!pending) return

    this.inflight.delete(frame.id)

    if (frame.ok) {
      pending.resolve(frame.payload)
      return
    }

    pending.reject(new Error(frame.error?.message ?? 'gateway error'))
  }

  private handleDisconnect(error: Error) {
    const ws = this.ws
    this.ws = null
    this.authenticated = false
    this.stopHeartbeat()

    if (
      ws &&
      (ws.readyState === WebSocket.OPEN ||
        ws.readyState === WebSocket.CONNECTING)
    ) {
      try {
        ws.terminate()
      } catch {
        // ignore
      }
    }

    this.rejectInflightRequests(error)

    if (this.destroyed) {
      this.rejectQueuedRequests(error)
      return
    }

    this.scheduleReconnect()
  }

  private flushQueue() {
    if (
      !this.authenticated ||
      !this.ws ||
      this.ws.readyState !== WebSocket.OPEN
    ) {
      return
    }

    while (this.requestQueue.length > 0) {
      const pending = this.requestQueue.shift()
      if (!pending) continue

      const frame: GatewayFrame = {
        type: 'req',
        id: pending.id,
        method: pending.method,
        params: pending.params,
      }

      this.inflight.set(pending.id, {
        resolve: pending.resolve,
        reject: pending.reject,
      })

      this.sendFrame(frame).catch((error: unknown) => {
        this.inflight.delete(pending.id)
        pending.reject(error)
      })
    }
  }

  private scheduleReconnect() {
    if (this.destroyed || this.reconnectTimer || this.connectPromise) {
      return
    }

    const delay = nextReconnectDelayMs(this.reconnectAttempts)
    this.reconnectAttempts += 1

    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null
      this.ensureConnected()
        .then(() => {
          this.flushQueue()
        })
        .catch(() => {
          // next reconnect is scheduled by ensureConnected/openAndHandshake
        })
    }, delay)
  }

  private startHeartbeat() {
    this.stopHeartbeat()

    this.heartbeatInterval = setInterval(() => {
      if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
        return
      }

      try {
        this.ws.ping()
        console.log('[gateway] ping sent')
      } catch {
        console.log('[gateway] ping FAILED to send')
        this.handleDisconnect(new Error('Gateway ping failed'))
        return
      }

      if (this.heartbeatTimeout) {
        clearTimeout(this.heartbeatTimeout)
      }

      this.heartbeatTimeout = setTimeout(() => {
        this.heartbeatTimeout = null
        console.log('[gateway] PONG TIMEOUT — gateway did not respond in 20s')
        this.handleDisconnect(new Error('Gateway ping timeout'))
      }, HEARTBEAT_TIMEOUT_MS)
    }, HEARTBEAT_INTERVAL_MS)
  }

  private stopHeartbeat() {
    if (this.heartbeatInterval) {
      clearInterval(this.heartbeatInterval)
      this.heartbeatInterval = null
    }
    if (this.heartbeatTimeout) {
      clearTimeout(this.heartbeatTimeout)
      this.heartbeatTimeout = null
    }
  }

  private async sendFrame(frame: GatewayFrame): Promise<void> {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      throw new Error('Gateway connection not open')
    }

    await new Promise<void>((resolve, reject) => {
      this.ws?.send(JSON.stringify(frame), (err) => {
        if (err) {
          reject(err)
          return
        }
        resolve()
      })
    })
  }

  private waitForOpen(ws: WebSocket, timeoutMs: number): Promise<void> {
    if (ws.readyState === WebSocket.OPEN) return Promise.resolve()

    return new Promise<void>((resolve, reject) => {
      const timeout = setTimeout(() => {
        cleanup()
        reject(new Error('WebSocket connection timed out'))
      }, timeoutMs)

      function onOpen() {
        cleanup()
        resolve()
      }

      function onError(error: Error) {
        cleanup()
        reject(new Error(`WebSocket error: ${String(error.message)}`))
      }

      function cleanup() {
        clearTimeout(timeout)
        ws.off('open', onOpen)
        ws.off('error', onError)
      }

      ws.on('open', onOpen)
      ws.on('error', onError)
    })
  }

  private closeSocket(ws: WebSocket): Promise<void> {
    if (
      ws.readyState === WebSocket.CLOSED ||
      ws.readyState === WebSocket.CLOSING
    ) {
      return Promise.resolve()
    }

    return new Promise<void>((resolve) => {
      ws.once('close', () => resolve())
      ws.close()
    })
  }

  private clearReconnectTimer() {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }
  }

  private rejectQueuedRequests(error: Error) {
    for (const pending of this.requestQueue) {
      pending.reject(error)
    }
    this.requestQueue = []
  }

  private rejectInflightRequests(error: Error) {
    for (const pending of this.inflight.values()) {
      pending.reject(error)
    }
    this.inflight.clear()
  }
}

function nextReconnectDelayMs(attempt: number) {
  if (attempt < RECONNECT_DELAYS_MS.length) {
    return RECONNECT_DELAYS_MS[attempt]
  }

  const doubled =
    RECONNECT_DELAYS_MS[RECONNECT_DELAYS_MS.length - 1] * 2 ** (attempt - 2)
  return Math.min(doubled, MAX_RECONNECT_DELAY_MS)
}

function rawDataToString(data: RawData): string {
  if (typeof data === 'string') return data
  if (Array.isArray(data)) return Buffer.concat(data).toString('utf8')
  return data.toString()
}

// Singleton guard: survive Vite SSR module reloads
const GW_KEY = '__clawsuite_gateway_client__' as const
declare global {
  // eslint-disable-next-line no-var
  var __clawsuite_gateway_client__: GatewayClient | undefined
}
const existingClient = (globalThis as any)[GW_KEY] as GatewayClient | undefined
if (existingClient) {
  console.log('[gateway] Reusing existing GatewayClient singleton (Vite SSR reload survived)')
}
let gatewayClient: GatewayClient = existingClient ?? new GatewayClient()
if (!existingClient) {
  console.log('[gateway] Created NEW GatewayClient (first load)')
}
;(globalThis as any)[GW_KEY] = gatewayClient

export async function gatewayRpc<TPayload = unknown>(
  method: string,
  params?: unknown,
): Promise<TPayload> {
  return gatewayClient.request<TPayload>(method, params)
}

export function onGatewayEvent(handler: GatewayEventHandler): () => void {
  return gatewayClient.onEvent(handler)
}

export async function gatewayConnectCheck(): Promise<void> {
  await gatewayClient.ensureConnected()
}

export async function cleanupGatewayConnection(): Promise<void> {
  await gatewayClient.shutdown()
}

/**
 * Force-reconnect the gateway client with current process.env values.
 * Call this after updating CLAWDBOT_GATEWAY_URL / CLAWDBOT_GATEWAY_TOKEN.
 */
export async function gatewayReconnect(): Promise<void> {
  await gatewayClient.shutdown()
  gatewayClient = new GatewayClient()
  ;(globalThis as any)[GW_KEY] = gatewayClient
  await gatewayClient.ensureConnected()
}
