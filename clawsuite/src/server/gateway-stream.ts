import { randomUUID } from 'node:crypto'
import EventEmitter from 'node:events'
import WebSocket from 'ws'

import { buildConnectParams, getGatewayConfig } from './gateway'
import type { GatewayFrame } from './gateway'

export type GatewayStreamEvent =
  | { type: 'agent'; payload: any }
  | { type: 'chat'; payload: any }
  | { type: 'other'; event: string; payload: any }

class GatewayStreamConnection extends EventEmitter {
  private ws: WebSocket | null = null
  private pending = new Map<
    string,
    { resolve: (value: any) => void; reject: (err: Error) => void }
  >()
  private closed = false

  async open(): Promise<void> {
    const { url, token, password } = getGatewayConfig()
    const connectParams = buildConnectParams(token, password)
    const ws = new WebSocket(url, { origin: 'http://localhost:3000', headers: { Origin: 'http://localhost:3000' } })
    this.ws = ws

    await this.waitForOpen(ws)

    ws.on('message', (data) => {
      this.handleMessage(data)
    })
    ws.on('close', () => {
      this.closed = true
      this.failPending(new Error('Gateway connection closed'))
      this.emit('close')
    })
    ws.on('error', (err) => {
      this.failPending(err instanceof Error ? err : new Error(String(err)))
      this.emit('error', err)
    })

    // Send connect frame
    const connectId = randomUUID()
    const connectFrame: GatewayFrame = {
      type: 'req',
      id: connectId,
      method: 'connect',
      params: connectParams,
    }

    await this.sendFrame(connectFrame)
    await this.waitForResponse(connectId)
  }

  async close(): Promise<void> {
    if (this.closed) return
    this.closed = true
    if (this.ws) {
      try {
        await new Promise<void>((resolve) => {
          this.ws?.once('close', () => resolve())
          this.ws?.close()
        })
      } catch {
        // ignore
      }
    }
    this.failPending(new Error('Connection closed'))
  }

  async request<T = unknown>(method: string, params?: unknown): Promise<T> {
    const id = randomUUID()
    const frame: GatewayFrame = {
      type: 'req',
      id,
      method,
      params,
    }

    await this.sendFrame(frame)
    const res = (await this.waitForResponse(id)) as {
      type: 'res'
      ok: boolean
      payload?: unknown
      error?: { code: string; message: string }
    }

    // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
    if (!res || res.type !== 'res') {
      throw new Error('Invalid gateway response')
    }
    if (!res.ok) {
      const message = res.error?.message ?? 'Gateway request failed'
      throw new Error(message)
    }
    return res.payload as T
  }

  private async sendFrame(frame: GatewayFrame): Promise<void> {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      throw new Error('Gateway connection not open')
    }
    await new Promise<void>((resolve, reject) => {
      this.ws?.send(JSON.stringify(frame), (err) => {
        if (err) reject(err)
        else resolve()
      })
    })
  }

  private waitForResponse(id: string): Promise<GatewayFrame> {
    return new Promise<GatewayFrame>((resolve, reject) => {
      this.pending.set(id, { resolve, reject })
    })
  }

  private waitForOpen(ws: WebSocket): Promise<void> {
    if (ws.readyState === WebSocket.OPEN) return Promise.resolve()
    return new Promise((resolve, reject) => {
      const handleOpen = () => {
        cleanup()
        resolve()
      }
      const handleError = (err: Error) => {
        cleanup()
        reject(err)
      }
      const cleanup = () => {
        ws.off('open', handleOpen)
        ws.off('error', handleError)
      }
      ws.once('open', handleOpen)
      ws.once('error', handleError)
    })
  }

  private handleMessage(raw: WebSocket.RawData) {
    try {
      const text = typeof raw === 'string' ? raw : raw.toString('utf8')
      const frame = JSON.parse(text) as GatewayFrame & {
        payloadJSON?: unknown
      }
      if (frame.type === 'res') {
        const waiter = this.pending.get(frame.id)
        if (waiter) {
          this.pending.delete(frame.id)
          waiter.resolve(frame)
        }
        return
      }
      // Handle both 'evt' (protocol v3) and 'event' (legacy) frame types
      if (frame.type === 'evt' || frame.type === 'event') {
        const payload = parsePayload(frame)
        // Emit by specific event name (chat, agent, etc.)
        this.emit(frame.event, payload)
        // Also emit generic 'event' for catch-all listeners
        this.emit('event', frame)
        // Legacy: emit 'other' for unrecognized events
        if (frame.event !== 'agent' && frame.event !== 'chat') {
          this.emit('other', frame.event, payload)
        }
      }
    } catch (err) {
      this.emit('error', err)
    }
  }

  private failPending(err: Error) {
    for (const waiter of this.pending.values()) {
      waiter.reject(err)
    }
    this.pending.clear()
  }
}

function parsePayload(frame: { payload?: unknown; payloadJSON?: unknown }) {
  if (frame.payload !== undefined) return frame.payload
  if (typeof frame.payloadJSON === 'string') {
    try {
      return JSON.parse(frame.payloadJSON)
    } catch {
      return null
    }
  }
  return null
}

export async function createGatewayStreamConnection() {
  const conn = new GatewayStreamConnection()
  await conn.open()
  return conn
}
