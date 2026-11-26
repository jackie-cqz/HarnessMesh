'use strict';

const healthUrl = '/api/health';
const todosUrl = '/api/todos';

const form = document.getElementById('todo-form');
const input = document.getElementById('todo-input');
const addButton = document.getElementById('add-button');
const refreshButton = document.getElementById('refresh-button');
const list = document.getElementById('todo-list');
const counts = document.getElementById('counts');
const banner = document.getElementById('state-banner');
const statusEl = document.getElementById('connection-status');
const statusText = document.querySelector('#connection-status .status-text');

let todos = [];
let busy = 0;

function setConnection(state, text) {
  statusEl.dataset.state = state;
  statusText.textContent = text;
}

async function apiFetch(url, options = {}) {
  let response;
  try {
    response = await fetch(url, options);
  } catch {
    throw new Error('网络错误，无法连接 API');
  }

  const text = await response.text();
  let data = null;
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      data = null;
    }
  }

  if (!response.ok) {
    const message =
      data && typeof data.error === 'string' && data.error
        ? data.error
        : `请求失败（HTTP ${response.status}）`;
    const error = new Error(message);
    error.status = response.status;
    throw error;
  }

  return data;
}

async function checkHealth() {
  try {
    const data = await apiFetch(healthUrl, {
      method: 'GET',
      headers: { Accept: 'application/json' },
    });
    if (data && data.status === 'ok') {
      const service = typeof data.service === 'string' ? data.service : 'linux-todo-api';
      setConnection('online', `Linux 在线 · ${service}`);
    } else {
      setConnection('offline', 'Linux 离线');
    }
  } catch {
    setConnection('offline', 'Linux 离线');
  }
}

function beginBusy() {
  busy += 1;
  updateControls();
}

function endBusy() {
  busy = Math.max(0, busy - 1);
  updateControls();
}

function updateControls() {
  const disabled = busy > 0;
  input.disabled = disabled;
  addButton.disabled = disabled;
  refreshButton.disabled = disabled;
  list.querySelectorAll('input[type="checkbox"]').forEach((checkbox) => {
    checkbox.disabled = disabled;
  });
}

function showBanner(kind, message) {
  banner.dataset.kind = kind;
  banner.textContent = message;
  banner.hidden = false;
}

function hideBanner() {
  banner.textContent = '';
  banner.hidden = true;
}

function formatDate(value) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return String(value || '');
  const pad = (n) => String(n).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

function renderCounts() {
  const total = todos.length;
  const completed = todos.filter((todo) => todo.completed).length;
  counts.textContent = `共 ${total} 项 · 已完成 ${completed} 项`;
}

function createTodoItem(todo) {
  const li = document.createElement('li');
  li.className = 'todo-item';
  if (todo.completed) li.classList.add('completed');

  const label = document.createElement('label');
  label.className = 'todo-label';

  const checkbox = document.createElement('input');
  checkbox.type = 'checkbox';
  checkbox.checked = Boolean(todo.completed);
  checkbox.disabled = busy > 0;

  const title = typeof todo.title === 'string' ? todo.title : '';
  checkbox.setAttribute('aria-label', `${todo.completed ? '取消完成' : '标记完成'}：${title}`);
  checkbox.addEventListener('change', () => {
    handleToggle(todo, checkbox);
  });

  const titleSpan = document.createElement('span');
  titleSpan.className = 'todo-title';
  titleSpan.textContent = title;

  label.append(checkbox, titleSpan);
  li.appendChild(label);

  if (todo.created_at) {
    const meta = document.createElement('span');
    meta.className = 'todo-meta';
    const time = document.createElement('time');
    time.dateTime = String(todo.created_at);
    time.textContent = formatDate(todo.created_at);
    meta.appendChild(time);
    li.appendChild(meta);
  }

  return li;
}

function render() {
  list.textContent = '';
  for (const todo of todos) {
    list.appendChild(createTodoItem(todo));
  }
  renderCounts();
  updateControls();
}

async function loadTodos() {
  beginBusy();
  showBanner('loading', '正在加载待办…');
  try {
    const data = await apiFetch(todosUrl, {
      method: 'GET',
      headers: { Accept: 'application/json' },
    });
    todos = data && Array.isArray(data.todos) ? data.todos : [];
    render();
    if (todos.length === 0) {
      showBanner('empty', '暂无待办，添加一个吧');
    } else {
      hideBanner();
    }
  } catch (err) {
    render();
    showBanner('error', `加载失败：${err.message}`);
    checkHealth();
  } finally {
    endBusy();
  }
}

async function handleAdd() {
  const title = input.value.trim();
  if (!title) {
    showBanner('error', '请输入待办内容');
    input.focus();
    return;
  }
  if (title.length > 200) {
    showBanner('error', '内容不能超过 200 个字符');
    input.focus();
    return;
  }

  beginBusy();
  try {
    const data = await apiFetch(todosUrl, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
      body: JSON.stringify({ title }),
    });
    const created = data && data.todo ? data.todo : data;
    if (created) {
      todos.push(created);
    }
    input.value = '';
    render();
    showBanner('info', '已添加');
  } catch (err) {
    render();
    showBanner('error', `添加失败：${err.message}`);
    checkHealth();
  } finally {
    endBusy();
    input.focus();
  }
}

async function handleToggle(todo, checkbox) {
  const next = checkbox.checked;
  beginBusy();
  try {
    const data = await apiFetch(`${todosUrl}/${encodeURIComponent(todo.id)}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
      body: JSON.stringify({ completed: next }),
    });
    const updated = data && data.todo ? data.todo : { ...todo, completed: next };
    const index = todos.findIndex((item) => item.id === todo.id);
    if (index !== -1) {
      todos[index] = updated;
    }
    render();
  } catch (err) {
    render();
    showBanner('error', `更新失败：${err.message}`);
    checkHealth();
  } finally {
    endBusy();
  }
}

form.addEventListener('submit', (event) => {
  event.preventDefault();
  handleAdd();
});

refreshButton.addEventListener('click', () => {
  checkHealth();
  loadTodos();
});

checkHealth();
loadTodos();

setInterval(() => {
  checkHealth();
}, 15000);
