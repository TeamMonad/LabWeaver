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

```sh
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
| `pnpm build:fixtures` | 构建本地 Fixture 预览 |
| `pnpm preview` | 预览生产构建 |
| `pnpm preview:fixtures` | 启动本地 Fixture 预览 |
| `pnpm lint` | ESLint 检查并自动修复 |
| `pnpm test` | 运行单元测试 |
| `pnpm typecheck` | TypeScript 类型检查 |
| `pnpm typecheck:fixtures` | Fixture Vue 页面类型检查 |
| `pnpm exec tsx scripts/screenshot-roles.ts` | 生成四角色导航截图 |

## Fixture UI 预览

从仓库根目录运行以下命令：

```sh
pnpm --dir web typecheck:fixtures
pnpm --dir web build:fixtures
pnpm --dir web preview:fixtures
node web/fixtures/gallery.mjs artifacts/ux-preview
```

Fixture 预览地址为 `http://127.0.0.1:4174/fixture-preview.html`。构建输出写入 `artifacts/fixture-build`，不会覆盖生产构建的 `web/dist`。场景选择器包含 21 个页面场景，适合检查不同角色、状态和空数据布局。Fixture 只使用本地预览数据；登录、提交、批准、续期、撤销等写操作明确标记为不支持，不会调用真实后端。

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
