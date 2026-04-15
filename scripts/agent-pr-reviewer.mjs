/**
 * ╔══════════════════════════════════════════════════════════════════════╗
 * ║  AGENT PR REVIEWER — ZeroClaw Contribution Review                  ║
 * ║                                                                    ║
 * ║  Reviews pull requests using parallel AI agents for security,      ║
 * ║  correctness, and test coverage, then produces a unified summary.  ║
 * ╚══════════════════════════════════════════════════════════════════════╝
 *
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-FileCopyrightText: Copyright (c) 2026 Jason Perlow. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * USAGE:
 *   node scripts/agent-pr-reviewer.mjs --branch fix/5155-delegate-prompt-injection-mode
 *   node scripts/agent-pr-reviewer.mjs --pr 42
 *   node scripts/agent-pr-reviewer.mjs --branch my-feature --issue-file /tmp/issue-body.md
 *
 * REQUIREMENTS:
 *   - Node.js 18+
 *   - @anthropic-ai/claude-agent-sdk (auto-installed if missing)
 *   - NVIDIA_INFERENCE_KEY environment variable
 *   - Git repository with an 'upstream' remote pointing to NVIDIA/NemoClaw
 *     (or set UPSTREAM_REMOTE env var to override)
 *
 * WHAT IT DOES:
 *   1. Gathers diff, changed file stats, and commit log for the branch
 *   2. Runs three review agents in parallel:
 *      - SecurityReview:   sandbox escapes, credential handling, tool denylist
 *      - CorrectnessReview: bugs, logic errors, race conditions, type issues
 *      - TestCoverage:     missing tests, coverage gaps, recommended test cases
 *   3. Runs a sequential summary agent that reads the three review files
 *      and produces a unified pr-review-summary.md with verdict + top findings
 *
 * OUTPUT:
 *   pr-review-security.md     — security findings
 *   pr-review-correctness.md  — correctness findings
 *   pr-review-tests.md        — test coverage analysis
 *   pr-review-summary.md      — unified verdict and top findings
 */

import { writeFileSync, readFileSync, existsSync, appendFileSync, mkdirSync, unlinkSync, rmSync } from 'fs';
import { join, dirname, resolve } from 'path';
import { fileURLToPath } from 'url';
import { platform, homedir } from 'os';
import { execSync as _execSyncRaw } from 'child_process';

// ─── SETUP (copied from agent-sdk-template.mjs) ─────────────────────

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(process.cwd());
const DEBUG_LOG = join(ROOT, 'agent-sdk-activity.log');
const IS_WINDOWS = platform() === 'win32';
const IS_MAC = platform() === 'darwin';

if (!process.env.ANTHROPIC_API_KEY && process.env.NVIDIA_INFERENCE_KEY)
  process.env.ANTHROPIC_API_KEY = process.env.NVIDIA_INFERENCE_KEY;
if (!process.env.ANTHROPIC_BASE_URL)
  process.env.ANTHROPIC_BASE_URL = 'https://inference-api.nvidia.com';

process.env.DEBUG_CLAUDE_AGENT_SDK = '1';
if (!process.env.ANTHROPIC_TIMEOUT) process.env.ANTHROPIC_TIMEOUT = '600000';

// ─── TCP KEEPALIVE (automatic) ──────────────────────────────────────

try {
  if (IS_MAC) {
    _execSyncRaw('sysctl -w net.inet.tcp.keepidle=30 net.inet.tcp.keepintvl=10 net.inet.tcp.keepcnt=5 2>/dev/null', { stdio: 'ignore' });
  } else if (platform() !== 'win32') {
    _execSyncRaw('sysctl -w net.ipv4.tcp_keepalive_time=30 net.ipv4.tcp_keepalive_intvl=10 net.ipv4.tcp_keepalive_probes=5 2>/dev/null', { stdio: 'ignore' });
  }
} catch { /* needs sudo — warn but continue */ }

function _resetKeepalive() {
  try {
    if (IS_MAC) {
      _execSyncRaw('sysctl -w net.inet.tcp.keepidle=7200 net.inet.tcp.keepintvl=75 net.inet.tcp.keepcnt=8 2>/dev/null', { stdio: 'ignore' });
    } else if (platform() !== 'win32') {
      _execSyncRaw('sysctl -w net.ipv4.tcp_keepalive_time=7200 net.ipv4.tcp_keepalive_intvl=75 net.ipv4.tcp_keepalive_probes=8 2>/dev/null', { stdio: 'ignore' });
    }
  } catch { /* best effort */ }
}

if (!process.env.ANTHROPIC_API_KEY) {
  console.error('');
  console.error('╔══════════════════════════════════════════════════════════════╗');
  console.error('║  NVIDIA_INFERENCE_KEY not found in your environment.        ║');
  console.error('╚══════════════════════════════════════════════════════════════╝');
  console.error('');
  console.error('1. Get your key: https://inference.nvidia.com/key-management');
  console.error('');
  console.error('2. Add it to your environment (do NOT paste keys in code or chat):');
  console.error('');
  if (IS_WINDOWS) {
    console.error('   Windows (PowerShell — persists across sessions):');
    console.error('     [Environment]::SetEnvironmentVariable("NVIDIA_INFERENCE_KEY", "your-key-here", "User")');
  } else if (IS_MAC) {
    console.error('   macOS (add to ~/.zshrc — persists across sessions):');
    console.error('     echo \'export NVIDIA_INFERENCE_KEY="your-key-here"\' >> ~/.zshrc');
    console.error('     source ~/.zshrc');
  } else {
    console.error('   Linux (add to ~/.bashrc — persists across sessions):');
    console.error('     echo \'export NVIDIA_INFERENCE_KEY="your-key-here"\' >> ~/.bashrc');
    console.error('     source ~/.bashrc');
  }
  console.error('');
  console.error('3. Restart your terminal, then run this script again.');
  console.error('');
  process.exit(1);
}

