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

## Sprint5 成员真源切换

#199 将成员事实移到 Control。`0016` 建立成员表，`0017` 保存课程职责和成员邀请，`0019` 提供现有运行策略、发布配置及完整批准的只读投影。组合 #197 的 `0014` 和 #203 的 `0015` 后，按 catalog 的有序并集生成部署输入。Access 只获得 Control 成员表的读取权限。

Access `0007` 在目录列创建后撤销 Control 的旧 actor 整表读取。它只授予 Control actor_id、username、display_name 和 disabled_at 四列。Resource 仅读取 actor_id、disabled_at 及当前会话安全投影。普通服务启动不得跳过这条迁移，也不得恢复更宽的账号读取。

此次硬切修改旧成员建表身份，只适用于新建数据库。已记录旧校验值的数据库必须拒绝 catalog 不一致。应用启动不能自动清空或重新授权。生产切换另需明确窗口和维护者批准。授权语义见[对象动作与成员授权](../contracts/object-authorization.md)。

## 取消 BEGIN 与连接复用

SQLx 0.8.6 在收到 BEGIN 应答后才登记事务深度。应答前取消 future 时，rollback guard 可能不发送 ROLLBACK。连接池的 ping 只排空协议，后续写入可能返回成功却仍留在未提交事务中。单独查询同一连接不能证明持久化。

仓库只对 sqlx-postgres 0.8.6 回移官方 [#4394](https://github.com/transact-rs/sqlx/pull/4394) 的事务补丁。driver 排队 BEGIN 或 SAVEPOINT 后立即登记新层级，再等待应答。取消时 guard 回滚该层级。正常连接归还不增加 ROLLBACK 往返。错误 BEGIN 可能产生无活动事务的 PostgreSQL warning。来源、校验和与移除条件见 [补丁说明](../../vendor/sqlx-postgres/PATCH.md)。

官方 [#4445](https://github.com/transact-rs/sqlx/issues/4445) 指出的较新 core guard 双重回滚没有进入此回移。sqlx-core 保持 0.8.6，其 Transaction guard 在 driver 完成 BEGIN 后才创建。失败 savepoint 的清理仍可能返回 missing-savepoint 错误。调用方须回滚已失败的外层事务，不能继续提交部分写入。

本地需要 Docker。运行确定性取消、业务持久化及恢复回归：

```sh
cargo test -p agent-service --test cancelled_begin_postgres --locked -- --nocapture
cargo test -p agent-service --test platform_image_jobs_postgres --locked
cargo test -p persistence-sqlx --locked
```

取消回归用隔离 PostgreSQL 和测试协议 relay 延迟真实普通 BEGIN 应答。只有服务端报告事务状态后，测试才取消 future 或通过生产 serve_tls/router 发送 h2 RST_STREAM。测试检查原 backend PID、独立 observer、嵌套 savepoint、错误 BEGIN、commit 和关池后的业务终态。HTTP 身份来自 fixture，不使用生产 JWT 或数据库。该结果证明可控条件下的调用链，不证明历史 CI 或线上事故的唯一根因。#234 的导入 fixture 修复仍由其独立 PR 交付。

此补丁不改 API、Schema、迁移或权限。需要回退时 revert 对应 PR。回退依赖补丁会恢复取消 BEGIN 的缺陷，不会修复已经丢失的历史写入。
