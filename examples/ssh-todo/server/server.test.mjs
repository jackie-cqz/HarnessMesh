import test from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { promises as fsp } from 'node:fs';

import { createTodoServer } from './server.mjs';

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

async function makeDataFile(t) {
  const dir = await fsp.mkdtemp(path.join(os.tmpdir(), 'orion-todo-test-'));
  t.after(() => fsp.rm(dir, { recursive: true, force: true }));
  return path.join(dir, 'nested', 'todos.json');
}

function closeServer(server) {
  return new Promise((resolve) => {
    if (!server.listening) {
      resolve();
      return;
    }
    server.close(() => resolve());
  });
}

async function startServer(t, options = {}) {
  const server = createTodoServer(options);
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const address = server.address();
  assert.equal(typeof address, 'object');
  const baseUrl = `http://127.0.0.1:${address.port}`;
  t.after(() => closeServer(server));
  return { server, baseUrl };
}

async function send(baseUrl, pathname, { method = 'GET', body, rawBody } = {}) {
  const init = { method };
  if (rawBody !== undefined) {
    init.body = rawBody;
    init.headers = { 'Content-Type': 'application/json' };
  } else if (body !== undefined) {
    init.body = JSON.stringify(body);
    init.headers = { 'Content-Type': 'application/json' };
  }
  const res = await fetch(`${baseUrl}${pathname}`, init);
  const text = await res.text();
  let json;
  try {
    json = text === '' ? undefined : JSON.parse(text);
  } catch {
    json = undefined;
  }
  return { status: res.status, json, text, headers: res.headers };
}

function assertIsoTimestamp(value) {
  assert.equal(typeof value, 'string');
  const parsed = new Date(value);
  assert.ok(!Number.isNaN(parsed.getTime()), `invalid timestamp: ${value}`);
  assert.equal(parsed.toISOString(), value);
}

function assertTodoShape(todo) {
  assert.ok(todo && typeof todo === 'object');
  assert.deepEqual(Object.keys(todo).sort(), ['completed', 'created_at', 'id', 'title']);
  assert.match(todo.id, UUID_RE);
  assert.equal(typeof todo.title, 'string');
  assert.equal(typeof todo.completed, 'boolean');
  assertIsoTimestamp(todo.created_at);
}

test('GET /api/health reports the default service and node label', async (t) => {
  const dataFile = await makeDataFile(t);
  const { baseUrl } = await startServer(t, { dataFile });

  const res = await send(baseUrl, '/api/health');
  assert.equal(res.status, 200);
  assert.deepEqual(res.json, { status: 'ok', service: 'orion-todo-api', node: 'Orion' });
  assert.match(res.headers.get('content-type') ?? '', /application\/json/);
});

test('health honours a custom node label', async (t) => {
  const dataFile = await makeDataFile(t);
  const { baseUrl } = await startServer(t, { dataFile, nodeLabel: 'CustomNode' });

  const res = await send(baseUrl, '/api/health');
  assert.equal(res.status, 200);
  assert.equal(res.json.node, 'CustomNode');
});

test('empty list returns { todos: [] }', async (t) => {
  const dataFile = await makeDataFile(t);
  const { baseUrl } = await startServer(t, { dataFile });

  const res = await send(baseUrl, '/api/todos');
  assert.equal(res.status, 200);
  assert.deepEqual(res.json, { todos: [] });
});

