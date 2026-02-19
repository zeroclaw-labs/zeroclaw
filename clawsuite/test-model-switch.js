#!/usr/bin/env node
/**
 * Test script for model switcher
 * Tests: GET /api/models + POST /api/model-switch across providers
 */

const BASE_URL = process.env.TEST_URL || 'http://localhost:3000'

async function fetchModels() {
  const res = await fetch(`${BASE_URL}/api/models`)
  if (!res.ok) throw new Error(`GET /api/models failed: ${res.status}`)
  return res.json()
}

async function switchModel(model, sessionKey = 'main') {
  const res = await fetch(`${BASE_URL}/api/model-switch`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ model, sessionKey }),
  })
  const data = await res.json()
  return { ok: res.ok, status: res.status, data }
}

async function main() {
  console.log('🧪 Model Switcher Test\n')

  // 1. Fetch models
  console.log('1️⃣  Fetching models...')
  const modelsResponse = await fetchModels()

  if (!modelsResponse.ok) {
    console.error('❌ /api/models returned ok=false')
    console.error(modelsResponse)
    process.exit(1)
  }

  const models = modelsResponse.models || []
  const providers = modelsResponse.configuredProviders || []

  console.log(
    `✅ Got ${models.length} models across ${providers.length} providers`,
  )
  console.log(`   Configured providers: ${providers.join(', ')}\n`)

  // 2. Group by provider
  const byProvider = {}
  for (const model of models) {
    const p = model.provider || 'unknown'
    if (!byProvider[p]) byProvider[p] = []
    byProvider[p].push(model)
  }

  // 3. Test one model per provider
  console.log('2️⃣  Testing model switch (one per provider)...\n')

  const results = []
  for (const provider of providers) {
    const providerModels = byProvider[provider] || []
    if (providerModels.length === 0) {
      console.log(`⚠️  ${provider}: no models found`)
      continue
    }

    const testModel = providerModels[0]
    const modelId = testModel.id
    const expectedValue = `${provider}/${modelId}`

    console.log(`   Testing: ${provider} → ${testModel.name}`)
    console.log(`      Model ID: ${modelId}`)
    console.log(`      Expected value sent: ${expectedValue}`)

    try {
      const result = await switchModel(expectedValue)

      if (result.ok && result.data.ok !== false) {
        console.log(`   ✅ Success`)
        const resolved = result.data.resolved || {}
        console.log(
          `      Resolved: ${resolved.modelProvider || '?'}/${resolved.model || '?'}`,
        )
        results.push({ provider, model: testModel.name, status: 'success' })
      } else {
        console.log(`   ❌ Failed: ${result.data.error || 'Unknown error'}`)
        results.push({
          provider,
          model: testModel.name,
          status: 'error',
          error: result.data.error,
        })
      }
    } catch (err) {
      console.log(`   ❌ Exception: ${err.message}`)
      results.push({
        provider,
        model: testModel.name,
        status: 'exception',
        error: err.message,
      })
    }
    console.log('')
  }

  // 4. Summary
  console.log('📊 Summary:\n')
  const successes = results.filter((r) => r.status === 'success').length
  const failures = results.filter((r) => r.status !== 'success').length

  console.log(`   ✅ Successes: ${successes}`)
  console.log(`   ❌ Failures: ${failures}`)

  if (failures > 0) {
    console.log('\n   Failed tests:')
    results
      .filter((r) => r.status !== 'success')
      .forEach((r) => console.log(`      - ${r.provider}: ${r.error}`))
  }

  console.log('')
  process.exit(failures > 0 ? 1 : 0)
}

main().catch((err) => {
  console.error('💥 Fatal error:', err)
  process.exit(1)
})
