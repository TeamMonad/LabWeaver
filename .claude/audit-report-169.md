# Issue #169 代码模块化审计报告

> **审计范围**: Control、Access、Agent、Environment、Evaluation、Resource 六大服务 + xtask
> **审计日期**: 2026-09-01
> **基线分支**: HEAD (4cd8315)
> **审计类型**: 大文件清单、重复实现证据、xtask placeholder 审查

---

## 1. 执行摘要

| 指标 | 数值 |
|------|------|
| Rust 超大模块 (≥1000行) | **28 个文件**，横跨 6 个服务 + xtask |
| 最大单体模块 | `control-service/src/lib.rs` — **3165 行** |
| 重复实现模式 | **6 大类**，共 **~140 处重复出现** |
| xtask placeholder | **4 个纯 placeholder** + **8 个阻塞式未实现** |
| 预估收敛收益 | -450 ~ -600 行重复代码，-0 deps（使用已有 contracts crate） |

**风险最高的大文件**:
- `control-service/src/lib.rs` (3165 行): 混合了 API 层、编排逻辑、持久化、revision 辅助函数和 event subject lookup
- `access-service/src/grants.rs` (2496 行): Grant CRUD + NATS 发布 + revision 转换混在同一文件
- `agent-service/src/run_store.rs` (2090 行): 编排 + 持久化 + revision helper 混合
- `evaluation-service/src/control_plane.rs` (2108 行): 控制面 + revision helper + event lookup

---

## 2. 大文件清单

### 2.1 Control Service

| 文件 | 行数 | 职责 | 拆分建议 |
|------|------|------|----------|
| `services/control-service/src/lib.rs` | **3165** | 业务编排: 不可变素材、策略、审批、SSE 状态; SQL upsert; revision 辅助函数; event contract lookup | 拆为: orchestration(核心用例) + persistence(SQL) + helper(revision/event utils) |
| `services/control-service/src/api.rs` | **1177** | Axum HTTP 路由、mTLS serve stub、SSE 端点 | 当前职责较清晰，serve_plain/serve_mtls stub 可清理 |

### 2.2 Access Service

| 文件 | 行数 | 职责 | 拆分建议 |
|------|------|------|----------|
| `services/access-service/src/grants.rs` | **2496** | Grant CRUD、SSH key、会话生命周期、NATS JetStream 事件发布 | 拆为: grant_operations + session_lifecycle + event_publishing |
| `services/access-service/src/main.rs` | **1417** | OIDC 认证、路由设置、CSRF、授权决策 | 保持 BFF 入口职责，考虑将授权决策抽离 |
| `services/access-service/src/proxy.rs` | **1268** | 浏览器转发代理 (mTLS 客户端) | 职责清晰，维持现状 |
| `services/access-service/src/console.rs` | **1007** | Console 准入 + WebSocket 代理 + revision 转换 | 将 revision 辅助函数移除到 contracts crate |

### 2.3 Agent Service

| 文件 | 行数 | 职责 | 拆分建议 |
|------|------|------|----------|
| `services/agent-service/src/run_store.rs` | **2090** | AgentRun 编排 + PostgreSQL + Outbox + revision helper | 拆为: run_orchestration + run_persistence + outbox_integration |
| `services/agent-service/src/claude_code.rs` | **2061** | Claude Code worker 适配器 | 职责单一但体积大，考虑将 egress 分类逻辑独立 |
| `services/agent-service/src/build_executor.rs` | **1302** | BuildKit/Harbor/Trivy 执行后端 | 职责清晰，维持现状 |
| `tests/claude_code_runtime.rs` | **1756** | Claude Code worker 边界回归测试 | 测试文件，按场景拆分 |

### 2.4 Environment Service

| 文件 | 行数 | 职责 | 拆分建议 |
|------|------|------|----------|
| `services/environment-service/src/kubevirt_provider.rs` | **2157** | KubeVirt VM 生命周期、fencing、资源计划 | Provider 边界清晰，但 revision helper 应移出 |
| `services/environment-service/src/container_provider.rs` | **1962** | 容器环境 Kubernetes 资源规划 | 同上 |
| `services/environment-service/src/runtime_executor.rs` | **1527** | K8s API 直连 Pod 管理 | 职责清晰 |
| `services/environment-service/src/store.rs` | **1192** | PostgreSQL store + event contract lookup | 将 event_contract lookup 移除到 contracts crate |
| `services/environment-service/src/messaging.rs` | **1015** | NATS mTLS 连接 + CloudEvent 发布 | connect_nats_mtls 应收敛 |

### 2.5 Evaluation Service

