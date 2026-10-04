// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { spawn, spawnSync } from 'node:child_process';

const sensitive = /^(\.env(?:\..*)?|\.ssh|\.aws|\.gnupg|\.npmrc|\.netrc|.*\.(?:pem|key|p12|pfx)|(?:secrets?|credentials?)(?:\..*)?)$/i;
const protectedNames = new Set(['.git', '.pi', '.hamstik-wheel.toml', 'node_modules']);
const inside = (root, target) => target === root || target.startsWith(root + path.sep);

export function guardPath(root, requested, mutation = false) {
  const target = path.resolve(root, requested);
  if (!inside(root, target)) throw new Error('Wheel denies paths outside the repository');
  let cursor = root;
  for (const part of path.relative(root, target).split(path.sep).filter(Boolean)) {
    if (sensitive.test(part) || part === '.git' || part === '.pi' || (mutation && protectedNames.has(part))) {
      throw new Error('Wheel denies sensitive or protected paths');
    }
    cursor = path.join(cursor, part);
    try {
      const stat = fs.lstatSync(cursor);
      // Deny symlinks for direct tools, including dangling and in-repo aliases.
      if (stat.isSymbolicLink()) throw new Error('Wheel denies symlink paths; use the real repository path');
      if (mutation && stat.isFile() && stat.nlink > 1) throw new Error('Wheel denies writes to hard-linked files');
      if (!stat.isFile() && !stat.isDirectory()) throw new Error('Wheel denies special files');
    } catch (error) { if (error.code !== 'ENOENT') throw error; }
  }
  return target;
}

// Collect mounts before executing untrusted shell code. Never follow symlinks.
function protectTree(root, args, readOnly = false) {
  const hardlinks = new Map();
  const walk = (dir, inheritedReadOnly) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const p = path.join(dir, entry.name);
      if (entry.isSymbolicLink()) {
        if (sensitive.test(entry.name)) throw new Error('Wheel refuses symlinked credential paths: ' + p);
        continue;
      }
      if (sensitive.test(entry.name) || entry.name === '.pi' || (inheritedReadOnly && entry.name === 'hamstik-wheel')) {
        args.push(...(entry.isDirectory() ? ['--tmpfs', p, '--remount-ro', p] : ['--ro-bind', '/dev/null', p]));
      } else if (protectedNames.has(entry.name)) {
        args.push('--ro-bind', p, p);
        if (entry.isDirectory()) walk(p, true);
      } else if (entry.isDirectory()) {
        walk(p, inheritedReadOnly);
      } else if (!entry.isFile()) {
        throw new Error('Wheel refuses repository sockets/devices/FIFOs: ' + p);
      } else {
        const stat = fs.statSync(p);
        if (!inheritedReadOnly && stat.nlink > 1) {
          const id = `${stat.dev}:${stat.ino}`;
          const record = hardlinks.get(id) ?? { count: stat.nlink, paths: [] };
          record.paths.push(p); hardlinks.set(id, record);
        }
      }
    }
  };
  walk(root, readOnly);
  for (const { count, paths } of hardlinks.values()) {
    if (paths.length < count) for (const p of paths) args.push('--ro-bind', '/dev/null', p);
  }
}

export function sandboxArgs(config, command) {
  if (process.platform !== 'linux') throw new Error('Wheel tool isolation requires Linux and bubblewrap');
  const root = fs.realpathSync(config.root);
  if (root === '/' || ['/usr', '/bin', '/lib', '/lib64', '/etc', '/proc', '/dev', '/run', '/tmp'].includes(root)) {
    throw new Error('Wheel requires a dedicated repository directory');
  }
  const args = ['--unshare-all', '--die-with-parent', '--new-session', '--cap-drop', 'ALL', '--clearenv'];
  for (const p of ['/usr', '/bin', '/lib', '/lib64']) {
    if (fs.existsSync(p)) args.push('--ro-bind', p, p);
  }
  args.push('--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp', '--dir', '/run', '--dir', '/etc');
  for (const p of config.readOnlyPaths ?? []) {
    const actual = fs.realpathSync(p);
    if (actual === '/' || actual === os.homedir() || /^\/home\/[^/]+$/.test(actual) || inside(root, actual) || ['/root', '/home', '/run', '/proc', '/dev', '/tmp', '/etc'].includes(actual) || inside(actual, root)) {
      throw new Error('read_only_paths must identify specific toolchain directories, not host roots');
    }
    if (!fs.statSync(actual).isDirectory()) throw new Error('read_only_paths must be directories');
    args.push('--ro-bind', actual, actual);
    protectTree(actual, args, true);
  }
  args.push('--bind', root, root);
  protectTree(root, args);
  // A linked worktree needs read-only access to its external Git metadata.
  for (const p of config.gitPaths ?? []) {
    if (!inside(root, p)) { args.push('--ro-bind', p, p); protectTree(p, args, true); }
    const metadata = path.join(p, 'hamstik-wheel');
    if (fs.existsSync(metadata)) args.push('--tmpfs', metadata, '--remount-ro', metadata);
  }
  args.push('--dir', '/tmp/wheel-home', '--setenv', 'HOME', '/tmp/wheel-home', '--setenv', 'TMPDIR', '/tmp',
    '--setenv', 'PATH', (config.readOnlyPaths ?? []).flatMap(p => [path.join(p, 'bin'), p]).concat(['/usr/local/bin', '/usr/bin', '/bin']).join(':'),
    '--setenv', 'LANG', 'C.UTF-8', '--setenv', 'GIT_CONFIG_NOSYSTEM', '1', '--setenv', 'GIT_CONFIG_GLOBAL', '/dev/null',
    '--chdir', root, '--remount-ro', '/', '--', '/bin/bash', '--noprofile', '--norc', '-c', command);
  return args;
}

