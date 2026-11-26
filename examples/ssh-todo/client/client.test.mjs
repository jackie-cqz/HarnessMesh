import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { createFrontendServer } from './serve.mjs';

const HOST = '127.0.0.1';

function listen(server) {
  return new Promise((resolve, reject) => {
    const onError = (err) => reject(err);
    server.once('error', onError);
    server.listen(0, HOST, () => {
      server.removeListener('error', onError);
      resolve(server.address().port);
    });
  });
}

function closeServer(server) {
  return new Promise((resolve, reject) => {
    if (typeof server.closeAllConnections === 'function') {
      server.closeAllConnections();
    }
    server.close((err) => (err ? reject(err) : resolve()));
  });
}

function rawRequest(port, { method = 'GET', path = '/', headers = {}, body } = {}) {
  return new Promise((resolve, reject) => {
    const req = http.request({ host: HOST, port, method, path, headers }, (res) => {
      const chunks = [];
      res.on('data', (chunk) => chunks.push(chunk));
      res.on('end', () => {
        resolve({
          status: res.statusCode,
          headers: res.headers,
          body: Buffer.concat(chunks).toString('utf8'),
        });
      });
    });
    req.on('error', reject);
    if (body !== undefined) req.write(body);
    req.end();
  });
}

test('serves declared static assets only', async (t) => {
  const server = createFrontendServer({ upstream: 'http://127.0.0.1:1' });
  const port = await listen(server);
  t.after(() => closeServer(server));

  const index = await rawRequest(port, { path: '/' });
  assert.equal(index.status, 200);
  assert.match(index.headers['content-type'], /text\/html/);
  assert.match(index.body, /待办事项/);
  assert.match(index.body, /Mesh Tasks/);

  const css = await rawRequest(port, { path: '/styles.css' });
  assert.equal(css.status, 200);
  assert.match(css.headers['content-type'], /text\/css/);

  const js = await rawRequest(port, { path: '/app.js' });
  assert.equal(js.status, 200);
  assert.match(js.headers['content-type'], /javascript/);

  const head = await rawRequest(port, { method: 'HEAD', path: '/' });
  assert.equal(head.status, 200);
  assert.equal(head.body, '');

  const missing = await rawRequest(port, { path: '/nope' });
  assert.equal(missing.status, 404);

  const postStatic = await rawRequest(port, { method: 'POST', path: '/' });
  assert.equal(postStatic.status, 405);
});

test('rejects traversal, hidden files and unrelated files', async (t) => {
  const server = createFrontendServer({ upstream: 'http://127.0.0.1:1' });
  const port = await listen(server);
  t.after(() => closeServer(server));

  const hostile = [
    '/../serve.mjs',
    '/..%2Fserve.mjs',
    '/%2e%2e/serve.mjs',
    '/.env',
    '/.hidden',
    '/api-contract.json',
    '/package.json',
    '/serve.mjs',
    '/client.test.mjs',
  ];

  for (const path of hostile) {
    const res = await rawRequest(port, { path });
    assert.equal(res.status, 404, `expected 404 for ${path}, got ${res.status}`);
  }
});

test('proxies /api/* to upstream preserving method, path and body', async (t) => {
  const seen = [];
  const upstream = http.createServer((req, res) => {
    const chunks = [];
    req.on('data', (chunk) => chunks.push(chunk));
    req.on('end', () => {
      const body = Buffer.concat(chunks).toString('utf8');
      seen.push({
        method: req.method,
        url: req.url,
        contentType: req.headers['content-type'] || null,
        body,
      });

      const json = { 'content-type': 'application/json' };
      if (req.method === 'GET' && req.url === '/api/health') {
        res.writeHead(200, json);
        res.end(JSON.stringify({ status: 'ok', service: 'linux-todo-api', node: 'Linux' }));
      } else if (req.method === 'GET' && req.url === '/api/todos') {
        res.writeHead(200, json);
        res.end(
          JSON.stringify({
            todos: [{ id: '1', title: '示例', completed: false, created_at: '2024-01-01T00:00:00Z' }],
          }),
        );
      } else if (req.method === 'POST' && req.url === '/api/todos') {
        res.writeHead(201, json);
        res.end(
          JSON.stringify({
            todo: { id: '2', title: JSON.parse(body).title, completed: false, created_at: '2024-01-01T00:00:00Z' },
          }),
        );
      } else if (req.method === 'PATCH' && req.url === '/api/todos/1') {
        res.writeHead(200, json);
        res.end(
          JSON.stringify({
            todo: { id: '1', title: '示例', completed: JSON.parse(body).completed, created_at: '2024-01-01T00:00:00Z' },
          }),
        );
      } else {
        res.writeHead(404, json);
        res.end(JSON.stringify({ error: 'not found' }));
      }
    });
  });

  const upstreamPort = await listen(upstream);
  t.after(() => closeServer(upstream));

  const server = createFrontendServer({ upstream: `http://${HOST}:${upstreamPort}` });
  const port = await listen(server);
  t.after(() => closeServer(server));

  const health = await rawRequest(port, { path: '/api/health' });
  assert.equal(health.status, 200);
  assert.match(health.headers['content-type'], /application\/json/);
  assert.deepEqual(JSON.parse(health.body), { status: 'ok', service: 'linux-todo-api', node: 'Linux' });

  const list = await rawRequest(port, { path: '/api/todos' });
  assert.equal(list.status, 200);
  assert.equal(JSON.parse(list.body).todos.length, 1);

  const post = await rawRequest(port, {
    method: 'POST',
    path: '/api/todos',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ title: '  新任务  ' }),
  });
  assert.equal(post.status, 201);
  assert.match(post.headers['content-type'], /application\/json/);
  assert.equal(JSON.parse(post.body).todo.title, '  新任务  ');

  const patch = await rawRequest(port, {
    method: 'PATCH',
    path: '/api/todos/1',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ completed: true }),
  });
  assert.equal(patch.status, 200);
  assert.equal(JSON.parse(patch.body).todo.completed, true);

  assert.equal(seen.length, 4);
  assert.deepEqual(
    seen.map((entry) => [entry.method, entry.url]),
    [
      ['GET', '/api/health'],
      ['GET', '/api/todos'],
      ['POST', '/api/todos'],
      ['PATCH', '/api/todos/1'],
    ],
  );
  assert.equal(seen[2].contentType, 'application/json');
  assert.equal(JSON.parse(seen[2].body).title, '  新任务  ');
  assert.equal(JSON.parse(seen[3].body).completed, true);
});

test('returns safe JSON 502 when upstream is unavailable', async (t) => {
  // Reserve a port, then close it so nothing is listening there.
  const dead = http.createServer();
  const deadPort = await listen(dead);
  await closeServer(dead);

  const server = createFrontendServer({ upstream: `http://${HOST}:${deadPort}` });
  const port = await listen(server);
  t.after(() => closeServer(server));

  const res = await rawRequest(port, { path: '/api/health' });
  assert.equal(res.status, 502);
  assert.match(res.headers['content-type'], /application\/json/);
  const parsed = JSON.parse(res.body);
  assert.equal(typeof parsed.error, 'string');
  assert.ok(parsed.error.length > 0);
});