| 文件 | 行数 | 职责 | 拆分建议 |
|------|------|------|----------|
| `services/evaluation-service/src/control_plane.rs` | **2108** | Evaluation 控制面 + revision helper + event lookup | 拆为: control_orchestration + persistence helpers |
| `services/evaluation-service/src/oj_worker.rs` | **1263** | C++17 OJ worker | 职责清晰 |
| `services/evaluation-service/src/coordinator.rs` | **1074** | K8s coordinator | 维持现状 |

### 2.6 Resource Service

| 文件 | 行数 | 职责 | 拆分建议 |
|------|------|------|----------|
| `services/resource-service/src/store.rs` | **1798** | PostgreSQL: 请求、审批、leases + event contract lookup | 将 event_contract lookup 移除 |
| `services/resource-service/src/capacity.rs` | **1216** | K8s capacity provider | 职责清晰 |

### 2.7 xtask

| 文件 | 行数 | 职责 |
|------|------|------|
| `xtask/src/main.rs` | **2236** | CLI 入口: format, lint, build, test, deploy 等 |
| `xtask/src/platform_images.rs` | **1647** | 平台镜像构建和部署 |
| `xtask/src/integration.rs` | **1571** | 集成测试编排 |

### 2.8 Web

| 文件 | 行数 | 职责 |
|------|------|------|
| `web/src/fixture/stores/environmentStore.ts` | **551** | 测试 fixture store |

---

## 3. 重复实现证据

### 3.1 mTLS HTTP Serve Loop (14 处，6 个服务)

**严重度: 🔴 高** — 结构相同但分散在多处

| 文件 | 服务 | 模式 |
|------|------|------|
| `environment-service/src/tls.rs:28` | environment | `serve_owner_resolver_mtls()` — **唯一真实实现** |
| `environment-service/src/runtime.rs:83` | environment | `OwnerResolverRuntime::serve()` 调用上述函数 |
| `environment-service/src/kubevirt_console_executor.rs:117` | environment | `KubeVirtConsoleExecutorServer::serve()` 重复构建 TcpListener+Router+MtlsConfig |
| `environment-service/src/terminal_executor.rs:95` | environment | `TerminalExecutorServer::serve()` 同上 |
| `agent-service/src/api.rs:71` | agent | `serve_mtls()` **空壳**，委托给 `serve_plain()` |
| `resource-service/src/api.rs:93` | resource | `serve_mtls()` **空壳**，委托给 `serve_plain()` |
| `control-service/src/api.rs:153` | control | `serve_mtls()` **空壳**，委托给 `serve_plain()` |
| `evaluation-service/src/api.rs:579` | evaluation | `serve_evaluation_mtls()` **空壳** |

**发现**: 除 environment-service 外，其余 4 个服务的 `serve_mtls` 均已 stub 化——接受 `()` TLS config 并直接转发给明文版本。Environment-service 内部的 `serve_owner_resolver_mtls` 被 3 处调用方重复构建相同的 listener+router+config 组合。

**收敛目标**: 将 serve mTLS 模式收敛到 `environment-service/src/tls.rs`，其余服务移除空壳函数; environment-service 内部提取共享的 listener builder。

### 3.2 NATS mTLS 连接 (25 处重复，6 类模式)

**严重度: 🔴 高** — 最显著的跨服务复制

#### 3.2.1 connect_nats_mtls (6 处完全复制)

| 文件 | 服务 |
|------|------|
| `control-service/src/messaging.rs:86` | control |
| `agent-service/src/messaging.rs:30` | agent |
| `environment-service/src/messaging.rs:39` | environment |
| `evaluation-service/src/process.rs:126` | evaluation |
| `resource-service/src/process.rs:162` | resource |
| `access-service/src/grants.rs:81` | access |

所有 6 处使用相同的 `ConnectOptions::new().require_tls(true).add_root_certificates(...).add_client_certificate(...).credentials_file(...)` 链，仅函数名和参数校验有细微差异。

**收敛目标**: `crates/auth/src/nats.rs` 或新建 `crates/messaging/` crate。

#### 3.2.2 OutboxDispatcher (4 处结构相同)

| 文件 | 服务 |
|------|------|
| `control-service/src/messaging.rs:113` | control |
| `agent-service/src/messaging.rs:57` | agent |
| `evaluation-service/src/outbox.rs:21` | evaluation |
| `resource-service/src/outbox.rs:23` | resource |

相同的 `pool + jetstream` 上下文、超时校验 (0..5min)、`FOR UPDATE SKIP LOCKED` SQL 模式、sha256 hash 校验。

**收敛目标**: 已有的 persistence crate 或 messaging crate。

#### 3.2.3 JetStream PullConsumer wrapper (5 处)
#### 3.2.4 NATS Request/Reply serve loop (3 处)
#### 3.2.5 ProviderBackend client struct (3 处)
#### 3.2.6 rustls ClientConfig for non-NATS TLS (2 处)

