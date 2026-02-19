export type ActivityEvent = {
  id: string
  timestamp: number
  type: 'gateway' | 'model' | 'usage' | 'cron' | 'tool' | 'error' | 'session'
  title: string
  detail?: string
  level: 'debug' | 'info' | 'warn' | 'error'
  source?: string
}
