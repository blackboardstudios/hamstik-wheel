// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0
import { writeSync } from 'node:fs';

// Pi redirects process.stdout.write to stderr while loading extensions. Write
// protocol records to the descriptor directly, without allowing other JS event
// writers to interleave a partially written record.
export function emitProtocol(event) {
  const bytes = Buffer.from(JSON.stringify(event) + '\n');
  const deadline = Date.now() + 10000;
  const sleeper = new Int32Array(new SharedArrayBuffer(4));
  let offset = 0;
  while (offset < bytes.length) {
    try {
      offset += writeSync(1, bytes, offset, bytes.length - offset);
    } catch (error) {
      if (!['EAGAIN', 'EWOULDBLOCK', 'ENOBUFS'].includes(error.code) || Date.now() >= deadline) throw error;
      Atomics.wait(sleeper, 0, 0, 10);
    }
  }
}
