import http from 'node:http';
import https from 'node:https';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const DEFAULT_PORT = 5173;
const DEFAULT_UPSTREAM = 'http://127.0.0.1:18080';
const PROXY_TIMEOUT_MS = 10_000;
const MAX_BODY_BYTES = 1_000_000;

// Only the declared front-end assets are served. Anything else (including
// files that happen to live in the same directory) is rejected with a 404.
const STATIC_FILES = new Map([
  ['/', { file: 'index.html', type: 'text/html; charset=utf-8' }],
  ['/index.html', { file: 'index.html', type: 'text/html; charset=utf-8' }],
  ['/styles.css', { file: 'styles.css', type: 'text/css; charset=utf-8' }],
  ['/app.js', { file: 'app.js', type: 'text/javascript; charset=utf-8' }],
]);

function sendJson(res, status, payload) {
  const body = JSON.stringify(payload);
  res.writeHead(status, {
    'content-type': 'application/json; charset=utf-8',
    'content-length': Buffer.byteLength(body),
  });
  res.end(body);
}

// Reject path traversal, Windows-style separators, NUL bytes and hidden files.
function isSafeStaticPath(pathname) {
  if (typeof pathname !== 'string') return false;
  if (pathname.includes('\\') || pathname.includes('\0') || pathname.includes('..')) {
    return false;
  }
  for (const segment of pathname.split('/')) {
    if (segment.startsWith('.')) return false;
  }
  return true;
}

function serveStatic(req, res, entry) {
  const filePath = path.join(__dirname, entry.file);
  fs.readFile(filePath, (err, data) => {
    if (err) {
      sendJson(res, 404, { error: 'Not found' });
      return;
    }
    const headers = {
      'content-type': entry.type,
      'content-length': data.length,
      'cache-control': 'no-store',
    };
    res.writeHead(200, headers);
    if (req.method === 'HEAD') {
      res.end();
    } else {
      res.end(data);
    }
  });
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    let settled = false;

    req.on('data', (chunk) => {
      if (settled) return;
      size += chunk.length;
      if (size > MAX_BODY_BYTES) {
        settled = true;
        const err = new Error('Request body too large');
        err.statusCode = 413;
        req.destroy();
        reject(err);
        return;
      }
      chunks.push(chunk);
    });

    req.on('end', () => {
      if (!settled) {
        settled = true;
        resolve(Buffer.concat(chunks));
      }
    });

    req.on('error', (err) => {
      if (!settled) {
        settled = true;
        reject(err);
      }
    });
  });
}

function proxyToUpstream(req, res, upstream, parsed) {
  const declaredLength = Number.parseInt(req.headers['content-length'], 10);
  if (Number.isFinite(declaredLength) && declaredLength > MAX_BODY_BYTES) {
    sendJson(res, 413, { error: 'Request body too large' });
    return;
  }

  readBody(req)
    .then((body) => {
      let target;
      try {
        // Use the configured origin only and append the original API path
        // unchanged, so /api/* is never rewritten.
        const base = new URL(upstream);
        target = new URL(base.origin + parsed.pathname + parsed.search);
      } catch {
        sendJson(res, 400, { error: 'Bad request' });
        return;
      }

      const lib = target.protocol === 'https:' ? https : http;
      const headers = {};
      for (const [key, value] of Object.entries(req.headers)) {
        const lower = key.toLowerCase();
        if (
          lower === 'host' ||
          lower === 'connection' ||
          lower === 'content-length' ||
          lower === 'transfer-encoding' ||
          lower === 'accept-encoding'
        ) {
          continue;
        }
        headers[key] = value;
      }
      headers['content-length'] = String(body.length);

      const upstreamReq = lib.request(
        {
          hostname: target.hostname,
          port: target.port || (target.protocol === 'https:' ? 443 : 80),
          path: target.pathname + target.search,
          method: req.method,
          headers,
        },
        (upstreamRes) => {
          const resHeaders = { ...upstreamRes.headers };
          delete resHeaders.connection;
          delete resHeaders['keep-alive'];
          delete resHeaders['transfer-encoding'];
          delete resHeaders['content-encoding'];
          res.writeHead(upstreamRes.statusCode, resHeaders);
          upstreamRes.pipe(res);
        },
      );

      upstreamReq.setTimeout(PROXY_TIMEOUT_MS, () => {
        upstreamReq.destroy(new Error('Upstream timed out'));
      });

      upstreamReq.on('error', () => {
        if (res.headersSent) {
          res.destroy();
          return;
        }
        sendJson(res, 502, { error: 'API 网关无法连接上游服务' });
      });

      upstreamReq.end(body);
    })
    .catch((err) => {
      if (res.headersSent) return;
      sendJson(res, err.statusCode || 500, { error: err.message || 'Bad request' });
    });
}

