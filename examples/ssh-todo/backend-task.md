Implement the backend of a small real cross-machine Todo web application. You are Pi running on Linux. The frontend is independently authored by DSH on Windows. Read api-contract.json first; it is the shared source of truth. Work ONLY in this registered project. Use read/write/edit tools; do not run commands, start services, install packages, access credentials or unrelated paths. A controlled runner will execute your tests and service after this task.

Create exactly these deliverables (api-contract.json already exists and must stay unchanged):
- server.mjs: executable Node.js ESM HTTP API using only Node built-in modules.
- server.test.mjs: meaningful node:test tests for real HTTP create/list/complete, validation, JSON persistence after restarting a server, and unknown IDs. Use isolated temporary data and ephemeral ports, close every test server. No external network.
- package.json: private ESM package; scripts start=node server.mjs and test=node --test server.test.mjs; NO dependencies.
- README.md: concise runtime and test instructions.

Implement the exact routes and response shapes in api-contract.json. Listen only on 127.0.0.1, use PORT (default 8000, allow 0 in tests), DATA_FILE (default a data/todos.json beneath this project), and NODE_LABEL (default Linux). GET /api/health must return status=ok, service=linux-todo-api, node=Linux by default. Expose an exported createTodoServer({dataFile,nodeLabel}={}) factory returning an http.Server without listening so tests can start/close it, and listen only when this module is the executable entry point. Direct-entry detection must work on Linux and Windows.

Todos have id, title, completed, created_at. Use crypto.randomUUID, ISO timestamps, validate strings and boolean values, trim titles, cap title at 200 characters and request bodies at 64 KiB. Persist mutation before returning success. Use atomic write/rename and serialize mutations to avoid concurrent lost writes. Do not overwrite a corrupt existing data file silently. Errors are JSON with a safe string error; no stack traces. Unknown routes/IDs return 404. Normal browser JSON requests need no CORS because the frontend will proxy /api. Do not hard-code mock responses for write/read/list. No model calls in the application itself.

Read your completed code back for consistency. End with a short summary of the files and state clearly that the runner, not you, must execute the tests. Do not claim tests passed unless you actually observed them.
