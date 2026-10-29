Implement the browser client of a real cross-machine Todo application. You are DSH on Windows. The backend is independently authored by Pi on Orion/Linux. Read api-contract.json first; it is the shared source of truth. Work ONLY in this registered project. Use read/write/edit tools; do not execute commands, install dependencies, start processes, access credentials or unrelated paths. A controlled runner will run the service, tests and browser acceptance.

Create these deliverables, leaving api-contract.json unchanged:
- index.html, styles.css, app.js: a polished but simple Chinese Todo page, no framework or CDN.
- serve.mjs: a Node.js ESM static server and /api proxy using built-in modules only.
- client.test.mjs: node:test HTTP tests of static serving, safe path handling, /api proxying to a temporary mock upstream, and upstream failure; clean up all test servers.
- package.json: private ESM package, scripts start=node serve.mjs and test=node --test client.test.mjs, NO dependencies.
- README.md: concise run/test instructions.

The UI needs an explicit label for the new task input (label text "新待办"), a button "添加", a button "刷新", task checkboxes with useful accessible names, completed styling, task counts, loading/empty/error states, and visible live API connection status showing Orion when health succeeds. Use /api/health and /api/todos exactly as in the contract. Fetch the list on startup. POST a title, PATCH a todo's completed boolean, display the saved server results, and reload server data when refreshing. Disable controls during relevant requests, handle failed requests visibly, and render user titles with textContent rather than interpolated HTML. No fake/localStorage fallback that hides connection failure.

Design: a clean light page branded "Mesh Tasks", max-width about 760px, subtle blue/slate accents, generous spacing, clear focus states and responsive layout. Keep the primary heading "待办事项". Small provenance text may say client on Windows / API on Orion. Use CSS or inline SVG only, no external assets.

serve.mjs binds only 127.0.0.1. PORT defaults to 5173; UPSTREAM defaults to http://127.0.0.1:18080 (the SSH forwarded Orion API). Proxy /api/* to this configured origin without rewriting API paths; preserve HTTP method, JSON body, response status/content type. Bound proxy timeout and return safe JSON 502 on disconnect. Serve only the declared static assets; reject traversal, hidden files and unrelated filesystem access. Export createFrontendServer({upstream}={}) returning an unstarted http.Server for tests; listen only as the direct entry point, using a check that works on Windows and Linux. No model calls in the application itself.

Read completed files back. End with a short summary and do not claim the tests passed before the runner executes them.