const PRIMARY_MODEL = process.env.AUDIT_MODEL || 'azure/anthropic/claude-sonnet-4-6';
const FALLBACK_MODEL = PRIMARY_MODEL.replace('azure/', 'aws/');
let MODEL = PRIMARY_MODEL;

// Auto-install SDK if missing
try {
  await import('@anthropic-ai/claude-agent-sdk');
} catch {
  console.log('Installing @anthropic-ai/claude-agent-sdk...');
  _execSyncRaw('npm install @anthropic-ai/claude-agent-sdk', { stdio: 'inherit' });
}
const { query } = await import('@anthropic-ai/claude-agent-sdk');

// SDK version check
const TESTED_SDK_RANGE = { min: '0.2.90', max: '0.3.99' };
try {
  const sdkPkg = JSON.parse(readFileSync(join(ROOT, 'node_modules/@anthropic-ai/claude-agent-sdk/package.json'), 'utf-8'));
  const ver = sdkPkg.version;
  if (ver < TESTED_SDK_RANGE.min || ver > TESTED_SDK_RANGE.max)
    _log(`SDK VERSION WARNING: installed ${ver}, tested ${TESTED_SDK_RANGE.min}--${TESTED_SDK_RANGE.max}. Behavior may differ.`);
  else _log(`SDK version: ${ver} (within tested range)`);
} catch { _log('SDK version: could not determine'); }

process.on('uncaughtException', (e) => { _log(`FATAL UNCAUGHT: ${e.message}\n${e.stack}`); process.exit(99); });
process.on('unhandledRejection', (e) => { _log(`FATAL UNHANDLED: ${e?.message || e}`); process.exit(98); });
process.on('exit', (code) => { _resetKeepalive(); _log(`PROCESS EXIT code=${code}`); });

function _log(msg) {
  const line = `[${new Date().toISOString()}] ${msg}`;
  console.log(line);
  try { appendFileSync(DEBUG_LOG, line + '\n'); } catch { /* best effort */ }
}

// ─── ALLOWED TOOLS ──────────────────────────────────────────────────

const ALLOWED_TOOLS = ['Read', 'Write', 'Edit', 'Glob', 'Grep'];

// ─── DATA BROKERING ─────────────────────────────────────────────────

const PROMPT_TOKEN_BUDGET = 8000;
const DATA_INLINE_MAX = 4000;
const TEMP_DIR = join(ROOT, '.tmp-agent-data');

function estimateTokens(text) { return Math.ceil(text.length / 4); }

async function prepareData(label, dataSpec) {
  if (!dataSpec || typeof dataSpec !== 'object') return { block: '', tempFiles: [] };
  const tempFiles = [], blocks = [];
  for (const [name, fetchFn] of Object.entries(dataSpec)) {
    if (typeof fetchFn !== 'function') continue;
    let raw;
    try { raw = fetchFn(); if (typeof raw !== 'string') raw = JSON.stringify(raw, null, 2); }
    catch (err) { _log(`[${label}] DATA "${name}" fetch failed: ${err.message}`); blocks.push(`=== DATA: ${name} (FETCH FAILED: ${err.message}) ===\n`); continue; }
    const tokens = estimateTokens(raw);
    _log(`[${label}] DATA "${name}": ${raw.split('\n').length} lines, ~${tokens} tokens`);
    if (tokens <= DATA_INLINE_MAX) {
      blocks.push(`=== DATA: ${name} (${tokens} tokens, inline) ===\n${raw}\n=== END ${name} ===\n`);
    } else {
      mkdirSync(TEMP_DIR, { recursive: true });
      const tempPath = join(TEMP_DIR, `${label}-${name}-${Date.now()}.txt`);
      writeFileSync(tempPath, raw); tempFiles.push(tempPath);
      _log(`[${label}] DATA "${name}": too large for inline (${tokens} tokens) -- summarizing`);
      let summary;
      try {
        const summaryPrompt = `Summarize this data concisely for another AI agent. Keep key details (IDs, names, statuses, counts). Under 80 lines.\n\n${raw.slice(0, 30000)}`;
        let summaryResult = '';
        for await (const msg of query({ prompt: summaryPrompt, options: { model: MODEL, allowedTools: [], maxTurns: 1, settingSources: [] } }))
          if ('result' in msg) summaryResult = msg.result || '';
        summary = summaryResult || raw.slice(0, DATA_INLINE_MAX * 4);
        _log(`[${label}] DATA "${name}": summarized to ~${estimateTokens(summary)} tokens`);
      } catch (err) {
        _log(`[${label}] DATA "${name}": summarizer failed (${err.message}) -- using truncation`);
        summary = raw.slice(0, DATA_INLINE_MAX * 4) + '\n... (truncated)';
      }
      blocks.push(`=== DATA: ${name} (summarized -- full data at ${tempPath}, use Grep for details) ===\n${summary}\n=== END ${name} ===\n`);
    }
  }
  return { block: blocks.join('\n'), tempFiles };
}

function cleanupTempFiles(tempFiles) {
  for (const f of tempFiles) { try { unlinkSync(f); } catch {} }
  try { rmSync(TEMP_DIR, { recursive: true, force: true }); } catch {}
}

