# Linux Todo API

Small Todo HTTP API built with Node.js built-in modules only (no dependencies).

## Requirements

- Node.js 18 or newer (uses global `fetch` in tests).

## Run

From this directory:

```sh
npm start
```

Equivalent to `node server.mjs`. The server listens on `127.0.0.1:8000` by default.

Environment variables:

| Variable     | Default                        | Meaning                                  |
| ------------ | ------------------------------ | ---------------------------------------- |
| `PORT`       | `8000`                         | Listen port; `0` selects a free port.    |
| `DATA_FILE`  | `data/todos.json` (this dir)   | JSON file used for persistence.          |
| `NODE_LABEL` | `Linux`                        | Value reported by `GET /api/health`.     |

Example:

```sh
PORT=18080 DATA_FILE=/tmp/todos.json NODE_LABEL=Linux npm start
```

## API

- `GET /api/health` → `{ "status": "ok", "service": "linux-todo-api", "node": "Linux" }`
- `GET /api/todos` → `{ "todos": [...] }`
- `POST /api/todos` with `{ "title": "Example" }` → `201 { "todo": {...} }`
- `PATCH /api/todos/:id` with `{ "completed": true }` → `200 { "todo": {...} }`

Todo objects: `{ id, title, completed, created_at }`. Titles are trimmed and must be 1–200 characters. Errors are JSON `{ "error": "..." }` with status 400 (invalid input), 404 (unknown todo or route), 413 (body over 64 KiB) or 500 (internal error). The frontend is expected to proxy `/api`, so no CORS headers are sent.

Todo data is written atomically (temp file + rename) before a mutation is acknowledged, and concurrent mutations are serialized. An existing corrupt `DATA_FILE` is reported as an error and never silently overwritten.

## Test

```sh
npm test
```

Equivalent to `node --test server.test.mjs`. Tests use isolated temporary data files, ephemeral ports and only the loopback interface; every test server is closed afterwards.
