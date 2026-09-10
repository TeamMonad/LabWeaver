# 开发与测试

使用仓库固定的 Rust 工具链、Python、Node 和 pnpm。数据库集成测试使用真实 PostgreSQL；本地外部依赖可通过 Docker 提供。不要为了普通代码验证连接共享集群。

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

## 本地运行

本地业务使用真实六服务、数据库、NATS、对象存储和身份服务。模型及 Kubernetes 等外部执行边界允许替换，前端不能再维护另一套环境、审批或评测状态机。

一次性 Kind 环境使用[本地六服务入口](local-stack.md)，由同一个命令管理启动、状态和清理。

服务配置显式加载，Secret 通过文件提供，缺少必需配置直接报错。测试同样执行真实身份、范围、状态及结果检查，不通过开发模式关闭正确性校验。

## 验证原则

优先覆盖用户行为、输入约束、负向权限、幂等、取消、恢复和资源释放。删除仅证明文件、类型或旧报告存在的测试；保留部署渲染后的实际权限和网络检查。

测试输出用于排障和 PR 说明，不形成运行时准入报告。真实 GPU、KubeVirt、外部连接和集群部署未运行时必须写清未验证，不能以边界 Mock 代替。

代码、配置和部署工具均使用项目相对路径。不要提交私有配置、令牌、用户提交、完整日志或生成的测试产物。具体数据库入口见 [数据库迁移](database-migrations.md)。
