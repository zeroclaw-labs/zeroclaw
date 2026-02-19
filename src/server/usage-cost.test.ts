import { describe, expect, it } from 'vitest'
import {
  buildCostSummary,
  buildUsageSummary,
  SENSITIVE_PATTERN,
} from './usage-cost'

describe('usage-cost security', function suite() {
  it('buildUsageSummary output contains no sensitive keywords', function test() {
    const usage = buildUsageSummary({
      configuredProviders: ['anthropic', 'minimax'],
      sessionsUsagePayload: {
        updatedAt: 1739011200000,
        totals: {
          input: 1200,
          output: 3400,
          totalTokens: 4600,
          totalCost: 1.24,
          token: 'should-not-leak',
          secret: 'do-not-leak',
        },
        aggregates: {
          byProvider: [
            {
              provider: 'anthropic',
              count: 2,
              totals: {
                input: 500,
                output: 1100,
                totalTokens: 1600,
                totalCost: 0.63,
                refresh: 'not-allowed',
              },
            },
            {
              provider: 'minimax',
              count: 1,
              totals: {
                input: 700,
                output: 2300,
                totalTokens: 3000,
                totalCost: 0.61,
                apiKey: 'not-allowed',
              },
            },
          ],
        },
      },
      usageStatusPayload: {
        providers: [
          {
            provider: 'anthropic',
            windows: [{ usedPercent: 42 }],
          },
        ],
      },
    })

    const serialized = JSON.stringify({ ok: true, usage })
    expect(serialized).not.toMatch(SENSITIVE_PATTERN)
  })

  it('buildCostSummary output contains no sensitive keywords', function test() {
    const cost = buildCostSummary({
      updatedAt: 1739011200000,
      totals: {
        totalCost: 14.82,
        inputCost: 5.1,
        outputCost: 9.72,
        password: 'not-allowed',
      },
      daily: [
        {
          date: '2026-02-06',
          totalCost: 6.2,
          input: 1200,
          output: 3400,
          token: 'not-allowed',
        },
        {
          date: '2026-02-07',
          totalCost: 8.62,
          input: 1500,
          output: 4800,
          refresh: 'not-allowed',
        },
      ],
    })

    const serialized = JSON.stringify({ ok: true, cost })
    expect(serialized).not.toMatch(SENSITIVE_PATTERN)
  })
})
