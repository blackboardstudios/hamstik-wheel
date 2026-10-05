// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0
import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import net from 'node:net';
import { spawnSync } from 'node:child_process';
import { guardPath, checkSandbox, executeSandbox, fileOperation, shellResult } from '../runtime/sandbox.mjs';

test('protocol records bypass Pi stdout redirection, including large verdicts', () => {
  const moduleUrl = new URL('../runtime/protocol.mjs', import.meta.url).href;
  const result = spawnSync(process.execPath, ['--input-type=module', '-e', `
    import { emitProtocol } from ${JSON.stringify(moduleUrl)};
    process.stdout.write = process.stderr.write.bind(process.stderr);
    emitProtocol({ type: 'wheel_guard_ready', version: 1 });
    emitProtocol({ type: 'wheel_result', result: { summary: 'x'.repeat(200000) } });
  `], { encoding: 'utf8', timeout: 15000 });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stderr, '');
  const events = result.stdout.trim().split('\n').map(line => JSON.parse(line));
  assert.equal(events[0].type, 'wheel_guard_ready');
  assert.equal(events[1].type, 'wheel_result');
  assert.equal(events[1].result.summary.length, 200000);
});

function fixture(t) {
  const parent = fs.mkdtempSync(path.join(os.tmpdir(), 'wheel-isolation-'));
  const root = path.join(parent, 'repo'); fs.mkdirSync(root);
  t.after(() => fs.rmSync(parent, { recursive: true, force: true }));
  return { parent, root, config: { root } };
}
async function shell(config, cmd) {
  let output = '';
  const result = await executeSandbox(config, cmd, { onData: b => output += b.toString(), timeout: 5 });
  return { ...result, output };
}

test('all file operations stay in the repository, including symlinks and hardlinks', async t => {
  const { parent, root, config } = fixture(t);
  const outside = path.join(parent, 'outside'); fs.writeFileSync(outside, 'original');
  fs.symlinkSync(outside, path.join(root, 'link'));
  fs.linkSync(outside, path.join(root, 'hardlink'));
  for (const target of [outside, '../outside', 'link', 'hardlink']) {
    assert.throws(() => guardPath(root, target, true));
  }
  await fileOperation(config, 'writeFile', path.join(root, 'ok.txt'), 'safe');
  assert.equal((await fileOperation(config, 'readFile', path.join(root, 'ok.txt'))).toString(), 'safe');
  for (const target of [outside, path.join(root, 'link'), path.join(root, 'hardlink')]) {
    await shell(config, `echo changed > '${target}'`);
    assert.equal(fs.readFileSync(outside, 'utf8'), 'original');
  }
  assert.equal(fs.readFileSync(outside, 'utf8'), 'original');
  const leaked = await shell(config, 'cat hardlink');
  assert.ok(!leaked.output.includes('original'));
  assert.equal((await fileOperation(config, 'readFile', path.join(root, 'hardlink'))).length, 0);
});

test('secret files and host credentials are unavailable, Git metadata cannot be changed', async t => {
  const { root, config } = fixture(t);
  fs.writeFileSync(path.join(root, '.env'), 'PRIVATE_VALUE=private-value');
  fs.mkdirSync(path.join(root, '.git')); fs.writeFileSync(path.join(root, '.git', 'config'), 'original');
  assert.throws(() => guardPath(root, '.env'));
  assert.throws(() => guardPath(root, '.git/config', true));
  process.env.WHEEL_TEST_SECRET = 'inherited-secret';
  t.after(() => delete process.env.WHEEL_TEST_SECRET);
  const result = await shell(config, 'cat .env; env; test ! -e /run/docker.sock; test ! -e /var/run/docker.sock');
  assert.equal(result.exitCode, 0);
  assert.ok(!result.output.includes('private-value') && !result.output.includes('inherited-secret'));
  assert.notEqual((await shell(config, 'echo change > .git/config')).exitCode, 0);
  assert.equal(fs.readFileSync(path.join(root, '.git', 'config'), 'utf8'), 'original');
});

