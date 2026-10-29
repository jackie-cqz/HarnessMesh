// Independent HTTP acceptance against the real SSH-backed application.
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { writeFile } from 'node:fs/promises';

const base = process.argv[2] || 'http://127.0.0.1:5173';
const output = process.argv[3];
const persistedId = process.argv[4];
const checks = [];
async function request(path, method = 'GET', body, expected = 200) {
  const response = await fetch(new URL(path, base), {
    method, headers: body === undefined ? {} : { 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(10000),
  });
  assert.equal(response.status, expected, `${method} ${path}`);
  const result = await response.json();
  checks.push({ method, path, status: response.status, passed: true });
  return result;
}
const health = await request('/api/health');
assert.equal(health.status, 'ok');
assert.equal(health.service, 'orion-todo-api');
assert.equal(health.node, 'Orion');
let record;
if (persistedId) {
  const list = await request('/api/todos');
  record = list.todos.find(todo => todo.id === persistedId);
  assert.ok(record, 'created todo survived backend restart');
  assert.equal(record.completed, true);
} else {
  const title = `跨机 API 验收 ${new Date().toISOString()}`;
  ({ todo: record } = await request('/api/todos', 'POST', { title: `  ${title}  ` }, 201));
  assert.equal(record.title, title);
  assert.equal(record.completed, false);
  assert.match(record.id, /^[0-9a-f-]{36}$/i);
  assert.ok(Number.isFinite(Date.parse(record.created_at)));
  const updated = await request(`/api/todos/${record.id}`, 'PATCH', { completed: true });
  assert.equal(updated.todo.completed, true);
  record = updated.todo;
  const list = await request('/api/todos');
  assert.ok(list.todos.some(todo => todo.id === record.id && todo.completed));
  for (const title of ['', '   ', '中'.repeat(201)]) {
    const error = await request('/api/todos', 'POST', { title }, 400);
    assert.equal(typeof error.error, 'string');
  }
  await request(`/api/todos/${record.id}`, 'PATCH', { completed: 'true' }, 400);
  await request(`/api/todos/${randomUUID()}`, 'PATCH', { completed: true }, 404);
  await request('/api/todos', 'POST', { title: 'x'.repeat(70000) }, 413);
}
const report = {
  date: new Date().toISOString(), base, status: 'passed',
  phase: persistedId ? 'persistence-after-backend-restart' : 'real-cross-machine-http',
  health, todo: record, checks,
};
if (output) await writeFile(output, JSON.stringify(report, null, 2) + '\n');
console.log(JSON.stringify(report, null, 2));
