// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0
import { createReadToolDefinition, createWriteToolDefinition, createEditToolDefinition } from '@earendil-works/pi-coding-agent';
import { Type } from 'typebox';
import { guardPath, checkSandbox, fileOperation, shellResult } from './sandbox.mjs';
import { emitProtocol } from './protocol.mjs';

function scrub(value) {
  if (typeof value === 'string') return value.replace(/([a-z][a-z0-9+.-]*:\/\/)[^\s/"']+@/gi, '$1[REDACTED]@');
  if (Array.isArray(value)) return value.map(scrub);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, /password|secret|token|api_key|authorization|database_url/i.test(k) && typeof v === 'string' ? '[REDACTED]' : scrub(v)]));
  return value;
}

export default function (pi) {
  pi.registerFlag('wheel-guard-check', { type: 'boolean', description: 'Verify Wheel tool isolation without a model request' });
  const config = JSON.parse(process.env.HAMSTIK_WHEEL_SANDBOX);
  delete process.env.HAMSTIK_WHEEL_SANDBOX;
  checkSandbox(config);
  let finished = false;
  let active = 0;
  const guarded = execute => async (...args) => {
    if (finished) throw new Error('Wheel result already submitted; tools are closed');
    active++;
    try { return scrub(await execute(...args)); } finally { active--; }
  };
  for (const [name, factory, mutation] of [
    ['read', createReadToolDefinition, false],
    ['write', createWriteToolDefinition, true],
    ['edit', createEditToolDefinition, true],
  ]) {
    const tool = factory(config.root, { autoResizeImages: false, operations: {
      readFile: p => fileOperation(config, 'readFile', p),
      writeFile: (p, content) => fileOperation(config, 'writeFile', p, content),
      access: p => fileOperation(config, 'access', p),
      mkdir: p => fileOperation(config, 'mkdir', p),
      detectImageMimeType: async p => ({ png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', webp: 'image/webp' })[p.split('.').pop().toLowerCase()],
    } });
    pi.registerTool({ ...tool, name: 'wheel_' + name,
      execute: guarded(async (id, params, signal, _onUpdate, ctx) => {
        guardPath(config.root, params.path, mutation);
        return tool.execute(id, params, signal, undefined, ctx);
      }),
    });
  }
  // Do not delegate shell output to Pi's standard accumulator: it spills
  // truncated output to a host /tmp file before a result can be redacted.
  pi.registerTool({
    name: 'wheel_bash', label: 'Isolated shell',
    description: 'Run a shell command inside the repository sandbox, with private temporary storage and no host networking or credentials. Output is bounded; narrow commands to inspect more.',
    parameters: Type.Object({ command: Type.String(), timeout: Type.Optional(Type.Number({ minimum: 1, maximum: 1800 })) }),
    execute: guarded(async (_id, params, signal) => {
      const result = await shellResult(config, params.command, { signal, timeout: params.timeout });
      return { content: [{ type: 'text', text: result.output + `\nExit code: ${result.exitCode}` + (result.truncated ? '\nOutput truncated to the last 64 KiB; no host spill file was written.' : '') }], details: { exitCode: result.exitCode, truncated: result.truncated } };
    }),
  });
  pi.registerTool({
    name: 'wheel_result', label: 'Submit Wheel result',
    description: 'Required final action. Submit the implementation/review verdict and every unverified required check. Tool use ends after submission. Do not replace this with a prose summary.',
    parameters: Type.Object({
      status: Type.Union(['ready_for_review', 'pass', 'blocked'].map(v => Type.Literal(v))),
      summary: Type.String(), findings: Type.Array(Type.String()), unverified_checks: Type.Array(Type.String()),
    }),
    async execute(_id, result) {
      if (finished) throw new Error('Wheel result already submitted');
      if (active) throw new Error('Wait for every running tool before submitting a result');
      finished = true;
      emitProtocol({ type: 'wheel_result', result: scrub(result) });
      return { content: [{ type: 'text', text: 'Result recorded. End the session now.' }] };
    },
  });
  emitProtocol({ type: 'wheel_guard_ready', version: 1 });
  if (pi.getFlag('wheel-guard-check')) process.exit(0);
}