test('network namespace cannot reach a host listener; temporary files are private', async t => {
  const { config, parent } = fixture(t);
  const server = net.createServer(socket => socket.end('host'));
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => server.close());
  const port = server.address().port;
  assert.notEqual((await shell(config, `echo hi > /dev/tcp/127.0.0.1/${port}`)).exitCode, 0);
  const hostTemp = path.join(parent, 'not-a-repository-file');
  await shell(config, `echo unsafe > '${hostTemp}'`);
  assert.notEqual((await shell(config, 'echo unsafe > /opt/wheel-outside')).exitCode, 0);
  assert.equal(fs.existsSync(hostTemp), false);
  assert.equal((await shell(config, 'echo private > /tmp/wheel-private')).exitCode, 0);
  assert.equal((await shell(config, 'test ! -e /tmp/wheel-private')).exitCode, 0);
});

test('background shell processes cannot write after the tool returns', async t => {
  const { root, config } = fixture(t);
  await shell(config, '(sleep .2; echo escaped > delayed) >/dev/null 2>&1 &');
  await new Promise(resolve => setTimeout(resolve, 400));
  assert.equal(fs.existsSync(path.join(root, 'delayed')), false);
});

test('repository sockets are masked without locking out unrelated tools', async t => {
  const { root, config } = fixture(t);
  const server = net.createServer();
  await new Promise(resolve => server.listen(path.join(root, 'host.sock'), resolve));
  t.after(() => server.close());
  checkSandbox(config);
  assert.throws(() => guardPath(root, 'host.sock'));
  assert.equal((await shell(config, 'echo usable > recovery.txt')).exitCode, 0);
  assert.notEqual((await shell(config, `node -e "require('net').connect('host.sock').on('error',()=>process.exit(1))"`)).exitCode, 0);
});

test('internal hardlinks do not produce thousands of bind mounts', t => {
  const { root, config } = fixture(t);
  fs.writeFileSync(path.join(root, 'a'), 'original');
  fs.linkSync(path.join(root, 'a'), path.join(root, 'b'));
  checkSandbox(config);
});

test('large shell output stays bounded without host spill files', async t => {
  const { config } = fixture(t);
  const before = new Set(fs.readdirSync(os.tmpdir()).filter(p => p.startsWith('pi-bash-')));
  const result = await shellResult(config, "head -c 200000 /dev/zero | tr '\\0' x");
  assert.equal(result.exitCode, 0);
  assert.equal(result.truncated, true);
  assert.equal(Buffer.byteLength(result.output), 64 * 1024);
  assert.deepEqual(new Set(fs.readdirSync(os.tmpdir()).filter(p => p.startsWith('pi-bash-'))), before);
});

test('read-only toolchains hide credentials and mask IPC sockets', async t => {
  const { parent, root, config } = fixture(t);
  const toolchain = path.join(parent, 'toolchain'); fs.mkdirSync(toolchain);
  fs.writeFileSync(path.join(toolchain, 'credentials.toml'), 'private-key');
  config.readOnlyPaths = [toolchain];
  const result = await shell(config, `cat '${toolchain}/credentials.toml'`);
  assert.ok(!result.output.includes('private-key'));
  const server = net.createServer();
  await new Promise(resolve => server.listen(path.join(toolchain, 'daemon.sock'), resolve));
  t.after(() => server.close());
  checkSandbox(config);
  assert.notEqual((await shell(config, `node -e "require('net').connect('${toolchain}/daemon.sock').on('error',()=>process.exit(1))"`)).exitCode, 0);
  assert.throws(() => checkSandbox({root, readOnlyPaths: [os.homedir()]}), /toolchain/);
});


