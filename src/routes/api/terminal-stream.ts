import { createFileRoute } from '@tanstack/react-router'
import { createTerminalSession } from '../../server/terminal-sessions'

export const Route = createFileRoute('/api/terminal-stream')({
  server: {
    handlers: {
      POST: async ({ request }) => {
        const body = (await request.json().catch(() => ({}))) as Record<
          string,
          unknown
        >
        const cwd = typeof body.cwd === 'string' ? body.cwd : undefined
        const cols = typeof body.cols === 'number' ? body.cols : undefined
        const rows = typeof body.rows === 'number' ? body.rows : undefined
        const command = Array.isArray(body.command)
          ? body.command.map(String)
          : undefined

        const encoder = new TextEncoder()
        const stream = new ReadableStream({
          start(controller) {
            let isStreamActive = true

            const send = (event: string, data: unknown) => {
              if (!isStreamActive || controller.desiredSize === null) return
              try {
                controller.enqueue(
                  encoder.encode(
                    `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`,
                  ),
                )
              } catch {
                isStreamActive = false
              }
            }

            let session: ReturnType<typeof createTerminalSession>

            try {
              session = createTerminalSession({
                command,
                cwd,
                cols,
                rows,
              })
            } catch (error) {
              if (import.meta.env.DEV) console.error(
                '[terminal-stream] Failed to create session:',
                error,
              )
              send('error', { message: String(error) })
              try {
                controller.close()
              } catch {
                /* */
              }
              return
            }

            send('session', { sessionId: session.id })

            const handleEvent = (evt: { event: string; payload: unknown }) => {
              if (evt.event === 'data') {
                send('data', evt.payload)
              } else if (evt.event === 'exit') {
                send('exit', evt.payload)
              }
            }

            const handleClose = () => {
              send('close', { sessionId: session.id })
              if (!isStreamActive) return
              isStreamActive = false
              try {
                controller.close()
              } catch {
                /* */
              }
            }

            session.emitter.on('event', handleEvent)
            session.emitter.on('close', handleClose)

            const keepAlive = setInterval(() => {
              send('ping', { t: Date.now() })
            }, 15000)

            const abort = () => {
              isStreamActive = false
              clearInterval(keepAlive)
              session.emitter.off('event', handleEvent)
              session.emitter.off('close', handleClose)
              session.close()
            }

            request.signal.addEventListener('abort', abort)
          },
        })

        return new Response(stream, {
          headers: {
            'Content-Type': 'text/event-stream',
            'Cache-Control': 'no-cache',
            Connection: 'keep-alive',
          },
        })
      },
    },
  },
})