// ─── AGENT RUNNER ───────────────────────────────────────────────────

const VERIFY_FOOTER = `

IMPORTANT RULES FOR THIS AGENT:
- Use ONLY these tools: Read, Write, Edit, Glob, Grep. NO Bash. NO Agent sub-tasks.
- All data you need is already in your prompt above. Do NOT try to fetch more.
- Read whole files. Do NOT chunk into small pieces.
- Write incrementally if producing large output -- one section at a time.

MANDATORY SELF-VERIFICATION AND SELF-CORRECTION:

After EVERY file write or edit:
1. Read the file back immediately.
2. Confirm your change is present and correct.
3. If the change is NOT present or is wrong: fix it now, then re-read to verify again.
4. Repeat until the file is correct. Do not move on until it is.

Before returning your final result you MUST complete this checklist:
1. List every file you were asked to create or modify.
2. For each file: read it and confirm it matches the task requirements.
3. If ANY file is missing, incomplete, or wrong: fix it now.
4. Return a verification table as the LAST thing in your response:

| File | Status | Check |
|------|--------|-------|
| path/to/file | VERIFIED or FIXED | what was confirmed |

You may NOT return your result until every file shows VERIFIED or FIXED.
If you cannot fix a problem, report it as FAILED with the reason -- but attempt the fix first.`;

const INITIAL_RESPONSE_TIMEOUT_MS = 30_000;
const SILENCE_TIMEOUT_MS = 423_000;
const MAX_RETRIES = 2;
const API_RETRY_DELAY_MS = 10_000;

async function runAgent(label, prompt, options = {}) {
  const maxTurns = options.maxTurns || 50;
  const retries = options.retries ?? MAX_RETRIES;
  const attempt = options._attempt || 1;
  const agentModel = options._model || options.model || MODEL;
  const start = Date.now();
  let toolCalls = 0, resultText = '', errorMsg = null, lastActivity = Date.now();
  let silenceKilled = false, streamStarted = false, failureType = null;
  const filesWritten = [], writeErrors = [];

  const dataResult = await prepareData(label, options.data);
  const dataBlock = dataResult.block ? '\n' + dataResult.block + '\n' : '';
  const totalTokens = estimateTokens(prompt) + estimateTokens(dataBlock) + estimateTokens(VERIFY_FOOTER);
  _log(`[${label}] PROMPT: ~${totalTokens} tokens sending to proxy`);

  const fullPrompt = dataBlock + prompt + VERIFY_FOOTER;
  const elapsed = () => Math.round((Date.now() - start) / 1000);
  _log(`[${label}] STARTING model=${agentModel} maxTurns=${maxTurns}${attempt > 1 ? ` (retry ${attempt})` : ''}`);

  const memWatch = setInterval(() => {
    const rss = Math.round(process.memoryUsage().rss / 1024 / 1024);
    if (rss > 500) _log(`[${label}] MEMORY: parent RSS ${rss}MB`);
  }, 30000);

  const heartbeat = setInterval(() => {
    const silence = Date.now() - lastActivity, silenceSec = Math.round(silence / 1000);
    const rss = Math.round(process.memoryUsage().rss / 1024 / 1024);
    _log(`[${label}] heartbeat ${elapsed()}s | ${toolCalls} tools | silent ${silenceSec}s | RSS ${rss}MB`);
    const timeout = !streamStarted ? INITIAL_RESPONSE_TIMEOUT_MS : (options.silenceTimeoutMs || SILENCE_TIMEOUT_MS);
    if (silence > timeout) {
      silenceKilled = true;
      failureType = !streamStarted ? 'api_no_response' : (toolCalls === 0 ? 'model_thinking_timeout' : 'mid_work_silence');
      const label2 = failureType === 'api_no_response' ? 'API NO RESPONSE' : failureType === 'model_thinking_timeout' ? 'MODEL THINKING TIMEOUT' : 'SILENCE TIMEOUT';
      _log(`[${label}] ${label2} -- ${silenceSec}s, ${toolCalls} tools, stream=${streamStarted}. Killing.`);
      clearInterval(heartbeat);
    }
  }, 15000);

  try {
    for await (const message of query({ prompt: fullPrompt, options: {
      model: agentModel, allowedTools: options.tools || ALLOWED_TOOLS, maxTurns, settingSources: [],
      stderr: (line) => {
        const trimmed = line.trim();
        if (!trimmed) return;
        _log(`[${label}] CHILD: ${trimmed}`);
        if (trimmed.includes('Stream started')) { lastActivity = Date.now(); streamStarted = true; }
      },
    }})) {
      if (silenceKilled) break;
      lastActivity = Date.now();
      if (message.type === 'assistant') {
        for (const block of message.message?.content ?? []) {
          if (block.type === 'tool_use') {
            toolCalls++;
            const input = JSON.stringify(block.input).slice(0, 100);
            _log(`[${label}] ${elapsed()}s | TOOL-${toolCalls}: ${block.name}(${input})`);
            if (block.name === 'Write' && block.input?.file_path) filesWritten.push(block.input.file_path);
          } else if (block.type === 'tool_result' && block.is_error) {
            const errText = typeof block.content === 'string' ? block.content : JSON.stringify(block.content);
            if (errText.includes('EPERM') || errText.includes('permission')) {
              writeErrors.push(errText.slice(0, 200));
              _log(`[${label}] ${elapsed()}s | WRITE ERROR: ${errText.slice(0, 150)}`);
            }
          } else if (block.type === 'text' && block.text) {
            _log(`[${label}] ${elapsed()}s | TEXT: ${block.text.slice(0, 150)}${block.text.length > 150 ? '...' : ''}`);
          }
        }
      } else if (message.type === 'system' && message.subtype === 'init') {
        _log(`[${label}] ${elapsed()}s | INIT: session=${message.session_id}`);
      } else if ('result' in message) {
        resultText = message.result || '';
        _log(`[${label}] ${elapsed()}s | RESULT: ${resultText.slice(0, 200)}${resultText.length > 200 ? '...' : ''}`);
      }
    }
  } catch (err) {
    errorMsg = err.message || String(err);
    if (errorMsg.includes('maximum number of turns')) failureType = 'max_turns';
    else if (errorMsg.includes('SIGKILL')) failureType = 'oom_killed';
    else if (errorMsg.includes('exited with code')) failureType = 'child_crash';
    else if (errorMsg.includes('terminated by signal')) failureType = 'child_signal';
    else failureType = 'unknown_error';
    _log(`[${label}] ${elapsed()}s | CATCH (${failureType}): ${errorMsg}`);
  } finally { clearInterval(heartbeat); clearInterval(memWatch); }

  if (silenceKilled && !failureType) failureType = !streamStarted ? 'api_no_response' : 'mid_work_silence';
  if (silenceKilled) errorMsg = `${failureType}: silent for ${Math.round((Date.now() - lastActivity) / 1000)}s with ${toolCalls} tools`;

  const canRetry = attempt <= retries && (silenceKilled || failureType === 'max_turns');
  if (canRetry) {
    _log(`[${label}] AUTO-RETRY (${failureType}) -- attempt ${attempt + 1}/${retries + 1}`);
    _log(`[${label}]   Files written so far: ${filesWritten.length ? filesWritten.join(', ') : 'none'}`);
    let retryOpts = { ...options, _attempt: attempt + 1 };
    if (failureType === 'api_no_response') {
      _log(`[${label}]   Waiting ${API_RETRY_DELAY_MS / 1000}s before retry...`);
      await new Promise(r => setTimeout(r, API_RETRY_DELAY_MS));
      if (attempt >= 2 && agentModel === PRIMARY_MODEL) {
        _log(`[${label}]   Switching to fallback provider: ${FALLBACK_MODEL}`);
        retryOpts._model = FALLBACK_MODEL;
      }
    }
    if (failureType === 'max_turns') retryOpts.maxTurns = Math.min(maxTurns + 10, 60);
    const retryHint = filesWritten.length
      ? `\n\nRETRY: Previous attempt wrote: ${filesWritten.join(', ')}. Read them, focus on what's NOT done. Smaller steps.`
      : `\n\nRETRY: Previous attempt failed (${failureType}). Work in smaller steps.`;
    return runAgent(label, prompt + retryHint, retryOpts);
  }

  const status = (errorMsg || silenceKilled) ? 'FAILED' : 'OK';
  const verdict = status === 'OK'
    ? `ENDED: completed normally | ${toolCalls} tools | ${elapsed()}s`
    : `ENDED: ${failureType || 'unknown'} | ${toolCalls} tools | ${elapsed()}s | ${errorMsg?.slice(0, 100)}`;
  _log(`[${label}] ${verdict}`);

  const cleaned = dataResult.tempFiles.length;
  cleanupTempFiles(dataResult.tempFiles);
  if (cleaned) _log(`[${label}] CLEANUP: removed ${cleaned} temp file(s)`);
  else _log(`[${label}] CLEANUP: no temp files`);

  const agentResult = { label, status, elapsed: elapsed(), toolCalls, error: errorMsg, result: resultText, filesWritten, failureType, verdict, writeErrors };
  _allResults.push(agentResult);
  return agentResult;
}