test('create, list and complete a todo over real HTTP', async (t) => {
  const dataFile = await makeDataFile(t);
  const { baseUrl } = await startServer(t, { dataFile });

  const created = await send(baseUrl, '/api/todos', {
    method: 'POST',
    body: { title: '  Buy milk  ' },
  });
  assert.equal(created.status, 201);
  assertTodoShape(created.json.todo);
  assert.equal(created.json.todo.title, 'Buy milk');
  assert.equal(created.json.todo.completed, false);
  const { id, created_at: createdAt } = created.json.todo;

  const listed = await send(baseUrl, '/api/todos');
  assert.equal(listed.status, 200);
  assert.equal(listed.json.todos.length, 1);
  assert.deepEqual(listed.json.todos[0], created.json.todo);

  const patched = await send(baseUrl, `/api/todos/${id}`, {
    method: 'PATCH',
    body: { completed: true },
  });
  assert.equal(patched.status, 200);
  assertTodoShape(patched.json.todo);
  assert.equal(patched.json.todo.id, id);
  assert.equal(patched.json.todo.title, 'Buy milk');
  assert.equal(patched.json.todo.completed, true);
  assert.equal(patched.json.todo.created_at, createdAt);

  const after = await send(baseUrl, '/api/todos');
  assert.equal(after.json.todos.length, 1);
  assert.equal(after.json.todos[0].completed, true);

  const unpatched = await send(baseUrl, `/api/todos/${id}`, {
    method: 'PATCH',
    body: { completed: false },
  });
  assert.equal(unpatched.status, 200);
  assert.equal(unpatched.json.todo.completed, false);
  assert.equal(unpatched.json.todo.created_at, createdAt);
});

test('invalid create and patch payloads are rejected with safe JSON errors', async (t) => {
  const dataFile = await makeDataFile(t);
  const { baseUrl } = await startServer(t, { dataFile });

  const badCreates = [
    {},
    { title: '' },
    { title: '   ' },
    { title: 42 },
    { title: null },
    { title: ['a'] },
    { title: 'x'.repeat(201) },
    { title: 'ok', completed: 'yes' },
    { title: 'ok', completed: 1 },
  ];

  for (const body of badCreates) {
    const res = await send(baseUrl, '/api/todos', { method: 'POST', body });
    assert.equal(res.status, 400, `expected 400 for ${JSON.stringify(body)}`);
    assert.equal(typeof res.json.error, 'string');
    assert.ok(res.json.error.length > 0);
    assert.equal(res.json.stack, undefined);
  }

  const badJson = await send(baseUrl, '/api/todos', {
    method: 'POST',
    rawBody: '{"title": ',
  });
  assert.equal(badJson.status, 400);
  assert.equal(typeof badJson.json.error, 'string');

  const missingTitle = await send(baseUrl, '/api/todos', {
    method: 'POST',
    rawBody: JSON.stringify({ completed: false }),
  });
  assert.equal(missingTitle.status, 400);

  const created = await send(baseUrl, '/api/todos', { method: 'POST', body: { title: 'Valid' } });
  assert.equal(created.status, 201);
  const { id } = created.json.todo;

  for (const body of [{}, { completed: 'true' }, { completed: 1 }, { completed: null }]) {
    const res = await send(baseUrl, `/api/todos/${id}`, { method: 'PATCH', body });
    assert.equal(res.status, 400, `expected 400 for ${JSON.stringify(body)}`);
    assert.equal(typeof res.json.error, 'string');
  }

  const maxTitle = await send(baseUrl, '/api/todos', {
    method: 'POST',
    body: { title: 'y'.repeat(200) },
  });
  assert.equal(maxTitle.status, 201);
  assert.equal(maxTitle.json.todo.title.length, 200);

  const listed = await send(baseUrl, '/api/todos');
  assert.equal(listed.json.todos.length, 2);
  assert.equal(listed.json.todos[0].completed, false);
});

test('oversized request bodies return 413', async (t) => {
  const dataFile = await makeDataFile(t);
  const { baseUrl } = await startServer(t, { dataFile });

  const res = await send(baseUrl, '/api/todos', {
    method: 'POST',
    rawBody: JSON.stringify({ title: 'z'.repeat(70 * 1024) }),
  });
  assert.equal(res.status, 413);
  assert.equal(typeof res.json.error, 'string');

  const listed = await send(baseUrl, '/api/todos');
  assert.deepEqual(listed.json, { todos: [] });
});