function handleRequest(req, res, upstream) {
  let parsed;
  try {
    const rawUrl = req.url || '/';
    if (rawUrl.includes('\0')) {
      sendJson(res, 400, { error: 'Bad request' });
      return;
    }
    parsed = new URL(rawUrl, 'http://127.0.0.1');
  } catch {
    sendJson(res, 400, { error: 'Bad request' });
    return;
  }

  let pathname;
  try {
    pathname = decodeURIComponent(parsed.pathname);
  } catch {
    sendJson(res, 400, { error: 'Bad request' });
    return;
  }

  if (pathname === '/api' || pathname.startsWith('/api/')) {
    const allowed = ['GET', 'POST', 'PATCH', 'PUT', 'DELETE'];
    if (!allowed.includes(req.method)) {
      res.setHeader('Allow', allowed.join(', '));
      sendJson(res, 405, { error: 'Method not allowed' });
      return;
    }
    proxyToUpstream(req, res, upstream, parsed);
    return;
  }

  if (req.method !== 'GET' && req.method !== 'HEAD') {
    res.setHeader('Allow', 'GET, HEAD');
    sendJson(res, 405, { error: 'Method not allowed' });
    return;
  }

  if (!isSafeStaticPath(pathname)) {
    sendJson(res, 404, { error: 'Not found' });
    return;
  }

  const entry = STATIC_FILES.get(pathname);
  if (!entry) {
    sendJson(res, 404, { error: 'Not found' });
    return;
  }

  serveStatic(req, res, entry);
}

export function createFrontendServer({ upstream = DEFAULT_UPSTREAM } = {}) {
  return http.createServer((req, res) => {
    handleRequest(req, res, upstream);
  });
}

function isDirectRun() {
  if (!process.argv[1]) return false;
  try {
    const modulePath = path.resolve(fileURLToPath(import.meta.url));
    const mainPath = path.resolve(process.argv[1]);
    if (process.platform === 'win32') {
      return modulePath.toLowerCase() === mainPath.toLowerCase();
    }
    return modulePath === mainPath;
  } catch {
    return false;
  }
}

// Only bind a port when this file is the entry point, which lets the test
// suite import createFrontendServer without starting a listener.
if (isDirectRun()) {
  const rawPort = process.env.PORT;
  let port = DEFAULT_PORT;
  if (rawPort !== undefined && rawPort !== '') {
    const parsedPort = Number(rawPort);
    if (Number.isInteger(parsedPort) && parsedPort >= 0 && parsedPort <= 65535) {
      port = parsedPort;
    }
  }
  const upstream = process.env.UPSTREAM || DEFAULT_UPSTREAM;

  const server = createFrontendServer({ upstream });
  server.listen(port, '127.0.0.1', () => {
    const address = server.address();
    console.log(`Mesh Tasks frontend listening on http://127.0.0.1:${address.port}`);
    console.log(`Proxying /api/* -> ${upstream}`);
  });
  server.on('error', (err) => {
    console.error('Frontend server error:', err);
    process.exitCode = 1;
  });
}
