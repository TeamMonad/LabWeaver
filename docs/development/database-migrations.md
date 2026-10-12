# 数据库迁移

六个服务使用同一 PostgreSQL 数据库中的领域 Schema。迁移源码在 migrations；运行时不自行执行 DDL、修复未知 Schema 或删除旧数据。

v3 面向空数据库验证当前初始化路径，同时更新所有仓库调用者，不保留旧接口兼容层。对既有部署的数据清理和离线切换须作为明确维护动作执行，不属于本轮本地代码任务。

## 一致性与权限

领域业务、防重和 Outbox 在同一事务提交。跨 Schema 操作只有存在具体原子不变量时才使用指定协调入口及受限权限；不由任意服务复制别的领域 SQL，不使用共享超级账号。

服务配置只包含凭据文件引用，不能将数据库密码写入命令行、仓库或日志。迁移错误、未知结构或部分初始化须明确报错；服务启动不能把失败转换成成功就绪。

## 本地检查与恢复

对一次性本地 PostgreSQL 执行迁移、关键约束、事务失败与并发行为测试。共享数据库变更需要检查所有受影响服务。

以下测试通过 Testcontainers 启动一次性 PostgreSQL，检查完整迁移和领域访问边界，需要本机 Docker 可用：

```sh
cargo test -p persistence-sqlx --test postgres_integration bootstrap_migrate_and_enforce_domain_boundaries --locked
```

Project 是幂等与归属的基本范围。可选 Course 不得作为普通唯一索引中唯一防重的组成条件，否则无课程记录可能绕过 PostgreSQL 的唯一约束。一次执行尝试的 TaskRunId 必须持久化且唯一，恢复时复用该身份；不得在读取时生成替代 ID。

若初始化失败，检查实际事务终态并修复原因；仅可重建明确属于本地测试的空数据库。对已有业务数据不自动 drop，不以修改已应用记录隐藏结构差异。代码回退和数据库恢复是不同操作，PR 应分别说明其影响。

## Credits migration

`resource/0016_project_credits.sql` 新增账户、预占和不可变账目。它不修改旧费用，不创建初始余额，不清空数据。catalog 必须包含此文件的 SHA-256。

Resource 0015 由 #200 的 `0015_task_approval_deadline.sql` 占用。组合时保留 0015，再加入 0016。Control 迁移编号属于另一领域。catalog 使用各领域有序并集。

迁移后验证账户约束、双边操作提交约束、账目 UPDATE/DELETE/TRUNCATE 拒绝，以及并发预占。运行时账号沿用现有领域边界，不增加生产凭据权限。

有真实账目后优先前滚修复。不要删除新表或覆盖历史账目。需要回退应用时先停止新增消费，保留三张表与幂等记录，再由维护者核对未结算预占。回滚不能让旧的费用计算路径继续接受付费消费。

## SQLx 版本固定

仓库暂时将 SQLx 全部子 crate 固定到上游提交 `f1e94ec34dd5f11ad6488a3a8ac7988439a3f757`。该提交包含 PostgreSQL 取消 `BEGIN` 时的事务回滚修复（上游 #4394）；当前 crates.io 的 0.9.x 发布版尚未包含此修复。Cargo workspace 使用同一个 git revision，避免 `sqlx`、`sqlx-core` 和各数据库 driver 版本分裂。

上游发布确实包含 #4394 的正式 SQLx 0.9.x 版本，并且该版本通过 `cancelled_begin_postgres` 真实 PostgreSQL 回归后，维护者才可以将根 `Cargo.toml` 的 git 依赖切回对应的 crates.io `version`，重新生成 `Cargo.lock`；不得仅因版本号发布就切回。

在上游 #4445 修复发布前，生产代码禁止在已有 `Transaction` 上再次调用 `begin()` 或 `begin_with()` 创建嵌套事务。SQLx 0.9 的 PostgreSQL driver 虽已包含 #4394，但嵌套 begin 取消仍可能回滚外层事务。根目录 `clippy.toml` 通过 `disallowed_methods` 拦截 `sqlx::Acquire::begin`、`sqlx::Connection::begin` 和 `sqlx::Connection::begin_with`；`sqlx::Pool::begin_with` 是从 Pool 获取新连接并开启事务的固有入口，不属于嵌套事务。新获取的 PoolConnection/PgConnection 开始事务的审计例外必须用 `#[expect(clippy::disallowed_methods, reason = "fresh connection, not nested")]` 标注。上游修复 #4445 并完成回归验证后，才可以解除这些限制。

git 依赖扩大了供应链风险：构建必须固定完整 commit，不得改用可变分支或 tag；更新前必须复核上游 commit、依赖源码、许可证和 Cargo.lock 的 source/revision，并重新运行取消 BEGIN 的真实 PostgreSQL 回归及 workspace 门禁。维护者应在 PR 中记录复核结果。

离线或镜像站构建必须在进入 `--network=none` 的编译步骤前完成 SQLx git revision 的 `cargo fetch --locked`，并将同一 Cargo git 缓存挂载到编译步骤；构建环境还必须能访问 `github.com` 获取该仓库，或由维护者预先准备并复核完整缓存/ vendored 源码。只挂载 registry 缓存不足以构建 git 依赖。

## Sprint5 成员真源切换

#199 将成员事实移到 Control，并保留 Control `0017_object_authorization.sql`、`0018_membership_governance.sql` 和 `0019_runtime_authorization_reads.sql`。组合 #197 的 `0014`、#203 的 `0015`/`0016` 后，按 catalog 的有序并集生成部署输入。Access 只获得 Control 成员表的读取权限。

此次硬切修改旧成员建表身份，只适用于新建数据库。已记录旧校验值的数据库必须拒绝 catalog 不一致。应用启动不能自动清空或重新授权。生产切换另需明确窗口和维护者批准。授权语义见[对象动作与成员授权](../contracts/object-authorization.md)。
