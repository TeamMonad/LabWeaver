# LabWeaver：GitHub 驱动 Scrum + Codex Agent 四人每日工作计划

> **版本**：v1.0
> **计划周期**：2026-07-11 至 2026-07-24
> **团队规模**：4 人
> **发布目标**：`v1.0.0`
> **依据文档**：`LabWeaver_项目计划与课程落地方案_v2.1.md`、`LabWeaver_生产级技术实现方案_v2.1.md`
> **代码仓库**：`github.com/TeamMonad/LabWeaver`

前端工作项统一采用 **Material You**。GCP Console 只作为信息架构、控制台密度、导航层级与状态表达的参考，不复制 Google 品牌或产品视觉。角色 C 负责后续 token、组件和页面细化；7 月 11 日角色 A 只更新设计方向，不实现或代替验收前端工作。

---

## 目录

1. [计划概览](#1-计划概览)
2. [GitHub 驱动的 Scrum 工作模型](#2-github-驱动的-scrum-工作模型)
3. [Codex Agent 协作协议](#3-codex-agent-协作协议)
4. [固定的每日 Scrum 节奏](#4-固定的每日-scrum-节奏)
5. [四人逐日详细工作计划](#5-四人逐日详细工作计划)
6. [四个 Sprint 的退出门禁](#6-四个-sprint-的退出门禁)
7. [范围控制和延期处理规则](#7-范围控制和延期处理规则)
8. [每日执行检查表](#8-每日执行检查表)

---

# 1. 计划概览

本计划从 **2026 年 7 月 11 日启动，到 7 月 24 日发布 `v1.0.0`**，覆盖四个 Sprint、四名成员和 14 个自然日。角色、Sprint 目标、Backlog 编号、范围冻结点和验收口径来自项目计划 v2.1；服务边界、代码目录、实现顺序和安全约束来自生产级技术实现方案 v2.1。

四人总投入约 **134.5 人时**：

| 成员 | 角色 | 计划投入 |
|---|---|---:|
| A | 架构工程师、组长、PM | 34 小时 |
| B | Agent / Environment / Evaluation 工程师 | 33 小时 |
| C | 前端工程师 | 32.5 小时 |
| D | 测试、DevOps、文档工程师 | 35 小时 |

Codex Agent 不作为“第五名成员”。每个任务始终有一个人类 Owner；Codex 负责分析、脚手架、局部实现、测试生成、文档和代码检查，人类负责需求、架构、安全边界、验收和合并。

---

# 2. GitHub 驱动的 Scrum 工作模型

## 2.1 GitHub Project 配置

建立一个组织级或仓库级 GitHub Project，字段如下：

| 字段 | 可选值 |
|---|---|
| Workflow Status | Backlog / Ready / In Progress / Draft PR / In Review / Verify / Done / Blocked |
| Sprint | S1 Foundation / S2 Environment / S3 Evaluation / S4 Release |
| Priority | P0 / P1 / P2 |
| Area | Architecture / Access / Environment / Agent / Evaluation / Resource / Web / Test / Deploy / Docs |
| Owner | A / B / C / D |
| Reviewer | A / B / C / D |
| Codex Mode | C0 / C1 / C2 / C3 |
| Risk | Low / Medium / High |
| SP | 1 / 2 / 3 / 5 |
| Evidence | PR、测试报告、截图、Trace、日志或演示链接 |
| Due Date | 7/11–7/24 |

GitHub 当前将内置 `Status`、`Priority` 和 `Target date` 暴露为 Issue-derived 字段。LabWeaver 使用可写的 `Workflow Status` 与 `Delivery Priority` 保存 Scrum 状态和 P0/P1/P2；`Target date` 通过 Issue field API 更新。不得在 Issue 正文中伪造 Project 字段证据。

原 Backlog 中 8 或 13 SP 的项目一律作为 Epic，拆为不超过 3 SP、最长一天可完成的子 Issue。

## 2.2 状态流转

```text
Backlog
→ Ready
→ In Progress
→ Draft PR
→ In Review
→ Verify
→ Done

任何状态均可进入 Blocked
```

每人 WIP 上限为：

- 1 个编码 Issue；
- 1 个 Review；
- 避免四个人同时开启大量半成品。

## 2.3 分支和合并规则

```text
main
└── release/v1.0.0
    └── develop
        ├── feature/<issue-id>-<slug>
        ├── fix/<issue-id>-<slug>
        └── docs/<issue-id>-<slug>
```

规则：

- `main` 和 `develop` 都禁止直接 Push；
- 合入 `develop` 至少需要 1 名人类 Reviewer，且必需 CI 全绿；
- Schema、Migration、权限、评分聚合、Agent Tool、CRD、安全策略由 A 与 B 双人评审；
- 合入 `main` 至少需要 2 名 Reviewer，且完整集成测试、Playwright Demo、Ansible Verify 全绿；
- 默认采用 Squash Merge，PR 标题作为最终 Commit；
- 禁止 Force Push 到受保护分支；
- Draft PR 用于尽早暴露接口、实现方向和冲突；
- Feature PR 目标分支为 `develop`，合并后由验收人在 Verify 完成时关闭 Issue。

## 2.4 Issue 模板

```markdown
## 用户价值
该变更解决什么问题？

## 范围
本 Issue 要修改哪些模块？

## 非目标
明确不做什么。

## 依赖
依赖哪些 Issue、Schema、服务或环境？

## 验收条件
- [ ] 功能条件
- [ ] 错误/边界条件
- [ ] 权限或安全条件
- [ ] 文档条件

## 必须执行
- [ ] 单元测试
- [ ] 契约/集成测试
- [ ] 手工或 E2E 验证

## Codex 使用方式
C0 / C1 / C2 / C3

## 证据
PR、日志、截图、Trace、测试报告。

## Reviewer
指定一名主 Reviewer 和一名验收人。
```

## 2.5 PR 模板

```markdown
## 关联 Issue
- Relates to #<issue-id>

## 变更目标
本 PR 完成什么？

## 修改范围
- 服务/目录：
- API/Schema：
- 数据库/事件：

## 验收结果
- [ ] 单元测试通过
- [ ] 契约或集成测试通过
- [ ] 手工/E2E 验证通过
- [ ] 文档已同步

## 测试命令与结果
```bash
# 粘贴实际执行命令
```

## 风险与回滚
- 风险：
- 回滚方式：

## Codex 使用说明
- Codex 生成或修改了哪些部分：
- 人工确认了哪些设计或安全边界：
- 尚存哪些不确定性：

## 证据
截图、日志、Trace、报告链接。
```

## 2.6 Label 建议

```text
priority:P0 / priority:P1 / priority:P2
area:architecture / area:access / area:environment
area:agent / area:evaluation / area:resource
area:web / area:test / area:deploy / area:docs
risk:high / risk:medium / risk:low
status:blocked
codex:C0 / codex:C1 / codex:C2 / codex:C3
release-blocker
needs-contract
needs-security-review
needs-demo-evidence
```

---

# 3. Codex Agent 协作协议

## 3.1 Codex 自主级别

| 模式 | 用法 | 适用任务 |
|---|---|---|
| C0 人工执行 | Codex 不接触实际变更 | Secret、生产凭据、最终发布、审批、数据删除 |
| C1 计划/审查 | Codex 分析、列计划、审查 Diff，不直接决定设计 | 架构、权限、评分、安全策略、Migration |
| C2 实现加测试 | Codex 在人类确定接口后实现局部代码和测试，提交 Draft PR | Rust Handler、状态机、Runner、Operator、API Client |
| C3 小型自主 PR | Codex 完成边界清晰的小任务并创建 Draft PR | UI 组件、Fixture、测试脚本、文档、代码生成物 |

## 3.2 每个 Issue 的 Codex 流程

```text
人类创建 Issue 和验收条件
→ Codex 只读分析并输出实施计划
→ Owner 审核计划和文件范围
→ Codex 在独立分支实现并运行指定测试
→ Owner 检查 Diff、测试和安全边界
→ 第二名成员 Review
→ CI Verify
→ 人类合并和关闭 Issue
```

一个 Issue 对应：

- 一个 Owner；
- 一个 Codex Thread；
- 一个分支；
- 一个主要 PR；
- 一组明确测试；
- 一份验收证据。

## 3.3 根目录 `AGENTS.md` 最低内容

- 项目目标和 P0/P1 边界；
- Monorepo 目录所有权；
- 编译、Lint、测试命令；
- API、事件和 YAML 版本兼容规则；
- “LLM 不计分”“不执行未建模 Shell”等硬约束；
- 禁止修改的文件或敏感目录；
- PR 大小和 Commit 规范；
- 何时必须请求人工确认。

建议的子目录说明文件：

```text
services/agent-service/AGENTS.md
services/evaluation-service/AGENTS.md
services/access-service/AGENTS.md
web/AGENTS.md
tests/e2e/AGENTS.md
deploy/ansible/AGENTS.md
```

## 3.4 Codex 通用任务提示词

```text
阅读根目录和当前目录下的 AGENTS.md，以及 GitHub Issue #<id>。

先只输出实施计划，不修改文件。计划必须包含：
1. 将修改的文件；
2. 不会修改的范围；
3. 风险和兼容性影响；
4. 测试方案；
5. 回滚方式。

计划获批后：
- 仅处理 Issue 验收范围；
- 不引入未经批准的生产依赖；
- 不修改已冻结接口；
- 先补测试再实现；
- 运行 AGENTS.md 中要求的命令；
- 创建 Draft PR；
- 在 PR 中列出 AI 生成部分、人工确认点和未解决风险。
```

## 3.5 Codex Review 提示词

```text
审查当前 PR，重点检查：
1. 是否只实现了关联 Issue 的范围；
2. 是否破坏已冻结 API、事件、Schema 或 Trait；
3. 状态机、幂等、权限和错误路径是否有测试；
4. 是否出现任意 Shell、Secret 泄露、过宽 RBAC、开放网络或未固定镜像版本；
5. LLM 输出是否可能进入确定性评分；
6. 是否有回滚方法和可执行验收命令。

按以下格式输出：
- Blocker
- Must Fix
- Suggestion
- 已验证内容
- 尚未验证内容
```

---

# 4. 固定的每日 Scrum 节奏

## 4.1 每日开始

每人在自己的 Sprint Parent Issue 下评论：

```text
昨天完成：
今天唯一目标：
当前阻塞：
今日准备提交的 PR：
需要谁 Review：
```

随后进行 10 分钟 Daily Scrum：

1. 只讨论 Issue、PR 和可运行证据；
2. 阻塞超过 4 小时立即标记 `Blocked`；
3. 阻塞项必须指定解除负责人；
4. 不在 Daily 中讨论长方案，另开 ADR 或技术讨论 Issue。

## 4.2 每日中段

- 编码任务必须尽早创建 Draft PR；
- Codex 生成的首个可编译增量推送到分支；
- 接口变化立即通知依赖成员；
- 前端和测试不得等待完整后端，优先使用 OpenAPI Mock、Fixture 和 Contract；
- 高风险 Issue 在中段进行一次 Owner + Reviewer 快速同步；
- 当天预计无法完成时，立即拆分可交付增量，不在日终才暴露。

## 4.3 每日结束

- Owner 更新 Issue Checklist；
- Reviewer 当天完成第一轮 Review；
- 附测试命令和结果；
- Project 状态更新为 In Review、Verify 或 Done；
- 未完成任务拆分，不把模糊的“大任务”原样带到第二天；
- 每个成员至少保留一条可演示、可测试或可审查的证据。

## 4.4 固定评审关系

| PR 作者 | 主 Reviewer | 验收人 |
|---|---|---|
| A 架构/后端 | B | D |
| B Agent/Runtime/Evaluation | A | D |
| C 前端 | 对应后端 Owner | D |
| D 测试/部署/文档 | 对应模块 Owner | A |
| Release PR | A、B 中非作者 + D | 全员 |

---

# 5. 四人逐日详细工作计划

## 5.1 7 月 11 日，周六——Sprint 1 Day 1：仓库和项目治理启动

### A：架构工程师 / PM（3 小时，C1+C2）

负责 `PM-01a`、`ARC-01a`、`API-01a`。

- 创建四个 Milestone、Project 字段、Labels、Issue/PR 模板和 Sprint Parent Issues；
- 建立 Cargo Workspace、基础 Crate 和各 Axum Service 空壳；
- 建立 `main`、`develop` 保护规则和 `CODEOWNERS`；
- 创建 C4、服务边界、数据所有权和 ADR 目录；
- Codex 负责生成 Workspace、CI 和服务脚手架；A 人工确认服务边界。

GitHub 产物：

```text
feature/API-01-axum-workspace
docs/ARC-01-c4-and-adrs
PR: feat(core): bootstrap LabWeaver Rust workspace
```

验收：

```bash
cargo build --workspace
cargo fmt --check
```

### B：Agent 工程师（3 小时，C1+C2）

负责 `CORE-02a`、`AG-01a`。

- 把 `EvaluationSpec` 拆为 Submission、Step、Runner、Checker、Aggregation、Review；
- 建立 Agent 显式状态机和 `AgentTool` Trait；
- 创建 OJ 和 Linux 两个最小 YAML 样例；
- Codex 先生成 Rust 类型和 Schema 测试草案，B 决定字段语义和安全边界。

验收：

- 两个样例可被反序列化；
- LLM Review 类型中不存在 `score`；
- 状态机非法迁移有失败测试。

### C：前端工程师（2.5 小时，C3）

负责 `UI-01a`。

- 初始化 Vue 3、TypeScript、Router、状态管理和测试框架；
- 建立教师、学生、科研用户、管理员四种角色导航；
- 建立实验列表、工作环境列表、资源申请和评测结果空页面；
- Codex 生成组件和 Mock API 骨架，C 统一命名和页面布局。

验收：

```bash
pnpm lint
pnpm test
pnpm build
```

### D：测试工程师（3 小时，C3）

负责 `REQ-01a`、`PW-01a`、`LAB-01a`、`LAB-02a`。

- 把用户旅程和验收条件转成可测试清单；
- 初始化 Playwright，并建立 setup、teacher、student、admin Project；
- 准备 OJ 示例题材料包和 Linux Nginx 实验材料包；
- 编写 KubeVirt、StorageClass、Headscale、Ingress Preflight 清单；
- Codex 生成测试矩阵和目录；D 人工检查是否与 P0 验收一致。

### 日终门禁

- GitHub Project 可用；
- 至少 12 个 P0 子 Issue 进入 Ready；
- 四人均有首个分支或 PR；
- Workspace 和前端均可构建。

---

## 5.2 7 月 12 日，周日——Sprint 1 Day 2：关键 Contract 和真实 VM Spike

### A（3 小时，C1+C2）

负责 `CORE-01a`、`DB-01a`、`MSG-01a`、`ACCESS-01a`。

- 实现 Experiment/Work × Container/VM 领域枚举和状态机；
- 建立 SQLx Migration、Repository 和 Outbox 最小实现；
- 冻结 NATS Subject v1 命名；
- 编写 Keycloak、Headscale、AccessGrant 双层权限 ADR；
- Codex 编写状态机属性测试和 Migration 验证脚本。

验收：

```bash
sqlx migrate run
cargo test -p common-domain
cargo test -p messaging-nats
```

### B（3.5 小时，C1+C2）

负责 `CORE-02b`、`AG-01b`、`VM-01a`。

- 完成 `EvaluationSpec v1alpha1` JSON Schema；
- 实现 Fixture LLM Backend、Tool Registry 和最多两次 Repair 逻辑；
- 与 D 在真实集群创建最小 KubeVirt VM；
- 保存 VM YAML、VMI 状态和启动日志作为证据；
- Codex 实现 Fixture 和 Schema 生成；真实集群操作由 B 人工执行。

验收：

```bash
kubectl get vm,vmi -A
cargo test -p agent-core
```

### C（3 小时，C2+C3）

负责 `UI-02a`、`UI-ACCESSa`。

- 集成 Monaco YAML Editor；
- 接入 JSON Schema 校验、错误定位和环境/评测 YAML Diff；
- 完成 Tailnet 接入向导静态页面；
- 用 Fixture 展示有效和无效 YAML。

验收：

- 无效字段可精确定位；
- EnvironmentSpec 和 EvaluationSpec 可切换；
- Diff 页面能显示 Agent 版本与教师修改版本。

### D（3 小时，C2+C3）

负责 `DEP-01a`、`VM-01b`、`PW-01b`。

- 执行 Kubernetes/KVM/StorageClass Preflight；
- 与 B 完成 VM Start、Stop、Start；
- 建立 `deploy/ansible` 目录、Inventory 和 `preflight.yml`；
- 生成首个 Playwright Trace；
- 把不满足项登记为环境风险 Issue，不隐藏失败。

### 日终门禁

- 真实 VM 进入 Running；
- OJ/Linux YAML 均通过 Schema；
- 前端可编辑和校验 YAML；
- Fixture LLM 重复执行结果一致。

---

## 5.3 7 月 13 日，周一——Sprint 1 Review、接口冻结和 `v0.1-foundation`

### A（2 小时，C0+C1）

- 主持 Sprint Review 和 Retro；
- 合并 S1 PR，解决接口冲突；
- 冻结关键 Trait、事件 Subject、状态机和目录；
- 更新 Project、风险表和 Sprint 2 Ready 队列；
- 创建并推送 Tag `v0.1-foundation`。

### B（2 小时，C1+C2）

- 补全 OJ/Linux 两个完整 Schema 示例；
- 建立 Schema Snapshot，后续 CI 检查破坏性变化；
- 修复真实 VM Spike 中发现的问题；
- Review A 的领域模型和 NATS Contract。

### C（1.5 小时，C2）

- 把前端演示路径固定为：角色切换 → 实验列表 → YAML 编辑 → Diff；
- 补充加载、空数据和 Schema 错误状态；
- 为第一次汇报准备截图。

### D（2 小时，C2+C3）

- 执行 S1 全量测试；
- 归档 VM 日志、Playwright Trace 和测试报告；
- 编写 Sprint 1 Review/Retro 文档；
- 创建 Sprint 2 测试子 Issue。

### Sprint 1 验收

```bash
cargo build --workspace
cargo test --workspace
pnpm test
kubectl get vm,vmi -A
```

---

## 5.4 7 月 14 日，周二——Sprint 2 Day 1：容器环境纵向切片

### A（2.5 小时，C1+C2）

负责 `CAT-01a`、`ART-01a`、`API-01b`。

- 实现实验包、模板草稿、版本和审批 API；
- 建立 MinIO Artifact Metadata 和预签名上传接口；
- 接入 Outbox，创建实验后发布 Agent 请求事件；
- 定义 SSE 事件 Envelope；
- Codex 实现 Handler、Repository 和 Tower Contract Test。

验收：

- 重复 `Idempotency-Key` 不产生重复实验包；
- 上传只能写入授权前缀；
- API 返回统一错误模型。

### B（3 小时，C1+C2）

负责 `ENV-01a`、`ENV-02a`、`BUILD-01a`。

- 定义 `ComputeEnvironment` CRD；
- 实现 Operator 最小 Reconcile；
- 创建 Namespace、Quota、PVC、Deployment 和 Service；
- 建立 BuildRequest 和 BuildKit Executor Stub；
- Codex 实现 kube-rs 控制器脚手架和 Fake Client 测试。

验收：

- 重复 Reconcile 不重复创建资源；
- 删除 CRD 可进入 Finalizer 流程；
- 容器环境状态可从 Requested 到 Ready。

### C（3 小时，C2+C3）

负责 `UI-03a`。

- 教师材料上传页面；
- Agent Run 状态页；
- Environment、Submission、Evaluation 三份 YAML Tab；
- 教师 Approve/Reject 操作；
- 先使用真实 Contract + Mock Response，不等待后端全部完成。

### D（2.5 小时，C2+C3）

负责 `TEST-ENV-a`、`COL-fixture-a`。

- 建立 API Contract Test Harness；
- 配置 PostgreSQL、NATS、MinIO Test Container；
- 建立 PVC Collector 测试目录和白名单样例；
- 把 CI 拆为 Rust、Frontend、Schema 和 Contract 四个唯一命名 Job。

### 日终门禁

教师上传材料后，系统可以产生 Agent 请求；最小容器 CRD 可以进入 Ready，前端能展示完整草稿和审批界面。

---

## 5.5 7 月 15 日，周三——Sprint 2 Day 2：OIDC、Tailnet、KubeVirt 和 Collector

### A（3 小时，C1+C2）

负责 `AUTH-01a`、`ACCESS-01b`、`ACCESS-02a`。

- 接入 Keycloak OIDC Authorization Code + PKCE；
- 实现角色 Claims 和课程/项目范围检查；
- 建立设备、AccessGrant、EndpointGrant API；
- 建立 Headscale Policy Compiler 最小模板；
- 实现 AccessGrant 创建、过期和撤销事件；
- Codex 实现 Token 验证 Middleware 和权限矩阵测试；Policy 语义由 A 人工确认。

### B（3 小时，C1+C2）

负责 `ENV-03a`、`VM-02a`、`CFG-01a`、`COL-01a`、`COL-02a`。

- 实现 KubeVirt Runtime Provider；
- 创建 DataVolume、VirtualMachine、cloud-init Secret 和 Service；
- 完成 Ubuntu 基础镜像或固定可用镜像接入；
- 执行最小 Ansible Bootstrap；
- 实现 PVC 和 SSH Collector 的接口及 Happy Path；
- Codex 生成 Resource Builder、状态映射和 Collector 测试；VM 权限和凭据人工检查。

### C（3 小时，C2+C3）

负责 `UI-04a`、`UI-ACCESSb`。

- 实验/工作环境控制台；
- Start、Stop、Reset、Delete 操作；
- code-server、SSH、VNC 入口卡片；
- Tailnet 设备状态和 AccessGrant 到期提示；
- 处理“设备未注册”“Grant 过期”“环境未 Ready”等错误状态。

### D（3 小时，C2+C3）

负责 `TEST-ACCESS-a`、`TEST-ENV-b`、`PW-01c`。

- Headscale Policy Allow/Deny 矩阵；
- AccessGrant 有效、过期、撤销测试；
- VM Start/Stop/Delete 集成测试；
- Ansible 首次执行和第二次幂等检查；
- 生成 teacher/student/admin 的 Playwright `storageState`。

### 日终门禁

- 教师和学生角色可登录；
- 真实 VM 可由平台创建；
- 授权用户可以获得端点信息；
- 未授权用户被明确拒绝；
- Collector 能生成初步快照和哈希。

---

## 5.6 7 月 16 日，周四——Sprint 2 Review 和 `v0.2-environment`

### A（2 小时，C0+C1）

- 修复 Catalog、Access、Artifact API 联调问题；
- 检查 Idempotency、权限和审计字段；
- Review B 的 Runtime/Collector PR；
- 合并并创建 Tag `v0.2-environment`。

### B（2 小时，C2）

- 完成 Container/VM Smoke Test；
- 让 Collector 输出模板版本、镜像摘要和 SHA-256；
- 完成 Work Container 安装普通软件的最小 BuildKit 路径；
- 完成 Work VM Ansible 配置的最小路径。

### C（2 小时，C2）

- 跑通教师发布、学生启动容器、学生启动 VM；
- 修复状态轮询和 SSE 显示；
- 提交环境闭环演示截图。

### D（2.5 小时，C2）

- 执行 Container、VM、Collector、Access 全量测试；
- 验证重复 Reconcile 和重复提交不产生重复资源；
- 生成 Sprint 2 Playwright Trace 和缺陷清单；
- 主持环境闭环验收。

### Sprint 2 验收

必须演示：

```text
材料上传
→ Agent 环境草稿
→ 教师批准
→ 容器 Ready
→ 真实 VM Ready
→ Tailnet 入口
→ Collector 冻结
→ MinIO 对象与 SHA-256
```

---

## 5.7 7 月 17 日，周五——Sprint 3 Day 1：Evaluation DAG 和 Program Runner

### A（3 小时，C1+C2）

负责 `EVAL-01a`。

- 实现 EvaluationRun、StepRun 状态机；
- 使用 `petgraph` 校验依赖存在、ID 唯一和无环；
- 创建 Run API、Step API、Retry、Cancel；
- 建立初始 Ready Step 投递和 Outbox；
- Codex 实现 DAG 测试和 Repository；A 决定重试和并发语义。

验收：

- 无依赖 Step 首先 Ready；
- 依赖失败时正确 Skip/Stop；
- 重复 Step 完成事件不重复计分。

### B（3 小时，C2）

负责 `EVAL-02a`、`EVAL-03a`、`GEN-01a`。

- 实现 Kubernetes Job Executor；
- 实现 C++17 Compile 和 Test Group Program Runner；
- 建立超时、取消、日志和 Evidence 收集；
- 建立 Cyaron Toolbox 容器及固定 Seed；
- Codex 生成 Job Builder、结果解析和测试样例。

### C（2.5 小时，C2+C3）

负责 `UI-05a`。

- Evaluation YAML 查看和编辑页面；
- EvaluationRun 状态时间线；
- Step 列表、状态、耗时和 Retry/Cancel；
- 用 Fixture 显示 Compile、Correctness、Review。

### D（3 小时，C2+C3）

负责 `TEST-EVAL-a`、`LAB-01b`。

- 准备正确解和至少 5 个典型错解；
- 建立 Runner Contract Test；
- 覆盖 Compile Error、Runtime Error、TLE、WA、Output Limit；
- 为 Cyaron 固定 Seed 建立哈希快照。

### 日终门禁

正确解和错误解均能进入 Compile Job；EvaluationRun 状态可以通过事件推进并在前端显示。

---

## 5.8 7 月 18 日，周六——Sprint 3 Day 2：Checker、Linux Probe 和资源审批

### A（3 小时，C1+C2）

负责 `EVAL-01b`、`RES-01a`。

- 实现确定性 Aggregator 和 Gate；
- 强制 Advisory Step 不参与分数；
- 实现资源申请、管理员 Approve/Resize/Reject API；
- 建立 Lease 和重复审批保护；
- Codex 编写聚合属性测试；评分语义由 A 与 B 双人确认。

### B（3 小时，C1+C2）

负责 `EVAL-04a`、`EVAL-05a`、`GEN-02a`。

- 实现 exact、token、float 和 SPJ Checker；
- 实现 Ansible/SSH System Probe；
- 建立 Nginx 包、服务、配置、端口和 HTTP 行为断言；
- 生成标程、暴力 Oracle、Validator、Cyaron Generator 和 Mutants；
- Codex 生成初始资产，B 人工验证 Checker 和 Probe 不执行危险操作。

### C（3 小时，C2+C3）

负责 `UI-05b`、`UI-06a`。

- Evidence 文件、日志、指标、反馈展示；
- 确定性结果和 LLM 建议分区展示；
- 资源申请表和管理员审批页面；
- Lease 状态和剩余时间显示。

### D（3 小时，C2+C3）

负责 `VERIFY-01a`、`LAB-02b`。

- 标程与 Oracle 差分测试；
- 固定 Seed 重复性测试；
- Mutant 杀伤率测试；
- SPJ 错误接受/错误拒绝测试；
- Linux 服务停止、端口错误、内容错误等故障 Fixture；
- 资源重复审批和越权审批测试。

### 日终门禁

- 正确解与 Oracle 一致；
- 至少 5 个错解中 4 个被杀死；
- Linux Probe 能识别至少一项故意破坏；
- 管理员可审批 Mock GPU 请求。

---

## 5.9 7 月 19 日，周日——Sprint 3 Day 3：LLM Review、Mock Capacity 和多角色 E2E

### A（2 小时，C1+C2）

负责 `RES-01b`、`ACCESS-02b`。

- 实现 Lease Active、Expiring、Expired；
- Lease 到期触发 AccessGrant 撤销和资源释放；
- 补充 Resource、Access 和 Environment 跨域审计；
- 检查 SSE 断线续传；
- Review B 的 Review Schema 和 Mock Provider。

### B（2.5 小时，C1+C2）

负责 `EVAL-06a`、`CAP-01a`、`RES-02a`、`GEN-02b`。

- 实现 advisory-only LLM Goal Review；
- 服务端拒绝含 `score` 的 LLM 输出；
- 实现 FixtureCapacityProvider 全状态链；
- 实现 Lease 到期回收 Worker；
- 完成编译、差分、变异、安全和预算 Verification Report；
- Codex 负责 Fixture、Mock 状态机和负面测试；B 审核输出边界。

### C（3 小时，C2+C3）

- 串联教师、学生、管理员三个角色页面；
- 完成 AccessGrant 过期后的拒绝页面；
- 完成 Mock Capacity 的 Estimating、Allocating、Ready、Releasing 动画和状态；
- 修复 Evaluation 时间线和 Evidence 导航。

### D（3 小时，C2+C3）

负责 `TEST-EVAL-b`、`PW-02a`。

- Playwright 教师发布 OJ → 学生错误解 → 正确解；
- Linux VM → 故障 Probe → 修复 → 通过；
- 资源申请 → 管理员审批 → Mock Ready → 到期；
- 未授权用户和过期 Grant 访问失败；
- 完成 12 个 Agent 黄金样例和 Prompt Injection 测试；
- 对 Worker Restart、NATS 重复事件、LLM 超时做失败注入。

### 日终门禁

三条黄金路径可以独立运行并产生 Trace；LLM 输出永远不改变确定性分数。

---

## 5.10 7 月 20 日，周一——Sprint 3 Review、功能冻结和 `v0.3-feature-complete`

### A（1.5 小时，C0+C1）

- 主持 Sprint Review；
- 关闭或降级所有功能型阻塞项；
- 冻结 API、事件、Runner 类型和用户功能；
- 建立 S4 缺陷、部署、文档和演示队列；
- 创建 Tag `v0.3-feature-complete`。

### B（1.5 小时，C1+C2）

- 只修复主链 Blocker；
- 固定 Evaluation Bundle 和 Fixture 版本；
- 输出 Agent/Evaluation 已知限制；
- 不新增 Runner 或生成器类型。

### C（1.5 小时，C2）

- 冻结页面和主导航；
- 把非关键视觉问题移入 P1；
- 确认三角色演示路径。

### D（2 小时，C2）

- 跑完整回归；
- 输出 Feature Complete Test Report；
- 把失败分为 Release Blocker、Must Fix、Known Issue；
- 验证所有关键 Issue 都有 Evidence。

### Sprint 3 验收

```text
OJ：错误解失败、正确解通过、LLM 仅建议
Linux：故障可识别、修复后通过
Resource：申请、审批、Mock Ready、到期回收
Access：未授权和过期访问失败
```

**从当天起禁止新增用户功能。**

---

## 5.11 7 月 21 日，周二——Sprint 4 Day 1：Ansible、Helm、安全硬化

### A（3 小时，C1+C2）

负责 `OPS-02a`、`OBS-01a`、`REL-01a`。

- 完成 LabWeaver Platform Helm Chart；
- 完成 Ansible Platform Role 和部署顺序；
- 配置 Migration、NATS Stream、MinIO Bucket 和 Keycloak Seed；
- 增加统一 Trace ID 和基本 Metrics；
- 建立 Release Checklist 和 Release Notes 草稿；
- Codex 实现 Role/Values 模板，A 检查变量和生产安全默认值。

### B（2 小时，C1+C2）

负责 `SEC-01a`。

- 锁定 Prompt、Tool 和 Fixture 版本；
- 对 Agent 生成资产计算哈希；
- 强化 Tool Allowlist、Ansible Module Allowlist；
- 增加危险命令、Prompt Injection、越界路径拒绝；
- 完成 Evaluation Job Restricted SecurityContext 模板。

### C（2.5 小时，C2+C3）

- 补充全局错误页、超时、重试和空状态；
- 修复响应式问题；
- 固定 Demo Seed 数据展示；
- 完成 Tailnet Onboarding 操作说明和页面截图；
- Codex 可处理重复 UI 和文档截图清单。

### D（2.5 小时，C2+C3）

负责 `OPS-01a`、`OPS-03a`。

- 完成 PostgreSQL、NATS、MinIO、Keycloak、Headscale、Kyverno、KubeVirt Role；
- 执行 `ansible-lint`、Syntax Check；
- 建立 `verify.yml`；
- 执行 Trivy、cargo audit/deny、Kyverno Policy Test；
- 检查 Secret 未进入 Git 或镜像。

### 日终门禁

从已有 Kubernetes 管理凭据执行 Ansible 后，平台核心组件能够启动；高危策略失败会阻止 Verify。

---

## 5.12 7 月 22 日，周三——Sprint 4 Day 2：升级回滚、Trace、权限负例和文档走读

### A（2.5 小时，C1+C2）

- 完成 Upgrade、Rollback、Backup、Restore 的控制流程；
- 验证 Headscale Policy Revision 和 Access Gateway Fail-Closed；
- 建立 Release Candidate 分支；
- 修复 API 性能和 NATS Lag 可观测问题；
- 执行一次 Migration Dry Run。

### B（2 小时，C1+C2）

- 验证 Worker 重启和重复事件幂等；
- 测试 LLM 不可用时 Fixture 降级；
- 测试 MinIO/NATS 短时故障恢复；
- 对评测脚本签名和 Bundle 哈希做最终校验；
- 不新增实现，只修复 Blocker。

### C（2 小时，C2）

- 对主路径进行 UI QA；
- 统一状态名称和错误文案；
- 验证教师、学生、管理员页面不会显示越权按钮；
- 完成演示模式和普通模式的明显标识。

### D（2.5 小时，C2+C3）

- Playwright 每次 Demo Run 保存 Trace；
- Tailnet Allow/Deny、节点过期、AccessGrant 撤销负例；
- API 和环境 Ready 基础性能测试；
- 按文档从干净环境执行开发 Quickstart 和部署 Quickstart；
- 修复文档中遗漏的命令和配置项。

### 日终门禁

```bash
make ansible-deploy ENV=demo
make ansible-verify ENV=demo
make playwright-e2e
```

均通过；失败时有明确日志、截图或 Trace。

---

## 5.13 7 月 23 日，周四——Sprint 4 Day 3：Release Candidate 和三次完整彩排

### A（2 小时，C0+C1）

- 创建 Release Candidate；
- 确认所有 Migration、CRD 和 Schema 可升级；
- 准备 GitHub Project、Commit Graph、Tags、PR Review 证据；
- 主持三次 19 分钟完整彩排；
- 只接受 Release Blocker。

### B（1.5 小时，C1+C2）

- 预拉取和预构建演示镜像；
- 固定 OJ 数据 Seed、VM 镜像和 Fixture；
- 准备 LLM 离线 Fixture 和 Mock Capacity；
- 在彩排中监控 Environment、Evaluation 和 Agent 状态；
- 修复主链唯一阻塞问题。

### C（2 小时，C2）

- 固定演示浏览器账号和页面顺序；
- 准备教师、学生、管理员三套窗口；
- 完成演示所需最小视觉打磨；
- 编写逐屏操作提示，不再改页面结构。

### D（2 小时，C2）

负责 `PW-03a`、`PRE-01a`。

- 连续执行三次 `demo-reset → demo-replay`；
- 保存每次 HTML Report、Trace 和录像；
- 录制备用演示；
- 检查时间分配、网络、账号、证书和设备状态；
- 关闭所有 Release Blocker 或记录明确降级步骤。

### 日终门禁

- 三次彩排全部成功；
- Release Candidate 无未说明高危问题；
- 主流程不依赖现场外网和实时 LLM；
- 备用录像与当前 Commit 完全一致。

---

## 5.14 7 月 24 日，周五——Sprint 4 Review、`v1.0.0` 发布和最终展示

### A（1.5 小时，C0）

- 冻结 `develop`；
- 创建并审批 `release/v1.0.0 → main` PR；
- 确认 Release Notes、SBOM、已知问题和回滚步骤；
- 创建 GitHub Release 和 Tag `v1.0.0`；
- 负责最终 Presentation 的架构、Git 和 Sprint 部分。

### B（1 小时，C0+C1）

- 检查 Agent、Environment、Evaluation Worker 健康状态；
- 演示期间负责 OJ 和 Linux 技术链路；
- 发生 LLM 故障时切换 Fixture；
- 发生动态构建过慢时使用缓存镜像并展示真实摘要。

### C（1 小时，C0）

- 操作教师、学生、管理员前端流程；
- 保证每一步都展示状态和证据；
- 避免临时改变演示数据或交互路径。

### D（1 小时，C0）

- 发布前执行最终 Preflight；
- 打开 Playwright Report 和 Trace 备用页；
- 负责测试、安全、Ansible 和演示复现部分；
- 展示结束后归档 Release Evidence。

### 最终发布命令

```bash
make bootstrap
make ansible-deploy ENV=demo
make ansible-verify ENV=demo
make test
make playwright-e2e
make demo-replay
make demo-reset
```

### 最终 GitHub 证据

```bash
git log --graph --decorate --oneline --all
git shortlog -sn --all
git tag --list
```

---

# 6. 四个 Sprint 的退出门禁

| 日期 | Sprint | 必须满足 |
|---|---|---|
| 7/13 | S1 Foundation | 真实 VM Running；Schema 通过；Workspace/前端构建；Tag `v0.1-foundation` |
| 7/16 | S2 Environment | 容器和 VM 环境闭环；Tailnet 授权；不可变快照；Tag `v0.2-environment` |
| 7/20 | S3 Feature Complete | OJ、Linux、Resource、Access 三条主线通过；Tag `v0.3-feature-complete` |
| 7/24 | S4 Release | Ansible、测试、Trace、三次彩排、Release 和 Tag `v1.0.0` |

## 6.1 Sprint Review 固定议程

```text
1. Sprint Goal 是否完成
2. 演示可运行增量
3. 展示测试、日志、Trace 或截图
4. 检查未完成 Issue 和范围变化
5. 记录技术债与风险
6. Retro：保留、停止、开始
7. 下一 Sprint 只将符合 DoR 的任务放入 Ready
```

## 6.2 Release Blocker 判定

以下任一情况存在即不得发布：

- 真实 KubeVirt 主流程不可运行；
- LLM 输出可改变确定性成绩；
- 未授权用户可访问他人环境；
- AccessGrant 过期或撤销后仍可建立新连接；
- 重复事件造成重复评分、重复环境或重复 Lease；
- Ansible Verify 无法通过；
- 三条主 E2E 中任一条无法稳定重放；
- 有未说明的高危依赖、镜像或 Kyverno 策略问题；
- Release 无回滚方法或 Migration 不可控。

---

# 7. 范围控制和延期处理规则

以下规则不可由 Codex 或单个成员自行突破：

1. **7 月 13 日后不增加微服务。**
2. **7 月 16 日后不增加 Runner 类型。**
3. **7 月 20 日后不增加用户功能。**
4. P1 不得占用 P0 时间。
5. 一个 Issue 连续两天未完成，必须拆分或降级。
6. 核心链路优先级始终为：

```text
真实 KubeVirt
→ Environment 闭环
→ Collector
→ EvaluationSpec
→ OJ Program Runner
→ Linux Probe
→ AccessGrant
→ Playwright
→ Ansible
→ 文档和演示
```

7. LLM 不可用时使用 Fixture；GPU/云使用 Mock Capacity；动态构建过慢时使用预构建缓存；但真实 KubeVirt VM 不可降级为纯 Mock。
8. Codex 生成的代码必须经过相同的 Review、测试、权限和发布门禁，不因“由 Agent 生成”而降低标准。
9. 阻塞超过 4 小时必须在 Project 标记 `Blocked`，并指定解除负责人。
10. 任何接口变更必须先更新 Contract 或 ADR，再改调用方。
11. 任何高风险安全问题优先于功能完成度。
12. 不能稳定自动化重放的功能，不视为真正完成。

## 7.1 延期时的降级顺序

优先延期或删除：

1. UI 视觉美化；
2. 非关键动画和通知；
3. 多浏览器测试；
4. 高级可观测看板；
5. P1 Provider；
6. 第三类实验示例；
7. 复杂资源公平共享和抢占；
8. 多集群和真实云扩容。

不得延期：

- 真实 KubeVirt VM；
- OJ 与 Linux 两类统一评测；
- 教师审批门禁；
- LLM advisory-only；
- AccessGrant 与越权拒绝；
- 不可变提交快照；
- Playwright 黄金路径；
- Ansible Deploy/Verify；
- Release、Tag、文档和演示证据。

---

# 8. 每日执行检查表

## 8.1 成员日开始检查表

```markdown
- [ ] 今日唯一目标已写入 Sprint Parent Issue
- [ ] 当前 Issue 处于 Ready
- [ ] 验收条件明确
- [ ] Reviewer 已指定
- [ ] Codex 模式已指定
- [ ] 分支名符合规范
- [ ] 已阅读 AGENTS.md
- [ ] 依赖和风险已确认
```

## 8.2 提交 Draft PR 前检查表

```markdown
- [ ] PR 关联 Issue
- [ ] 修改范围未超出 Issue
- [ ] 已运行最小测试
- [ ] API/Schema/事件变化已说明
- [ ] Codex 生成部分已标注
- [ ] 无 Secret、临时凭据和本地配置进入仓库
- [ ] 有明确回滚方式
```

## 8.3 合并前检查表

```markdown
- [ ] 验收条件全部勾选
- [ ] Reviewer 已批准
- [ ] CI 必需检查全绿
- [ ] 安全或 Schema 高风险项已双人评审
- [ ] 文档已同步
- [ ] Evidence 已附到 Issue/PR
- [ ] Project 状态进入 Verify
- [ ] 验收人已实际执行关键路径
```

## 8.4 日终检查表

```markdown
- [ ] 今日 Issue 状态已更新
- [ ] 测试命令和结果已记录
- [ ] 阻塞已登记并指定负责人
- [ ] 未完成任务已拆分
- [ ] Reviewer 已完成第一轮反馈
- [ ] 明日任务已满足 DoR
- [ ] 演示或测试证据已归档
```

---

## 结论

该计划将 LabWeaver 的课程目标、生产级技术边界、GitHub Scrum 治理和 Codex Agent 协作方式统一到一个可执行的两周工作流中。四名成员每天都有明确 Owner、Issue、分支、PR、验收命令和证据要求；每个 Sprint 都有可运行的退出门禁；范围冻结、降级顺序和 Release Blocker 则保证团队在有限投入下优先完成真实 KubeVirt、统一评测、安全接入、自动化测试和可重复部署的核心闭环。