export function checkSandbox(config) {
  const result = spawnSync('/usr/bin/bwrap', sandboxArgs(config, 'test -x /usr/bin/node && /usr/bin/node --version >/dev/null'), { encoding: 'utf8', timeout: 10000, killSignal: 'SIGKILL' });
  if (result.error || result.status !== 0) throw new Error('Wheel sandbox unavailable: ' + (result.error?.message ?? result.stderr));
}

export function executeSandbox(config, command, options) {
  return new Promise((resolve, reject) => {
    if (options.signal?.aborted) return reject(new Error('Aborted'));
    const child = spawn('/usr/bin/bwrap', sandboxArgs(config, command), { stdio: ['pipe', 'pipe', 'pipe'], env: {} });
    child.stdin.on('error', () => {});
    child.stdin.end(options.input ?? '');
    const abort = () => child.kill('SIGKILL');
    const timer = setTimeout(abort, Math.min(options.timeout ?? 300, 1800) * 1000);
    options.signal?.addEventListener('abort', abort, { once: true });
    child.stdout.on('data', options.onData);
    child.stderr.on('data', options.onStderr ?? options.onData);
    const cleanup = () => { clearTimeout(timer); options.signal?.removeEventListener('abort', abort); };
    child.on('error', error => { cleanup(); reject(error); });
    child.on('close', (code) => { cleanup(); resolve({ exitCode: code ?? 137 }); });
  });
}

if (process.argv[2] === '--check') checkSandbox(JSON.parse(process.env.HAMSTIK_WHEEL_SANDBOX));

// All file I/O also runs inside the mount namespace: path validation alone
// cannot prevent a concurrent shell from swapping a symlink after a check.
export async function fileOperation(config, operation, target, content) {
  guardPath(config.root, target, ['writeFile', 'mkdir'].includes(operation));
  const script = `const fs=require('fs');const x=JSON.parse(fs.readFileSync(0,'utf8'));
    if(x.operation==='readFile')process.stdout.write(fs.readFileSync(x.target).toString('base64'));
    else if(x.operation==='writeFile')fs.writeFileSync(x.target,x.content);
    else if(x.operation==='mkdir')fs.mkdirSync(x.target,{recursive:true});
    else fs.accessSync(x.target);`;
  const chunks = [];
  const result = await executeSandbox(config, '/usr/bin/node -e ' + "'" + script.replaceAll("'", "'\\''") + "'", {
    input: JSON.stringify({ operation, target, content }), onData: data => chunks.push(data), timeout: 30,
  });
  if (result.exitCode !== 0) throw new Error('Sandboxed file operation failed: ' + Buffer.concat(chunks).toString());
  return operation === 'readFile' ? Buffer.from(Buffer.concat(chunks).toString(), 'base64') : undefined;
}

export async function shellResult(config, command, options = {}) {
  const maxBytes = 64 * 1024;
  let tail = Buffer.alloc(0);
  let total = 0;
  const result = await executeSandbox(config, command, {
    ...options,
    onData: data => {
      total += data.length;
      tail = Buffer.concat([tail, data]).subarray(-maxBytes);
    },
  });
  return { ...result, output: tail.toString('utf8'), truncated: total > maxBytes };
}


if (process.argv[2] === '--run') {
  const config = JSON.parse(process.env.HAMSTIK_WHEEL_SANDBOX);
  const result = await executeSandbox(config, process.argv[3], {
    onData: data => process.stdout.write(data),
    onStderr: data => process.stderr.write(data),
    timeout: 1800,
  });
  process.exitCode = result.exitCode;
}