// ─── PARALLEL + SEQUENTIAL RUNNERS ─────────────────────────────────

async function runParallel(agents) {
  _log(`Running ${agents.length} agents in parallel...`);
  const results = await Promise.all(agents.map(a => runAgent(a.label, a.prompt, { ...a.options, data: a.data })));
  _log('Parallel batch complete:');
  for (const r of results) _log(`  ${r.label.padEnd(25)} ${r.status.padEnd(8)} ${(r.elapsed + 's').padStart(8)} ${String(r.toolCalls).padStart(6)} tools`);
  _log(`${results.filter(r => r.status === 'OK').length}/${results.length} succeeded`);
  return results;
}

async function runSequential(agents) {
  _log(`Running ${agents.length} agents sequentially...`);
  const results = [];
  for (const a of agents) { results.push(await runAgent(a.label, a.prompt, { ...a.options, data: a.data })); }
  return results;
}

// ─── RUN SUMMARY + PROGRESS ────────────────────────────────────────

const ERROR_ADVICE = {
  api_no_response: 'Proxy did not respond after 2 retries (including backup provider). This is a proxy outage -- not a code or prompt problem. Check proxy status on #nv-inference Slack, then re-run when back.',
  model_thinking_timeout: 'The API responded but the model thought longer than the silence timeout before acting. The default timeout (423s) already exceeds the proxy stream limit. If you hit this, the task may need to be split into smaller agents (Rule 10).',
  mid_work_silence: 'Model thinking too long mid-work, proxy killed the stream. Split into smaller agents (max 5-6 files each).',
  max_turns: 'Ran out of conversation turns. Increase maxTurns or simplify the task.',
  oom_killed: 'Out of memory (SIGKILL). Reduce the number of large files the agent reads.',
  child_crash: 'SDK child process crashed. Check CHILD stderr lines in logs for the specific error.',
  child_signal: 'Child process terminated by signal. Check signal name in logs.',
  unknown_error: 'Unexpected error. Check the CATCH line in logs for the full message.',
};

