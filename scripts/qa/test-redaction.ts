/**
 * Test script for diagnostics bundle redaction
 * Phase 2.4-001: Verify no sensitive patterns leak through
 *
 * Run: npx tsx scripts/qa/test-redaction.ts
 */

import {
  redactSensitiveData,
  redactObject,
  extractFolderName,
} from '../../src/lib/diagnostics'

const SENSITIVE_TEST_CASES: Array<{
  input: string
  shouldRedact: boolean
  description: string
}> = [
  // OpenAI keys
  {
    input: 'sk-abc123def456ghi789jkl012mno345pqr678',
    shouldRedact: true,
    description: 'OpenAI API key',
  },
  {
    input: 'Bearer sk-ant-api03-abc123',
    shouldRedact: true,
    description: 'Anthropic API key with Bearer',
  },

  // GitHub tokens
  {
    input: 'ghp_abc123def456ghi789jkl012mno345pqr678stu',
    shouldRedact: true,
    description: 'GitHub PAT',
  },
  {
    input: 'github_pat_11ABC123_xyz789',
    shouldRedact: true,
    description: 'GitHub fine-grained PAT',
  },

  // Generic patterns
  {
    input: 'token=abc123def456',
    shouldRedact: true,
    description: 'Generic token',
  },
  {
    input: 'secret: mysecretvalue123',
    shouldRedact: true,
    description: 'Generic secret',
  },
  {
    input: 'password=hunter2verysecure',
    shouldRedact: true,
    description: 'Password',
  },
  {
    input: 'api_key="sk-proj-12345"',
    shouldRedact: true,
    description: 'API key in quotes',
  },
  {
    input: 'Authorization: Bearer xyz.abc.123',
    shouldRedact: true,
    description: 'Auth header',
  },

  // Paths
  {
    input: '/Users/eric/secret/project',
    shouldRedact: true,
    description: 'macOS user path',
  },
  {
    input: '/home/eric/.config/tokens',
    shouldRedact: true,
    description: 'Linux user path',
  },
  {
    input: 'C:\\Users\\Eric\\Documents\\secrets',
    shouldRedact: true,
    description: 'Windows user path',
  },

  // Safe values
  {
    input: 'Gateway connected',
    shouldRedact: false,
    description: 'Normal status',
  },
  {
    input: 'Session started',
    shouldRedact: false,
    description: 'Normal event',
  },
  {
    input: 'Error: connection refused',
    shouldRedact: false,
    description: 'Error message',
  },
  {
    input: 'ws://127.0.0.1:18789',
    shouldRedact: false,
    description: 'Local URL',
  },
]

let passed = 0
let failed = 0

console.log('🔒 Testing diagnostics redaction...\n')

for (const testCase of SENSITIVE_TEST_CASES) {
  const result = redactSensitiveData(testCase.input)
  const wasRedacted = result.includes('[REDACTED]')

  if (wasRedacted === testCase.shouldRedact) {
    console.log(`✅ ${testCase.description}`)
    passed++
  } else {
    console.log(`❌ ${testCase.description}`)
    console.log(`   Input: ${testCase.input}`)
    console.log(`   Output: ${result}`)
    console.log(`   Expected redaction: ${testCase.shouldRedact}`)
    failed++
  }
}

// Test object redaction
console.log('\n🔒 Testing object redaction...\n')

const testObject = {
  status: 'connected',
  apiKey: 'sk-abc123def456',
  token: 'secret123',
  gatewayUrl: 'ws://127.0.0.1:18789',
  nested: {
    secretKey: 'should-be-redacted',
    normalValue: 'keep-this',
  },
}

const redactedObject = redactObject(testObject)

if (redactedObject.apiKey === '[REDACTED]') {
  console.log('✅ apiKey field redacted')
  passed++
} else {
  console.log('❌ apiKey field NOT redacted')
  failed++
}

if (redactedObject.token === '[REDACTED]') {
  console.log('✅ token field redacted')
  passed++
} else {
  console.log('❌ token field NOT redacted')
  failed++
}

if (
  (redactedObject.nested as Record<string, unknown>).secretKey === '[REDACTED]'
) {
  console.log('✅ nested secretKey field redacted')
  passed++
} else {
  console.log('❌ nested secretKey field NOT redacted')
  failed++
}

if (redactedObject.gatewayUrl === 'ws://127.0.0.1:18789') {
  console.log('✅ gatewayUrl preserved (no secrets)')
  passed++
} else {
  console.log('❌ gatewayUrl incorrectly modified')
  failed++
}

// Test folder name extraction
console.log('\n🔒 Testing folder name extraction...\n')

const pathTests = [
  { input: '/Users/eric/projects/clawsuite', expected: 'clawsuite' },
  { input: 'C:\\Users\\Eric\\Desktop\\project', expected: 'project' },
  { input: null, expected: 'Not set' },
  { input: '', expected: 'Not set' },
]

for (const test of pathTests) {
  const result = extractFolderName(test.input)
  if (result === test.expected) {
    console.log(`✅ extractFolderName("${test.input}") = "${result}"`)
    passed++
  } else {
    console.log(
      `❌ extractFolderName("${test.input}") = "${result}", expected "${test.expected}"`,
    )
    failed++
  }
}

console.log('\n' + '='.repeat(50))
console.log(`Results: ${passed} passed, ${failed} failed`)
console.log('='.repeat(50))

if (failed > 0) {
  process.exit(1)
}

console.log('\n✅ All redaction tests passed!')
