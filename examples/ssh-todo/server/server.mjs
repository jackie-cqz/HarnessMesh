#!/usr/bin/env node
// Linux Todo API - Node.js built-ins only.
import http from 'node:http';
import { promises as fsp } from 'node:fs';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';

const HOST = '127.0.0.1';
const DEFAULT_PORT = 8000;
const DEFAULT_NODE_LABEL = 'Linux';
const SERVICE_NAME = 'linux-todo-api';
const MAX_TITLE_LENGTH = 200;
const MAX_BODY_BYTES = 64 * 1024;

const MODULE_FILE = fileURLToPath(import.meta.url);
const PROJECT_DIR = path.dirname(MODULE_FILE);

class ApiError extends Error {
  constructor(status, message) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
  }
}

class CorruptDataError extends Error {
  constructor(message) {
    super(message);
    this.name = 'CorruptDataError';
  }
}

function isPlainObject(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function normalizeTitle(value) {
  if (typeof value !== 'string') {
    throw new ApiError(400, 'title must be a string');
  }
  const title = value.trim();
  if (title.length === 0) {
    throw new ApiError(400, 'title must not be empty');
  }
  if (title.length > MAX_TITLE_LENGTH) {
    throw new ApiError(400, `title must be at most ${MAX_TITLE_LENGTH} characters`);
  }
  return title;
}

function isTodo(value) {
  return (
    isPlainObject(value) &&
    typeof value.id === 'string' &&
    value.id.length > 0 &&
    typeof value.title === 'string' &&
    typeof value.completed === 'boolean' &&
    typeof value.created_at === 'string'
  );
}

function createStore(dataFile) {
  let state = null;
  let queue = Promise.resolve();

  async function load() {
    if (state !== null) return state;
    let raw;
    try {
      raw = await fsp.readFile(dataFile, 'utf8');
    } catch (err) {
      if (err && err.code === 'ENOENT') {
        state = [];
        return state;
      }
      throw err;
    }

    let parsed;
    try {
      parsed = JSON.parse(raw);
    } catch {
      throw new CorruptDataError('data file is not valid JSON');
    }

    let todos;
    if (Array.isArray(parsed)) {
      todos = parsed;
    } else if (isPlainObject(parsed) && Array.isArray(parsed.todos)) {
      todos = parsed.todos;
    } else {
      throw new CorruptDataError('data file has an unsupported shape');
    }

    if (!todos.every(isTodo)) {
      throw new CorruptDataError('data file contains invalid todo records');
    }

    state = todos.map((todo) => ({ ...todo }));
    return state;
  }

  async function writeAtomic(todos) {
    const dir = path.dirname(dataFile);
    await fsp.mkdir(dir, { recursive: true });
    const tmpFile = path.join(
      dir,
      `.${path.basename(dataFile)}.${process.pid}.${crypto.randomUUID()}.tmp`,
    );
    const payload = `${JSON.stringify({ todos }, null, 2)}\n`;
    let handle = null;
    try {
      handle = await fsp.open(tmpFile, 'wx', 0o600);
      await handle.writeFile(payload, 'utf8');
      await handle.sync();
      await handle.close();
      handle = null;
      await fsp.rename(tmpFile, dataFile);
    } finally {
      if (handle) {
        await handle.close().catch(() => {});
      }
      await fsp.unlink(tmpFile).catch(() => {});
    }
  }

  function enqueue(task) {
    const run = queue.then(task, task);
    queue = run.then(
      () => undefined,
      () => undefined,
    );
    return run;
  }

  function mutate(fn) {
    return enqueue(async () => {
      const current = await load();
      const draft = current.map((todo) => ({ ...todo }));
      const result = fn(draft);
      await writeAtomic(draft);
      state = draft;
      return result;
    });
  }

  return {
    async read() {
      const todos = await load();
      return todos.map((todo) => ({ ...todo }));
    },
    async create(title, completed) {
      return mutate((todos) => {
        const todo = {
          id: crypto.randomUUID(),
          title,
          completed,
          created_at: new Date().toISOString(),
        };
        todos.push(todo);
        return { ...todo };
      });
    },
    async setCompleted(id, completed) {
      return mutate((todos) => {
        const index = todos.findIndex((todo) => todo.id === id);
        if (index === -1) {
          throw new ApiError(404, 'todo not found');
        }
        todos[index] = { ...todos[index], completed };
        return { ...todos[index] };
      });
    },
  };
}

async function readJsonBody(req) {
  const chunks = [];
  let size = 0;
  let tooLarge = false;

  for await (const chunk of req) {
    const buf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    size += buf.length;
    if (size > MAX_BODY_BYTES) {
      tooLarge = true;
      chunks.length = 0;
      continue;
    }
    if (!tooLarge) chunks.push(buf);
  }

  if (tooLarge) {
    throw new ApiError(413, 'request body exceeds the 64 KiB limit');
  }

  const raw = Buffer.concat(chunks).toString('utf8');
  if (raw.trim() === '') return {};
  try {
    return JSON.parse(raw);
  } catch {
    throw new ApiError(400, 'request body must be valid JSON');
  }
}

function sendJson(res, status, payload) {
  const body = JSON.stringify(payload);
  res.writeHead(status, {
    'Content-Type': 'application/json; charset=utf-8',
    'Content-Length': Buffer.byteLength(body),
    'Cache-Control': 'no-store',
    'X-Content-Type-Options': 'nosniff',
  });
  res.end(body);
}

function sendError(res, status, message) {
  if (res.headersSent) {
    res.destroy();
    return;
  }
  sendJson(res, status, { error: message });
}

function parsePort(raw) {
  if (raw === undefined || raw === null || String(raw).trim() === '') return DEFAULT_PORT;
  const value = Number(raw);
  if (!Number.isInteger(value) || value < 0 || value > 65535) {
    throw new Error(`PORT must be an integer between 0 and 65535, received: ${raw}`);
  }
  return value;
}

export function createTodoServer({ dataFile, nodeLabel } = {}) {
  const resolvedDataFile = path.resolve(
    dataFile ?? process.env.DATA_FILE ?? path.join(PROJECT_DIR, 'data', 'todos.json'),
  );
  const labelCandidate = nodeLabel ?? process.env.NODE_LABEL ?? DEFAULT_NODE_LABEL;
  const resolvedNodeLabel =
    typeof labelCandidate === 'string' && labelCandidate.trim() !== ''
      ? labelCandidate
      : DEFAULT_NODE_LABEL;

  const store = createStore(resolvedDataFile);

  return http.createServer(async (req, res) => {
    try {
      const method = req.method ?? 'GET';
      const url = new URL(req.url ?? '/', 'http://127.0.0.1');
      const pathname = url.pathname;

      if (pathname === '/api/health') {
        if (method !== 'GET') {
          sendError(res, 404, 'not found');
          return;
        }
        sendJson(res, 200, { status: 'ok', service: SERVICE_NAME, node: resolvedNodeLabel });
        return;
      }

      if (pathname === '/api/todos') {
        if (method === 'GET') {
          const todos = await store.read();
          sendJson(res, 200, { todos });
          return;
        }
        if (method === 'POST') {
          const body = await readJsonBody(req);
          if (!isPlainObject(body)) {
            throw new ApiError(400, 'request body must be a JSON object');
          }
          const title = normalizeTitle(body.title);
          const completed = body.completed === undefined ? false : body.completed;
          if (typeof completed !== 'boolean') {
            throw new ApiError(400, 'completed must be a boolean');
          }
          const todo = await store.create(title, completed);
          sendJson(res, 201, { todo });
          return;
        }
        sendError(res, 404, 'not found');
        return;
      }

      const match = /^\/api\/todos\/([^/]+)$/.exec(pathname);
      if (match) {
        if (method !== 'PATCH') {
          sendError(res, 404, 'not found');
          return;
        }
        let id;
        try {
          id = decodeURIComponent(match[1]);
        } catch {
          sendError(res, 404, 'not found');
          return;
        }
        const body = await readJsonBody(req);
        if (!isPlainObject(body)) {
          throw new ApiError(400, 'request body must be a JSON object');
        }
        if (typeof body.completed !== 'boolean') {
          throw new ApiError(400, 'completed must be a boolean');
        }
        const todo = await store.setCompleted(id, body.completed);
        sendJson(res, 200, { todo });
        return;
      }

      sendError(res, 404, 'not found');
    } catch (err) {
      if (err instanceof ApiError) {
        sendError(res, err.status, err.message);
        return;
      }
      if (err instanceof CorruptDataError) {
        console.error(`[linux-todo-api] refusing to use corrupt data file: ${err.message}`);
        sendError(res, 500, 'todo data is unavailable');
        return;
      }
      console.error(`[linux-todo-api] internal error: ${err && err.message ? err.message : err}`);
      sendError(res, 500, 'internal server error');
    }
  });
}

function currentEntryPoint() {
  if (!process.argv[1]) return false;
  const self = MODULE_FILE;
  const entry = process.argv[1];
  try {
    const a = fs.realpathSync(entry);
    const b = fs.realpathSync(self);
    return process.platform === 'win32' ? a.toLowerCase() === b.toLowerCase() : a === b;
  } catch {
    const a = path.resolve(entry);
    const b = path.resolve(self);
    return process.platform === 'win32' ? a.toLowerCase() === b.toLowerCase() : a === b;
  }
}

function startFromEntryPoint() {
  let port;
  try {
    port = parsePort(process.env.PORT);
    const server = createTodoServer();
    server.on('error', (err) => {
      console.error(`[linux-todo-api] failed to start: ${err && err.message ? err.message : err}`);
      process.exitCode = 1;
    });
    server.listen(port, HOST, () => {
      const address = server.address();
      const boundPort = address && typeof address === 'object' ? address.port : port;
      console.log(`[linux-todo-api] listening on http://${HOST}:${boundPort}`);
    });
    const shutdown = () => {
      server.close(() => process.exit(0));
    };
    process.on('SIGINT', shutdown);
    process.on('SIGTERM', shutdown);
  } catch (err) {
    console.error(`[linux-todo-api] startup error: ${err && err.message ? err.message : err}`);
    process.exitCode = 1;
  }
}

if (currentEntryPoint()) {
  startFromEntryPoint();
}

export default createTodoServer;