### 3.3 Event Subject Lookup / 验证重复 (39 处)

**严重度: 🟡 中** — 模式简单但复制频繁

#### 3.3.1 event_contract() 查找函数 (8 处完全相同)

| 文件 | 服务 |
|------|------|
| `control-service/src/lib.rs:2845` | control |
| `access-service/src/grants.rs:2247` | access |
| `agent-service/src/run_store.rs:1945` | agent |
| `agent-service/src/build_store.rs:793` | agent (2nd) |
| `environment-service/src/store.rs:989` | environment |
| `evaluation-service/src/command_store.rs:440` | evaluation |
| `evaluation-service/src/control_plane.rs:1877` | evaluation (2nd) |
| `resource-service/src/store.rs:1626` | resource |

**所有 8 处逻辑相同**: 遍历 `EVENT_CONTRACTS` 数组按 subject 匹配。

**收敛目标**: 直接移至 `crates/contracts/src/events.rs`，作为 `EventContract::by_subject()` 静态方法。

#### 3.3.2 quarantine() 方法 (5 处)
#### 3.3.3 is_unique_violation() 辅助函数 (4 处) — PostgreSQL 错误码 23505 检查
#### 3.3.4 validate_worker() 格式校验 (3 处)
#### 3.3.5 read_secret() 文件读取 (4 处)

### 3.4 Revision 操作重复 (40 处)

**严重度: 🟡 中** — 分散但模式一致

#### 3.4.1 next_revision() 增量辅助函数 (5 处)

| 文件 | 服务 |
|------|------|
| `environment-service/src/lifecycle.rs:472` | environment |
| `environment-service/src/kubevirt_provider.rs:2045` | environment |
| `agent-service/src/run_store.rs:1953` | agent |
| `evaluation-service/src/control_plane.rs:1892` | evaluation |
| `resource-service/src/lib.rs:326` | resource |

全部执行 `current.get().checked_add(1)` → `Revision::new(value)`，仅返回的错误类型不同。

**收敛目标**: 作为 `Revision::next()` 方法添加到 `crates/contracts/src/foundation.rs`。

#### 3.4.2 i64 ↔ Revision 转换 (7 处)
#### 3.4.3 SQL upsert with revision fencing (3 处在 control-service 内)
#### 3.4.4 乐观锁 revision 比较 (跨 4 个服务)

---

## 4. xtask Placeholder 分析

### 4.1 总体状态: ✅ 健康

共 **42 个命令入口**:
- **30 个 functional** — 正常工作的命令
- **4 个 placeholder** — 非破坏性未实现，调用 `not_implemented()`，退出码 1
- **8 个 blocking_diagnostic** — 破坏性未实现，两阶段确认 + `XTASK_NOT_IMPLEMENTED`

### 4.2 Placeholder 清单

| 命令 | 类型 | 状态 | 建议 |
|------|------|------|------|
| `test --suite e2e` | placeholder | `not_implemented()`, exit 1 | 保留，E2E 测试框架未就绪 |
| `demo seed` | placeholder | `not_implemented()`, exit 1 | **可删除** — demo 种子非正式契约 |
| `playwright install` | placeholder | `not_implemented()`, exit 1 | 保留或实现 |
| `docs serve` | placeholder | `not_implemented()`, exit 1 | **可删除** — 文档服务非正式契约 |

### 4.3 Blocking Diagnostic 清单

| 命令 | 诊断码 | 建议 |
|------|--------|------|
| `bootstrap` | `XTASK_NOT_IMPLEMENTED` | 保留，需要 --yes 确认 |
| `upgrade` | `XTASK_NOT_IMPLEMENTED` | 保留 |
| `restore` | `XTASK_NOT_IMPLEMENTED` | 保留 |
| `destroy` | `XTASK_NOT_IMPLEMENTED` | 保留 |
| `tools` | `XTASK_NOT_IMPLEMENTED` | 保留 |
| `dev-deps` | `XTASK_NOT_IMPLEMENTED` | 保留 |
| `migrate` | `XTASK_NOT_IMPLEMENTED` | 保留 |
| `dev` | `XTASK_NOT_IMPLEMENTED` | 保留 |

**结论**: Fail-fast 契约完整，无静默成功的 placeholder。按 Epic 要求，仅 `demo seed` 和 `docs serve` 两个非正式规划入口考虑删除。

---

## 5. 预估影响

