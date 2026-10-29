# Mesh Tasks · SSH Todo 客户端

HarnessMesh SSH Todo 示例的浏览器客户端，以及本地静态资源 / API 代理服务器。
纯 Node.js 内置模块实现，无第三方依赖、无 CDN。

## 运行

要求：Node.js 18+。

```bash
npm start
```

- 前端监听 `http://127.0.0.1:5173`（可用环境变量 `PORT` 覆盖）。
- `/api/*` 原样代理到 `UPSTREAM`，默认 `http://127.0.0.1:18080`（SSH 转发的 Orion API）。

例如：

```bash
PORT=8080 UPSTREAM=http://127.0.0.1:18080 npm start
```

## 测试

```bash
npm test
```

等价于 `node --test client.test.mjs`，覆盖：

- 静态资源服务（HTML / CSS / JS、HEAD、未知路径、非 GET 方法）。
- 安全路径处理（目录穿越、隐藏文件、目录内无关文件一律 404）。
- `/api/*` 代理到临时模拟上游（方法、路径、JSON 请求体、状态码与内容类型原样透传）。
- 上游不可用（断连）时返回安全的 JSON 502。