test('source modules and templates remain readable while credentials stay masked', async t => {
  const { root, config } = fixture(t);
  for (const name of ['secrets.ts', 'secrets.test.ts', 'Secrets.js', 'Secrets.js.map', 'secrets.d.ts.map', 'credentials.py', '.env.example']) {
    fs.writeFileSync(path.join(root, name), 'public-source');
    assert.equal((await fileOperation(config, 'readFile', path.join(root, name))).toString(), 'public-source');
  }
  for (const name of ['secrets.json', 'credentials.toml', '.env.local', 'private.key', '.env.example.local']) {
    fs.writeFileSync(path.join(root, name), 'private-value');
    assert.throws(() => guardPath(root, name));
    assert.ok(!(await shell(config, `cat '${name}'`)).output.includes('private-value'));
  }
});

test('dependency writes and caches are ephemeral; credentials and host hardlinks stay protected', async t => {
  const { root, parent, config } = fixture(t);
  const modules = path.join(root, 'node_modules'); fs.mkdirSync(modules);
  const outside = path.join(parent, 'dependency-outside'); fs.writeFileSync(outside, 'host-original');
  fs.linkSync(outside, path.join(modules, 'linked.js'));
  fs.writeFileSync(path.join(modules, 'Secrets.js'), 'export default 1');
  fs.writeFileSync(path.join(modules, 'credentials.json'), 'private-value');
  const result = await shell(config, 'mkdir -p node_modules/.vite-temp && echo cached > node_modules/.vite-temp/config.mjs && echo edited > node_modules/linked.js && cat node_modules/Secrets.js');
  assert.equal(result.exitCode, 0, result.output);
  assert.ok(result.output.includes('export default 1'));
  assert.equal(fs.readFileSync(outside, 'utf8'), 'host-original');
  assert.equal(fs.existsSync(path.join(modules, '.vite-temp')), false);
  assert.equal((await shell(config, 'test ! -e node_modules/.vite-temp/config.mjs')).exitCode, 0);
  assert.ok(!(await shell(config, 'cat node_modules/credentials.json')).output.includes('private-value'));
});

test('system identity lookup works without exposing shadow or host configuration', async t => {
  const { config } = fixture(t);
  const result = await shell(config, 'id -un && test ! -e /etc/shadow && test ! -e /etc/shadow-');
  assert.equal(result.exitCode, 0, result.output);
});


test('build caches hide old env copies and allow clean builds without host writes', async t => {
  const { root, config } = fixture(t);
  fs.mkdirSync(path.join(root, '.next', 'standalone'), { recursive: true });
  fs.writeFileSync(path.join(root, '.next', 'standalone', '.env'), 'private-build-value');
  const result = await shell(config, 'rm -rf .next/standalone && mkdir -p .next/standalone && echo fresh > .next/standalone/output');
  assert.equal(result.exitCode, 0, result.output);
  assert.equal(fs.readFileSync(path.join(root, '.next', 'standalone', '.env'), 'utf8'), 'private-build-value');
  assert.equal(fs.existsSync(path.join(root, '.next', 'standalone', 'output')), false);
  assert.ok(!(await shell(config, 'cat .next/standalone/.env')).output.includes('private-build-value'));
});


test('multiple masked credentials are empty regular files for dotenv and file APIs', async t => {
  const { root, config } = fixture(t);
  fs.writeFileSync(path.join(root, '.env'), 'WHEEL_PRIVATE_VALUE=host-secret');
  fs.writeFileSync(path.join(root, '.env.local'), 'WHEEL_PRIVATE_VALUE=other-host-secret');
  checkSandbox(config);
  const result = await shell(config, `node -e "const fs=require('fs');for(const p of ['.env','.env.local']){if(!fs.statSync(p).isFile())process.exit(2);if(fs.readFileSync(p).length)process.exit(3);process.loadEnvFile(p);}if(process.env.WHEEL_PRIVATE_VALUE)process.exit(4)"`);
  assert.equal(result.exitCode, 0, result.output);
  assert.equal(fs.readFileSync(path.join(root, '.env'), 'utf8'), 'WHEEL_PRIVATE_VALUE=host-secret');
});