const _allResults = [];
const _startTime = Date.now();

function writeRunSummary() {
  if (_allResults.length === 0) return;
  const summaryPath = join(ROOT, 'agent-sdk-last-run.md');
  const ok = _allResults.filter(r => r.status === 'OK').length;
  const failed = _allResults.filter(r => r.status === 'FAILED');
  const lines = ['# Agent SDK -- Last Run Summary', '', `> Generated: ${new Date().toISOString()}`, `> Agents: ${_allResults.length} total, ${ok} succeeded, ${failed.length} failed`, '', '## Results', '', '| Agent | Status | Time | Tools | Failure | Advice |', '|-------|--------|------|-------|---------|--------|'];
  for (const r of _allResults) {
    const advice = r.failureType ? (ERROR_ADVICE[r.failureType] || 'Check logs.').slice(0, 80) : '--';
    lines.push(`| ${r.label} | ${r.status} | ${r.elapsed}s | ${r.toolCalls} | ${r.failureType || '--'} | ${r.status === 'OK' ? '--' : advice} |`);
  }
  if (failed.length) {
    lines.push('', '## Failed Agents -- What To Fix', '');
    for (const r of failed) {
      lines.push(`### ${r.label}`, `- **Failure type:** ${r.failureType}`, `- **Verdict:** ${r.verdict}`, `- **Advice:** ${ERROR_ADVICE[r.failureType] || 'Check agent-sdk-activity.log'}`, '');
      if (r.writeErrors?.length) lines.push(`- **Write errors:** ${r.writeErrors.join('; ')}`);
      if (r.filesWritten?.length) lines.push(`- **Partial output:** ${r.filesWritten.join(', ')}`);
    }
  }
  lines.push('', '*Full logs: agent-sdk-activity.log*');
  try { writeFileSync(summaryPath, lines.join('\n')); _log(`RUN SUMMARY: ${summaryPath}`); } catch (err) { _log(`RUN SUMMARY write failed: ${err.message}`); }
}

const PROGRESS_PATH = join(ROOT, 'agent-sdk-progress.md');
const _progressTimer = setInterval(() => {
  const ok = _allResults.filter(r => r.status === 'OK').length;
  const failed = _allResults.filter(r => r.status === 'FAILED').length;
  const running = _allResults.length === 0 ? 'starting...' : `${ok} done, ${failed} failed`;
  const rss = Math.round(process.memoryUsage().rss / 1024 / 1024);
  const uptime = Math.round((Date.now() - _startTime) / 1000);
  const lines = ['# Agent SDK -- In Progress', '', `> Updated: ${new Date().toISOString()}`, `> Running for: ${uptime}s | Status: ${running} | Memory: ${rss}MB`, '', '## Completed So Far', ''];
  for (const r of _allResults) lines.push(`- **${r.label}**: ${r.status} (${r.elapsed}s, ${r.toolCalls} tools)${r.failureType ? ` -- ${r.failureType}` : ''}`);
  if (_allResults.length === 0) lines.push('- _(waiting for first agent to finish)_');
  try { writeFileSync(PROGRESS_PATH, lines.join('\n')); } catch {}
}, 120_000);

process.on('exit', () => { writeRunSummary(); clearInterval(_progressTimer); try { unlinkSync(PROGRESS_PATH); } catch {} });

// ═══════════════════════════════════════════════════════════════════════
// CLI ARGUMENT PARSING
// ═══════════════════════════════════════════════════════════════════════

function parseArgs() {
  const args = process.argv.slice(2);
  const parsed = { branch: null, pr: null, issueFile: null };
  for (let i = 0; i < args.length; i++) {
    if ((args[i] === '--branch' || args[i] === '-b') && args[i + 1]) {
      parsed.branch = args[++i];
    } else if ((args[i] === '--pr' || args[i] === '-p') && args[i + 1]) {
      parsed.pr = args[++i];
    } else if (args[i] === '--issue-file' && args[i + 1]) {
      parsed.issueFile = args[++i];
    } else if (args[i] === '--help' || args[i] === '-h') {
      console.log(`
Usage: node scripts/agent-pr-reviewer.mjs [options]

Options:
  --branch, -b <name>    Branch name to review (compared against upstream/main)
  --pr, -p <number>      PR number (used for labeling; optionally provide --issue-file)
  --issue-file <path>    Path to a file containing the issue/PR body text
  --help, -h             Show this help message

Environment:
  NVIDIA_INFERENCE_KEY   Required. Your NVIDIA inference proxy API key.
  UPSTREAM_REMOTE        Git remote name for the upstream repo (default: "upstream")
  AUDIT_MODEL            Override the model (default: azure/anthropic/claude-sonnet-4-6)

Examples:
  node scripts/agent-pr-reviewer.mjs --branch fix/5155-delegate-prompt-injection-mode
  node scripts/agent-pr-reviewer.mjs --pr 42 --branch fix/some-feature
  node scripts/agent-pr-reviewer.mjs --branch my-feature --issue-file /tmp/issue-body.md
`);
      process.exit(0);
    }
  }
  if (!parsed.branch && !parsed.pr) {
    console.error('Error: --branch or --pr is required. Run with --help for usage.');
    process.exit(1);
  }
  return parsed;
}