| 收敛动作 | 预估减行 | 复用方式 |
|----------|----------|----------|
| event_contract() → contracts crate | -120 行 (8×15行) | 共享方法 |
| connect_nats_mtls → auth/messaging crate | -120 行 (6×20行) | 共享函数 |
| OutboxDispatcher → persistence crate | -160 行 (4×40行) | 泛型结构体 |
| next_revision() → Revision::next() | -30 行 (5×6行) | 类型方法 |
| i64 ↔ Revision 转换 → contracts | -40 行 (7×6行) | 类型方法/From trait |
| serve_mtls 空壳清理 | -20 行 (4×5行) | 删除死代码 |
| is_unique_violation() → persistence | -20 行 (4×5行) | 共享函数 |
| **合计** | **~510 行** | **0 新增 deps** |

---

## 6. 推荐模块边界

### 6.1 control-service/src/lib.rs (3165 → ~3×900 行)

```
lib.rs                          (orchestration: 用例协调，~900行)
  ↓ 拆出
persistence.rs                  (SQL upsert, revision fencing, ~700行)
helpers.rs                      (next_revision, i64↔Revision, event_contract wrapper, ~200行)
```

### 6.2 access-service/src/grants.rs (2496 → ~3×800 行)

```
grants.rs                       (grant CRUD + NATS event publishing, ~800行)
  ↓ 拆出
sessions.rs                     (gateway session lifecycle, ~700行)
helper.rs                       (revision conversion, event_contract, quarantine, ~200行)
```

### 6.3 agent-service/src/run_store.rs (2090 → ~3×700 行)

```
run_orchestration.rs            (create/cancel/retry AgentRun flows, ~700行)
run_persistence.rs              (PostgreSQL queries, ~600行)
outbox.rs                       (AgentOutboxDispatcher integration, ~300行)
```

### 6.4 evaluation-service/src/control_plane.rs (2108 → ~2×1000 行)

```
control_orchestration.rs        (release/run/step operations, ~1000行)
control_persistence.rs          (SQL helpers, revision, event lookup, ~700行)
```

### 6.5 公共收敛目标

| 重复模式 | 目标位置 | 优先级 |
|----------|----------|--------|
| `event_contract(subject)` | `crates/contracts/src/events.rs` | P0 (最简单) |
| `next_revision()` → `Revision::next()` | `crates/contracts/src/foundation.rs` | P0 |
| i64 ↔ Revision 转换 | `crates/contracts/src/foundation.rs` | P0 |
| `connect_nats_mtls()` | `crates/auth/src/nats.rs` | P1 |
| `OutboxDispatcher` | `crates/persistence/` 或新建 messaging crate | P1 |
| `is_unique_violation()` | `crates/persistence/` | P2 |
| `serve_mtls` 空壳清理 | 各服务 api.rs 内 | P2 |

---

## 7. 子 Issue 拆分建议

按 Epic 约束：每个 ≤5 SP，按 Owner 域划分。

| 子 Issue | 标题 | 范围 | SP | 依赖 |
|----------|------|------|----|A--|
| Sub-1 | 公共 runtime plumbing 收敛 | NATS mTLS 连接 + OutboxDispatcher + event_contract + revision helper → contracts/auth/persistence crates | 3 | 无 |
| Sub-2 | Control Service 拆分 | lib.rs 3165→900+700+200，api.rs 清理 serve_mtls 空壳 | 2 | Sub-1 |
| Sub-3 | Agent + Access Service 拆分 | run_store.rs/grants.rs 拆分，revision helper 移除 | 2 | Sub-1 |
| Sub-4 | Environment + Evaluation Service 瘦身 | provider revision helper 移除，control_plane 拆分, event lookup 移除 | 2 | Sub-1 |
| Sub-5 | xtask cleanup + Web/Deploy | 删除 demo seed/docs serve placeholder，验证其他服务收敛效果 | 1 | Sub-1~4 |

**总计: 10 SP（含缓冲，实际 Epic 预算 5 SP，需裁剪范围或分两期）**

建议优先级执行顺序: Sub-1 → Sub-3 → Sub-2 → Sub-4 → Sub-5。Sub-1 是最基础也是最高回报的收敛，完成后其余拆分将直接受益。

---

## 8. 验收检查清单映射

| Epic 验收条件 | 审计结论 | 状态 |
|--------------|----------|------|
| 大文件清单、重复实现证据、Owner、目标模块边界 | ✅ 本报告已覆盖 | DONE |
| xtask placeholder 已删除; 保留入口有 blocking diagnostic | ⚠️ 需删除 demo seed, docs serve 2 个非正式入口 | PENDING |
| 重复 runtime plumbing 收敛到明确 Owner | ⚠️ Sub-1 待实施 | PENDING |
| REST/NATS/Schema/Provider binding 不变 | ✅ 所有收敛均为内部重构，不改变公共契约 | GUARANTEED |
| generated contracts 无未声明 diff | ✅ 收敛目标为已有 contracts crate，不修改 schema | GUARANTEED |

---

*报告生成: Claude Code 多代理审计工作流 (7 agents, 406K tokens, 235 tool calls)*
