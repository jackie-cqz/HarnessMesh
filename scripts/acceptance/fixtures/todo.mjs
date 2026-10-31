// Synthetic test backend, never real Harness authoring evidence.
import http from 'node:http';
import { readFileSync, writeFileSync, renameSync, existsSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
const file = new URL('data.json', import.meta.url);
let todos = existsSync(file) ? JSON.parse(readFileSync(file)).todos : [];
function save() { const tmp = new URL('data.tmp', import.meta.url); writeFileSync(tmp, JSON.stringify({ todos })); renameSync(tmp, file); }
http.createServer(async (req, res) => {
  const reply = (status, json) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(json === undefined ? undefined : JSON.stringify(json)); };
  try {
    if (req.method === 'GET' && req.url === '/health') return reply(200, { status: 'ok', service: 'tailnet-todo', node: 'mac' });
    if (req.method === 'GET' && req.url === '/todos') return reply(200, { todos });
    let raw = ''; for await (const chunk of req) { raw += chunk; if (raw.length > 65536) return reply(413, {}); }
    const body = raw ? JSON.parse(raw) : {};
    if (req.method === 'POST' && req.url === '/todos') {
      if (typeof body.title !== 'string' || !body.title.trim()) return reply(400, {});
      const todo = { id: randomUUID(), title: body.title, completed: false }; todos.push(todo); save(); return reply(201, todo);
    }
    const id = decodeURIComponent(req.url.slice('/todos/'.length)); const todo = todos.find(t => t.id === id);
    if (!req.url.startsWith('/todos/') || !todo) return reply(404, {});
    if (req.method === 'PATCH' && typeof body.completed === 'boolean') { todo.completed = body.completed; save(); return reply(200, todo); }
    if (req.method === 'DELETE') { todos = todos.filter(t => t.id !== id); save(); return reply(204); }
    reply(400, {});
  } catch { reply(400, {}); }
}).listen(Number(process.env.PORT), '127.0.0.1');