// ═══════════════════════════════════════════════════════════════════════
// DATA GATHERING (runs BEFORE agents start — Rule 14)
// ═══════════════════════════════════════════════════════════════════════

function execGit(cmd) {
  try {
    return _execSyncRaw(cmd, { encoding: 'utf-8', cwd: ROOT, maxBuffer: 10 * 1024 * 1024 }).trim();
  } catch (err) {
    _log(`GIT COMMAND FAILED: ${cmd}\n  ${err.message}`);
    return '';
  }
}

function gatherData(cliArgs) {
  const upstream = process.env.UPSTREAM_REMOTE || 'upstream';
  const branch = cliArgs.branch;

  // Ensure upstream/main is fetched
  _log(`Fetching ${upstream}/main...`);
  execGit(`git fetch ${upstream} main 2>/dev/null`);

  // If a branch is specified, make sure we have it locally
  if (branch) {
    const localBranches = execGit('git branch --list');
    if (!localBranches.includes(branch)) {
      _log(`Branch "${branch}" not found locally, trying to fetch...`);
      execGit(`git fetch ${upstream} ${branch} 2>/dev/null`);
      execGit(`git fetch origin ${branch} 2>/dev/null`);
    }
  }

  const ref = branch || 'HEAD';
  const mergeBase = execGit(`git merge-base ${upstream}/main ${ref}`);
  if (!mergeBase) {
    console.error(`Error: Could not find merge base between ${upstream}/main and ${ref}.`);
    console.error('Make sure the branch exists and the upstream remote is configured.');
    process.exit(1);
  }

  _log(`Merge base: ${mergeBase.slice(0, 12)}`);
  _log(`Comparing: ${upstream}/main...${ref}`);

  const diff = execGit(`git diff ${mergeBase}..${ref}`);
  const changedFiles = execGit(`git diff --stat ${mergeBase}..${ref}`);
  const commitMessages = execGit(`git log --format="%h %s%n  Author: %an <%ae>%n  Date: %ai%n" ${mergeBase}..${ref}`);

  let issueBody = '';
  if (cliArgs.issueFile && existsSync(cliArgs.issueFile)) {
    issueBody = readFileSync(cliArgs.issueFile, 'utf-8');
    _log(`Loaded issue body from ${cliArgs.issueFile} (${issueBody.length} chars)`);
  }

  _log(`Data gathered: diff=${diff.length} chars, files=${changedFiles.split('\n').length} lines, commits=${commitMessages.split('\n').length} lines`);

  return { diff, changedFiles, commitMessages, issueBody, ref, upstream, mergeBase };
}

// ═══════════════════════════════════════════════════════════════════════
// AGENT DEFINITIONS
// ═══════════════════════════════════════════════════════════════════════

