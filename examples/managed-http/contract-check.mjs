import assert from "node:assert/strict";

const baseUrl = process.argv[2];
assert.ok(baseUrl, "Usage: node contract-check.mjs <base-url>");

async function request(path, options = {}) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 45_000);
  try {
    const res = await fetch(new URL(path, baseUrl).toString(), {
      ...options,
      signal: controller.signal,
    });
    let body = null;
    const text = await res.text();
    if (text) {
      try {
        body = JSON.parse(text);
      } catch {
        body = text;
      }
    }
    return { status: res.status, body };
  } finally {
    clearTimeout(timer);
  }
}

// 1. Health check
const health = await request("/api/health");
assert.equal(health.status, 200, "GET /api/health must return 200");
assert.equal(health.body?.status, "ok");
assert.equal(health.body?.service, "orion-todo-api");
assert.equal(health.body?.node, "Orion");

// 2. Create a unique todo
const title = `contract-check-${Date.now()}-${Math.random().toString(36).slice(2)}`;
const created = await request("/api/todos", {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ title }),
});
assert.equal(created.status, 201, "POST /api/todos must return 201");
assert.ok(created.body?.todo, "POST /api/todos must return a todo");
assert.equal(created.body.todo.title, title);
const id = created.body.todo.id;
assert.ok(id, "created todo must have an id");

// 3. PATCH completed=true
const patched = await request(`/api/todos/${encodeURIComponent(id)}`, {
  method: "PATCH",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ completed: true }),
});
assert.equal(patched.status, 200, "PATCH /api/todos/:id must return 200");
assert.equal(patched.body?.todo?.completed, true);

// 4. GET list and assert record present and completed
const list = await request("/api/todos");
assert.equal(list.status, 200, "GET /api/todos must return 200");
const todos = list.body?.todos;
assert.ok(Array.isArray(todos), "GET /api/todos must return a todos array");
const found = todos.find((t) => t.id === id);
assert.ok(found, "created todo must be present in the list");
assert.equal(found.title, title);
assert.equal(found.completed, true);

console.log(JSON.stringify({ passed: true, checks: 4 }));
