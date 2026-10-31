import { randomUUID } from 'node:crypto';
import { assert, jsonRequest } from './common.mjs';

export class TodoProbe {
  constructor(base, health, run) { this.base = base; this.health = health; this.run = run; }
  async healthCheck(expected = this.health) {
    const r = await jsonRequest(this.base, '/health');
    assert(r.status === 200, 'Health status must be 200');
    for (const [key, value] of Object.entries(expected)) assert(r.json?.[key] === value, `Health field ${key} mismatch`);
    return { passed: true };
  }
  async list() { const r = await jsonRequest(this.base, '/todos'); assert(r.status === 200 && Array.isArray(r.json?.todos), 'Invalid todo list'); return r.json.todos; }
  async seed() {
    const title = `hmesh-acceptance-${this.run}-${randomUUID()}`;
    const r = await jsonRequest(this.base, '/todos', { method: 'POST', body: { title } });
    assert(r.status === 201 && typeof r.json?.id === 'string' && r.json.id.length > 0 && r.json.title === title && r.json.completed === false, 'Create contract failed');
    return r.json;
  }
  async verify(todo) {
    assert((await this.list()).some(t => t.id === todo.id && t.title === todo.title && t.completed === todo.completed), 'Persisted item mismatch');
    return { passed: true, id: todo.id };
  }
  async remove(todo) {
    const rows = await this.list();
    const found = rows.find(t => t.id === todo.id);
    if (!found) return { passed: true, already_absent: true };
    assert(found.title === todo.title && todo.title.startsWith(`hmesh-acceptance-${this.run}-`), 'Refuse deletion of unrelated data');
    const r = await jsonRequest(this.base, `/todos/${encodeURIComponent(todo.id)}`, { method: 'DELETE' });
    assert(r.status === 204 && !(await this.list()).some(t => t.id === todo.id), 'Delete contract failed');
    return { passed: true };
  }
  async crud() {
    await this.healthCheck(); const todo = await this.seed();
    // On failure preserve the item for diagnosis; do not guess whether a timed-out write completed.
    await this.verify(todo);
    const r = await jsonRequest(this.base, `/todos/${encodeURIComponent(todo.id)}`, { method: 'PATCH', body: { completed: true } });
    assert(r.status === 200 && r.json?.id === todo.id && r.json.title === todo.title && r.json.completed === true, 'Patch contract failed');
    await this.verify({ ...todo, completed: true }); await this.remove(todo);
    return { passed: true, checks: 8 };
  }
  async revoked() {
    try {
      const r = await jsonRequest(this.base, '/health', { timeout: 10000 });
      assert([403, 409, 410, 503].includes(r.status), 'Revoked endpoint still accessible or unexpected response');
      return { passed: true, denied_status: r.status };
    } catch (error) {
      // A deadline or malformed response is not proof of revocation.
      if (error.cause?.code === 'ECONNREFUSED') return { passed: true, connection_refused: true };
      throw error;
    }
  }
}