function buildAgents(data, cliArgs) {
  const outputDir = ROOT;
  const branchLabel = cliArgs.branch || `PR #${cliArgs.pr}`;

  const issueContext = data.issueBody
    ? `\n\nISSUE/PR DESCRIPTION:\n${data.issueBody}\n`
    : '';

  // ── Agent 1: SecurityReview ───────────────────────────────────────

  const securityAgent = {
    label: 'SecurityReview',
    data: {
      diff: () => data.diff,
      changedFiles: () => data.changedFiles,
      commitMessages: () => data.commitMessages,
    },
    prompt: `You are a security reviewer for the ZeroClaw project (a sandboxed AI agent runtime built on NemoClaw).

You are reviewing the changes on branch: ${branchLabel}
${issueContext}

YOUR TASK: Review the provided diff for security implications. Focus on:

1. **Tool Registration Safety**: Check for new tool registrations that lack corresponding denylist entries. In NemoClaw, every tool that can execute code or access the filesystem must have a denylist entry in the policy files. Look for:
   - New tool_use definitions without corresponding policy restrictions
   - Tools that could bypass sandbox isolation
   - Missing allowlist/denylist updates when new capabilities are added

2. **Credential and Key Handling**: Check for:
   - Hardcoded secrets, API keys, tokens, or passwords
   - Credentials passed via environment variables without sanitization
   - Keys logged to stdout/stderr
   - Credentials stored in files without proper permissions

3. **Sandbox Escape Vectors**: Check for:
   - Shell injection via unsanitized inputs to exec/spawn/execSync
   - Path traversal (../ in file paths not validated)
   - Symlink following that could escape the sandbox
   - Network requests to localhost/internal IPs (SSRF)
   - Docker socket access or capability escalation
   - Process spawning that bypasses the sandbox process limit

4. **Input Validation Gaps**: Check for:
   - User/agent input used directly in file paths, URLs, or shell commands
   - Missing type checks on external data
   - Buffer/string size limits not enforced
   - Regex denial of service (ReDoS) patterns

Write your findings to: ${join(outputDir, 'pr-review-security.md')}

FORMAT your output file as:
\`\`\`markdown
# Security Review: ${branchLabel}

> Reviewed: [timestamp]
> Files analyzed: [count]

## Critical Findings
[List any critical security issues -- these MUST be fixed before merge]

## Warnings
[List moderate concerns that should be addressed]

## Observations
[List minor notes or suggestions]

## Files Reviewed
| File | Security Relevant | Notes |
|------|-------------------|-------|
| ... | Yes/No | ... |

## Verdict
[PASS / FAIL / NEEDS_DISCUSSION]
[One-paragraph summary of security posture]
\`\`\`

If you find NO security issues, still write the file with "PASS" verdict and explain what you checked.`,
    options: { maxTurns: 30 },
  };

  // ── Agent 2: CorrectnessReview ────────────────────────────────────

  const correctnessAgent = {
    label: 'CorrectnessReview',
    data: {
      diff: () => data.diff,
      changedFiles: () => data.changedFiles,
      commitMessages: () => data.commitMessages,
    },
    prompt: `You are a code correctness reviewer for the ZeroClaw project (a sandboxed AI agent runtime built on NemoClaw).

You are reviewing the changes on branch: ${branchLabel}
${issueContext}

YOUR TASK: Review the provided diff for bugs, logic errors, and edge cases. Focus on:

1. **Missing Error Handling**: Check for:
   - Async operations without try/catch or .catch()
   - File I/O without error handling (readFileSync, writeFileSync)
   - Network requests without timeout or error recovery
   - Promise chains that swallow errors silently
   - Missing null/undefined checks before property access

2. **Race Conditions**: Check for:
   - Shared mutable state accessed from async contexts without synchronization
   - Check-then-act patterns (TOCTOU) on filesystem operations
   - Event handlers that assume ordering
   - Concurrent writes to the same file or resource

3. **Type Mismatches**: Check for:
   - String used where number expected (or vice versa)
   - Array methods called on potentially non-array values
   - Object property access on potentially null/undefined
   - Implicit type coercion that could cause bugs (== vs ===)
   - TypeScript type assertions that skip validation (as any, !)

4. **Off-by-One and Logic Errors**: Check for:
   - Array index bounds (0-based vs 1-based confusion)
   - Loop boundary conditions (< vs <=)
   - String slicing/substring edge cases
   - Incorrect boolean logic (De Morgan violations, short-circuit issues)
   - Early returns that skip cleanup code

5. **API Contract Violations**: Check for:
   - Functions that don't return what callers expect
   - Changed function signatures without updating all call sites
   - Event names or message types that don't match listeners

Write your findings to: ${join(outputDir, 'pr-review-correctness.md')}

FORMAT your output file as:
\`\`\`markdown
# Correctness Review: ${branchLabel}

> Reviewed: [timestamp]
> Files analyzed: [count]

## Bugs Found
[List actual bugs with file:line references and explanation]

## Potential Issues
[List code that is suspicious but might be intentional]

## Edge Cases
[List unhandled edge cases that could cause failures]

## Code Quality Notes
[Optional: patterns that work but could be improved]

## Verdict
[CLEAN / HAS_BUGS / NEEDS_DISCUSSION]
[One-paragraph summary]
\`\`\`

If you find NO bugs, still write the file with "CLEAN" verdict and explain what you checked.`,
    options: { maxTurns: 30 },
  };

  // ── Agent 3: TestCoverage ─────────────────────────────────────────

  const testCoverageAgent = {
    label: 'TestCoverage',
    data: {
      diff: () => data.diff,
      changedFiles: () => data.changedFiles,
      commitMessages: () => data.commitMessages,
    },
    prompt: `You are a test coverage reviewer for the ZeroClaw project (a sandboxed AI agent runtime built on NemoClaw).

You are reviewing the changes on branch: ${branchLabel}
${issueContext}

YOUR TASK: Analyze whether the changed code has adequate test coverage. Focus on:

1. **New Functions Without Tests**: Check for:
   - Newly added exported functions that have no corresponding test
   - New classes or modules without test files
   - New CLI commands without integration tests
   - New API endpoints without request/response tests

2. **Changed Behavior Without Updated Tests**: Check for:
   - Modified function logic where existing tests don't cover the new behavior
   - Changed error handling paths without tests for the new error cases
   - Modified default values or configuration without tests
   - Changed validation rules without corresponding test updates

3. **Test Quality**: Check for:
   - Tests that only check the happy path (no error cases)
   - Tests that don't assert on the actual changed behavior
   - Missing edge case tests (empty input, null, boundary values)
   - Tests that are too tightly coupled to implementation details

4. **Recommended Test Cases**: For each gap found, suggest a specific test case:
   - What to test (function/behavior name)
   - Input values to use
   - Expected output or behavior
   - Why this test matters

Use the diff and changed file list to identify what was changed, then use Glob and Read
to check if corresponding test files exist in the repository. Look for test files in:
- \`test/\` directory (integration tests, ESM)
- \`nemoclaw/src/**/*.test.ts\` (co-located unit tests)
- \`test/e2e/\` (end-to-end tests)

Write your findings to: ${join(outputDir, 'pr-review-tests.md')}

FORMAT your output file as:
\`\`\`markdown
# Test Coverage Review: ${branchLabel}

> Reviewed: [timestamp]
> Files analyzed: [count]

## Coverage Gaps
[List functions/modules that are changed but lack test coverage]

## Recommended Test Cases

### [Gap 1 title]
- **What**: [function or behavior to test]
- **Test file**: [where to add the test]
- **Input**: [suggested test input]
- **Expected**: [expected behavior]
- **Priority**: High/Medium/Low

### [Gap 2 title]
...

## Existing Test Assessment
[Are existing tests still valid after the changes? Do any need updating?]

## Verdict
[ADEQUATE / NEEDS_TESTS / CRITICAL_GAP]
[One-paragraph summary]
\`\`\`

If test coverage is adequate, still write the file with "ADEQUATE" verdict and explain what you verified.`,
    options: { maxTurns: 40 },
  };

  // ── Agent 4: ReviewSummary (sequential — reads the 3 review files) ─

  const summaryAgent = {
    label: 'ReviewSummary',
    data: {},
    prompt: `You are a senior engineering reviewer producing the final summary for a PR review of the ZeroClaw project.

You are summarizing the review for branch: ${branchLabel}
${issueContext}

Three specialized review agents have already completed their analysis and written files:
1. ${join(outputDir, 'pr-review-security.md')} -- security review
2. ${join(outputDir, 'pr-review-correctness.md')} -- correctness review
3. ${join(outputDir, 'pr-review-tests.md')} -- test coverage review

YOUR TASK:
1. Read all three review files.
2. Synthesize them into a unified summary.
3. Write the summary to: ${join(outputDir, 'pr-review-summary.md')}

FORMAT your output file as:
\`\`\`markdown
# PR Review Summary: ${branchLabel}

> Generated: [timestamp]
> Reviewed by: SecurityReview, CorrectnessReview, TestCoverage agents

## Overall Verdict

**[APPROVE / REQUEST_CHANGES / COMMENT]**

[2-3 sentence justification for the verdict. Be direct.]

## Top 3 Most Important Findings

### 1. [Title]
- **Category**: Security / Correctness / Test Coverage
- **Severity**: Critical / High / Medium / Low
- **Details**: [Concise explanation]
- **Recommended action**: [What to do]

### 2. [Title]
- **Category**: Security / Correctness / Test Coverage
- **Severity**: Critical / High / Medium / Low
- **Details**: [Concise explanation]
- **Recommended action**: [What to do]

### 3. [Title]
- **Category**: Security / Correctness / Test Coverage
- **Severity**: Critical / High / Medium / Low
- **Details**: [Concise explanation]
- **Recommended action**: [What to do]

## Review Breakdown

### Security
- **Verdict**: [from security review]
- **Key points**: [1-2 bullet summary]

### Correctness
- **Verdict**: [from correctness review]
- **Key points**: [1-2 bullet summary]

### Test Coverage
- **Verdict**: [from test coverage review]
- **Key points**: [1-2 bullet summary]

## Commit Message Suggestions

If the existing commit messages could be improved, suggest better alternatives here.
Focus on Conventional Commits format: \`<type>(<scope>): <description>\`

Current commits:
${data.commitMessages || '(no commits found)'}

Suggested improvements (if any):
[List suggestions or "Commit messages are adequate."]

---
*This review was generated by the ZeroClaw PR Review Agent. All findings should be
verified by a human reviewer before acting on them.*
\`\`\`

RULES FOR THE VERDICT:
- **APPROVE**: No critical/high findings across all three reviews. Minor observations are OK.
- **REQUEST_CHANGES**: Any critical finding, or 2+ high findings that affect correctness or security.
- **COMMENT**: High findings that are debatable, or significant test gaps without correctness issues.

If a review file is missing or empty, note it in the summary and base the verdict on available reviews only.`,
    options: { maxTurns: 20 },
  };

  return { parallel: [securityAgent, correctnessAgent, testCoverageAgent], sequential: [summaryAgent] };
}