test('unknown routes and unknown IDs return 404', async (t) => {
  const dataFile = await makeDataFile(t);
  const { baseUrl } = await startServer(t, { dataFile });

  const cases = [
    { pathname: '/', method: 'GET' },
    { pathname: '/api', method: 'GET' },
    { pathname: '/api/unknown', method: 'GET' },
    { pathname: '/api/todos', method: 'DELETE' },
    { pathname: '/api/todos', method: 'PUT' },
    { pathname: '/api/todos/some-id', method: 'GET' },
    { pathname: '/api/todos/some-id', method: 'DELETE' },
  ];

  for (const { pathname, method } of cases) {
    const res = await send(baseUrl, pathname, { method });
    assert.equal(res.status, 404, `expected 404 for ${method} ${pathname}`);
    assert.equal(typeof res.json.error, 'string');
  }

  const missingId = '11111111-2222-4333-8444-555555555555';
  const patch = await send(baseUrl, `/api/todos/${missingId}`, {
    method: 'PATCH',
    body: { completed: true },
  });
  assert.equal(patch.status, 404);
  assert.equal(typeof patch.json.error, 'string');
});

test('todos persist as JSON across a server restart', async (t) => {
  const dataFile = await makeDataFile(t);
  const first = await startServer(t, { dataFile });

  const one = await send(first.baseUrl, '/api/todos', {
    method: 'POST',
    body: { title: 'First task' },
  });
  const two = await send(first.baseUrl, '/api/todos', {
    method: 'POST',
    body: { title: 'Second task' },
  });
  assert.equal(one.status, 201);
  assert.equal(two.status, 201);

  const done = await send(first.baseUrl, `/api/todos/${two.json.todo.id}`, {
    method: 'PATCH',
    body: { completed: true },
  });
  assert.equal(done.status, 200);

  const expected = (await send(first.baseUrl, '/api/todos')).json.todos;
  assert.equal(expected.length, 2);

  const onDisk = JSON.parse(await fsp.readFile(dataFile, 'utf8'));
  assert.ok(Array.isArray(onDisk.todos));
  assert.equal(onDisk.todos.length, 2);
  assert.deepEqual(onDisk.todos, expected);

  await closeServer(first.server);
  const second = await startServer(t, { dataFile });

  const reloaded = await send(second.baseUrl, '/api/todos');
  assert.equal(reloaded.status, 200);
  assert.deepEqual(reloaded.json.todos, expected);
  assert.equal(reloaded.json.todos[1].completed, true);
});

test('concurrent creates do not lose writes', async (t) => {
  const dataFile = await makeDataFile(t);
  const { baseUrl } = await startServer(t, { dataFile });

  const results = await Promise.all(
    Array.from({ length: 20 }, (_, index) =>
      send(baseUrl, '/api/todos', { method: 'POST', body: { title: `task-${index}` } }),
    ),
  );

  assert.ok(results.every((res) => res.status === 201));
  const ids = new Set(results.map((res) => res.json.todo.id));
  assert.equal(ids.size, 20);

  const listed = await send(baseUrl, '/api/todos');
  assert.equal(listed.json.todos.length, 20);

  const onDisk = JSON.parse(await fsp.readFile(dataFile, 'utf8'));
  assert.equal(onDisk.todos.length, 20);
  assert.equal(new Set(onDisk.todos.map((todo) => todo.id)).size, 20);
});

test('a corrupt data file is never silently overwritten', async (t) => {
  const dataFile = await makeDataFile(t);
  await fsp.mkdir(path.dirname(dataFile), { recursive: true });
  const corrupt = '{"todos": [ this is not json';
  await fsp.writeFile(dataFile, corrupt, 'utf8');

  const { baseUrl } = await startServer(t, { dataFile });

  const listed = await send(baseUrl, '/api/todos');
  assert.equal(listed.status, 500);
  assert.equal(typeof listed.json.error, 'string');
  assert.equal(listed.json.stack, undefined);

  const created = await send(baseUrl, '/api/todos', { method: 'POST', body: { title: 'nope' } });
  assert.equal(created.status, 500);

  assert.equal(await fsp.readFile(dataFile, 'utf8'), corrupt);
});
