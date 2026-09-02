# LabWeaver Web Frontend

基于 Vue 3 + TypeScript + Vite 的 LabWeaver 门户前端。

## 技术栈

- Vue 3（Composition API）
- TypeScript
- Vite
- Pinia（状态管理）
- Vue Router
- Vitest（单元测试）
- Playwright（E2E / 截图）
- ESLint（代码检查）

## 快速开始

```bash
cd web
pnpm install
pnpm dev
```

开发服务器默认运行在 http://localhost:5173。

## 常用命令

| 命令 | 说明 |
|---|---|
| `pnpm dev` | 启动开发服务器 |
| `pnpm build` | 生产构建 |
| `pnpm preview` | 预览生产构建 |
| `pnpm lint` | ESLint 检查并自动修复 |
| `pnpm test` | 运行单元测试 |
| `pnpm typecheck` | TypeScript 类型检查 |
| `pnpm exec tsx scripts/screenshot-roles.ts` | 生成四角色导航截图 |

## 环境变量

复制 `.env.example` 为 `.env.development` 或 `.env.production`，并根据实际部署填写：

- `VITE_API_BASE_URL`：后端 API 基础路径
- `VITE_OIDC_AUTHORITY`：Keycloak / OIDC Provider 地址
- `VITE_OIDC_CLIENT_ID`：OIDC Client ID
- `VITE_OIDC_REDIRECT_URI`：登录回调地址

## 目录结构

```
web/
├── src/
│   ├── api/           # API 客户端封装
│   ├── components/    # Vue 组件
│   ├── composables/   # 组合式函数
│   ├── config/        # 运行时配置
│   ├── router/        # 路由配置
│   ├── stores/        # Pinia 状态
│   └── views/         # 页面视图
├── tests/             # 单元测试
├── scripts/           # 工具脚本
└── screenshots/       # 生成的截图证据
```

## 角色与页面

当前已实现四角色入口导航：

- 教师 `/teacher`
- 学生 `/student`
- 科研用户 `/researcher`
- 管理员 `/admin`

## 开发代理

开发环境下，`/api` 和 `/auth` 请求会通过 Vite proxy 转发到 `http://localhost:8080`。可通过环境变量 `VITE_API_PROXY_TARGET` 覆盖目标地址。