// ═══════════════════════════════════════════════════════════════════════
// MAIN
// ═══════════════════════════════════════════════════════════════════════

const cliArgs = parseArgs();

_log('PR Review Agent');
_log(`Model: ${MODEL}`);
_log(`Fallback: ${FALLBACK_MODEL}`);
_log(`Proxy: ${process.env.ANTHROPIC_BASE_URL}`);
_log(`Key: ${process.env.ANTHROPIC_API_KEY ? 'set' : 'MISSING'}`);
_log(`Tools: ${ALLOWED_TOOLS.join(', ')}`);
_log(`Debug log: ${DEBUG_LOG}`);
_log(`Branch: ${cliArgs.branch || '(from PR)'}`);
_log(`PR: ${cliArgs.pr || '(none)'}`);

const reviewData = gatherData(cliArgs);

if (!reviewData.diff) {
  console.error('');
  console.error('No diff found. The branch may be identical to upstream/main.');
  console.error('Make sure:');
  console.error(`  1. The branch "${cliArgs.branch || 'HEAD'}" has commits ahead of upstream/main`);
  console.error('  2. The upstream remote is configured: git remote add upstream <url>');
  console.error(`  3. You have fetched recently: git fetch upstream`);
  console.error('');
  process.exit(1);
}

const agents = buildAgents(reviewData, cliArgs);

_log(`Starting Phase 1: ${agents.parallel.length} parallel review agents...`);
const parallelResults = await runParallel(agents.parallel);

_log(`Phase 1 complete. Starting Phase 2: summary agent...`);
const sequentialResults = await runSequential(agents.sequential);

// ─── FINAL REPORT ──────────────────────────────────────────────────

const allOk = [...parallelResults, ...sequentialResults].every(r => r.status === 'OK');
const totalTime = Math.round((Date.now() - _startTime) / 1000);

_log('');
_log('═══════════════════════════════════════════════════════════════');
_log(`PR REVIEW COMPLETE: ${allOk ? 'ALL AGENTS SUCCEEDED' : 'SOME AGENTS FAILED'}`);
_log(`Total time: ${totalTime}s`);
_log('');
_log('Output files:');
_log(`  ${join(ROOT, 'pr-review-security.md')}`);
_log(`  ${join(ROOT, 'pr-review-correctness.md')}`);
_log(`  ${join(ROOT, 'pr-review-tests.md')}`);
_log(`  ${join(ROOT, 'pr-review-summary.md')}`);
_log('═══════════════════════════════════════════════════════════════');

if (!allOk) process.exit(1);
