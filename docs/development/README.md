# 开发与测试

使用仓库固定的 Rust 工具链、Python、Node 和 pnpm。数据库集成测试使用真实 PostgreSQL；本地外部依赖可通过 Docker 提供。不要为了普通代码验证连接共享集群。

## Issue 与 PR

Sprint5 按 [重构方案](../plans/sprint5-refactor-plan.md) 的细分 Issue 推进。开始前确认前置依赖，读取受影响代码并保护已有改动。从最新 develop 创建功能分支，PR 关联所完成的 Issue。

每个 PR 包含一个可评审的纵向切片，并更新契约、调用方、实现、必要测试和文档。删除被替代的无人消费路径，不增加无用兼容层。评审按变更需要选择维护者，不固定人员。

## Codex 与 OMP/Grok 分工

Codex 负责六服务后端、共享 crate、数据库、契约与生成 SDK、部署和集成验收。OMP 中的 Grok 负责前端组件、路由、状态展示、交互、前端测试，以及用户手册中的界面操作。Codex 复核手册中的权限、费用、持久性和部署语义。该分工不改变产品自身的 Claude Code 后端。

跨层 Issue 不拆成两套业务规则。GitHub 使用 `executor:codex`、`executor:omp-grok` 标明执行范围，允许同一 Issue 同时具有两个标签。标签不替代维护者评审，也不代表任务已启动。

1. Codex 在 Issue/PR 中确定接口输入输出、权限、状态、错误和幂等语义，更新生成契约并给出对应提交。
2. 给 OMP/Grok 指定具体 Issue、契约提交、允许修改的文件和验收条件。界面布局可先做，接入不得依靠另建模拟业务状态机。
3. OMP/Grok 在前端范围实现并运行受影响检查，提交可交接的差异。发现接口缺口时列出缺口，交回 Codex 修改真源。
4. Codex 整合双方修改，覆盖实际前后端流程。各自通过检查不等于跨层 Issue 已完成。
5. 维护者评审后合入 develop，全部行为条件完成后关闭 Issue。

默认串行交接同一工作树。并行开发使用独立分支/工作树，约定文件所有权。不要并发修改生成 SDK、锁文件、共享配置或同一份文档。

OMP 启动前用 `omp models find grok` 查询具体模型，以明确的 `--model` 指定 provider/model，并禁用自动模型切换。目录列出模型不代表已确认认证、额度或推理可用。本轮分工调整不启动模型任务，不改变全局模型配置。

功能合入 develop。阶段稳定后由维护者决定合入 main 和发布。作者不自行批准、合并或发布。

## 文档随功能交付

修改用户行为时更新使用手册和 API 示例。修改配置、依赖或运行方式时更新部署运维手册。只写实际可用命令，使用项目相对路径和 sh 示例，不提交真实凭据。

使用 ste-writing 的一致术语、主动语态和一步一动作原则整理文档。中文不机械套用英文词数。每个操作写清前提、步骤、预期结果、失败后的下一步，以及费用或数据影响。

复用实际用户流程、安装和恢复检查验证文档，不增加重复证明报告。完整文档是 Sprint5 完成条件。

## 增量验证

```sh
cargo fmt --all -- --check
cargo check -p contracts --locked
cargo test -p contracts --locked
cargo clippy -p contracts --all-targets -- -D warnings
pnpm --dir web lint
pnpm --dir web typecheck
pnpm --dir web test
```

按改动替换 Rust 包名。修改公共契约后重新生成 Schema、OpenAPI 和 Web SDK，不手改生成文件。契约、权限、持久化与计量变更扩大本地集成检查；遇到构建锁等待，不启动另一套竞争构建。

### Agent 镜像导入测试超时

本地需要 Docker。测试为每个 fixture 启动独立 PostgreSQL，只替换外部对象存储和镜像仓库。

1. 记录失败的 PR head、CI checkout SHA、run 和 job。其他提交的通过不能消除该失败。
2. 运行受影响测试，保留失败输出。

   ```sh
   cargo test -p agent-service --test platform_image_import_postgres --locked -- --nocapture
   cargo test -p agent-service --test platform_image_jobs_postgres --locked
   ```

3. 从 `case` 找到失败子例。用 `upload_id` 关联 worker 事件，再用 `claimed` 的 `artifact_id` 关联阶段事件。
4. 检查 `admission`、`object_read`、`oci_verify`、`disk_verify`、`disk_wrap` 和 `registry_publish` 的耗时。`scope_closed` 只表示阶段观察结束，不表示成功或后台阻塞任务已停止。
5. 超时后检查 `worker_finished` 和 `job_facts`。数据库快照依次为状态、revision、租约存在、数据库时钟判断的有效性、剩余毫秒和最近更新距今毫秒。`last` 是最近 HTTP 观察，可能早于快照。无法读取时标记 `unavailable`。

fixture 使用现有安全 JSON 日志格式，只输出固定阶段、身份、稳定诊断码和耗时。不要输出私有请求、对象键、路径、凭据或租约 token。保留原 15 秒终态等待、串行导入锁和 worker 的 abort/join。每次 helper 启动新 worker，立即处理一个已入队任务。60 秒轮询间隔避免它在退出前发起额外空闲 claim，不改变终态期限。专用 worker 测试继续覆盖轮询恢复、续期和取消。

慢文件回归使用 3 秒租约和 4 秒同步文件写入，检查同一次 claim 达到失败终态且没有发布。`FakeObjects` 在阻塞线程创建文件，状态查询和续期仍由异步 runtime 执行。该回归证明 fixture 行为，不能替代原始 CI 根因证据。生产排空与租约接管问题归 #229 协调。

## 本地运行

本地业务使用真实六服务、数据库、NATS、对象存储和身份服务。模型及 Kubernetes 等外部执行边界允许替换，前端不能再维护另一套环境、审批或评测状态机。

一次性 Kind 环境使用[本地六服务入口](local-stack.md)，由同一个命令管理启动、状态和清理。

服务配置显式加载，Secret 通过文件提供，缺少必需配置直接报错。测试同样执行真实身份、范围、状态及结果检查，不通过开发模式关闭正确性校验。

## 验证原则

优先覆盖用户行为、输入约束、负向权限、幂等、取消、恢复和资源释放。删除仅证明文件、类型或旧报告存在的测试；保留部署渲染后的实际权限和网络检查。

测试输出用于排障和 PR 说明，不形成运行时准入报告。真实 GPU、KubeVirt、外部连接和集群部署未运行时必须写清未验证，不能以边界 Mock 代替。

代码、配置和部署工具均使用项目相对路径。不要提交私有配置、令牌、用户提交、完整日志或生成的测试产物。具体数据库入口见 [数据库迁移](database-migrations.md)。
