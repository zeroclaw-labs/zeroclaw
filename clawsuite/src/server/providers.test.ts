import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'

async function loadProvidersModule() {
  vi.resetModules()
  return import('./providers')
}

function writeConfig(homeDir: string, config: unknown) {
  const openclawDir = path.join(homeDir, '.openclaw')
  fs.mkdirSync(openclawDir, { recursive: true })
  fs.writeFileSync(
    path.join(openclawDir, 'openclaw.json'),
    JSON.stringify(config),
    'utf8',
  )
}

describe('providers config parsing', function suite() {
  afterEach(function cleanup() {
    vi.restoreAllMocks()
  })

  it('parses configured providers from auth profiles', async function test() {
    const homeDir = fs.mkdtempSync(path.join(os.tmpdir(), 'clawsuite-test-'))
    vi.spyOn(os, 'homedir').mockReturnValue(homeDir)

    writeConfig(homeDir, {
      auth: {
        profiles: {
          'anthropic:default': {},
          'openai-codex:default': {},
          'github-copilot:work': {},
        },
      },
    })

    const providers = await loadProvidersModule()
    expect(providers.getConfiguredProviderNames()).toEqual([
      'anthropic',
      'github-copilot',
      'openai-codex',
    ])
  })

  it('parses configured model ids from legacy models.providers schema', async function test() {
    const homeDir = fs.mkdtempSync(path.join(os.tmpdir(), 'clawsuite-test-'))
    vi.spyOn(os, 'homedir').mockReturnValue(homeDir)

    writeConfig(homeDir, {
      models: {
        providers: {
          anthropic: {
            models: [{ id: 'claude-opus-4-6' }, { id: 'claude-sonnet-4-5' }],
          },
          'openai-codex': {
            models: [{ id: 'gpt-5.2' }, { id: 'gpt-5.2-codex' }],
          },
        },
      },
    })

    const providers = await loadProvidersModule()
    const modelIds = providers.getConfiguredModelIds()
    expect(modelIds.has('claude-opus-4-6')).toBe(true)
    expect(modelIds.has('claude-sonnet-4-5')).toBe(true)
    expect(modelIds.has('gpt-5.2')).toBe(true)
    expect(modelIds.has('gpt-5.2-codex')).toBe(true)
  })

  it('parses configured model ids from agents.defaults schema', async function test() {
    const homeDir = fs.mkdtempSync(path.join(os.tmpdir(), 'clawsuite-test-'))
    vi.spyOn(os, 'homedir').mockReturnValue(homeDir)

    writeConfig(homeDir, {
      agents: {
        defaults: {
          model: {
            primary: 'anthropic/claude-opus-4-6',
            fallbacks: [
              'openai-codex/gpt-5.2',
              'openai-codex/gpt-5.2-codex',
              'github-copilot/gpt-5.2-codex',
            ],
          },
          models: {
            'openai-codex/gpt-5.2': {},
            'openai-codex/gpt-5.3-codex': {},
            'github-copilot/gpt-5.2': {},
            'anthropic/claude-opus-4-6': { alias: 'opus' },
          },
        },
      },
    })

    const providers = await loadProvidersModule()
    const modelIds = providers.getConfiguredModelIds()
    expect(modelIds.has('claude-opus-4-6')).toBe(true)
    expect(modelIds.has('gpt-5.2')).toBe(true)
    expect(modelIds.has('gpt-5.2-codex')).toBe(true)
    expect(modelIds.has('gpt-5.3-codex')).toBe(true)
  })
})
