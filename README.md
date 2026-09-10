# LabWeaver

LabWeaver 是 Agent 原生的教学与科研工作环境平台。六个独立服务共同提供实验创作、容器和虚拟机、科研 Work、访问、统一评测以及资源与费用管理。

v3 重构由 [Issue #180](https://github.com/TeamMonad/LabWeaver/issues/180) 跟踪。目标能力不等于已经通过实机验证；实际变更、测试和未验证范围以关联 PR 为准。

## 开发

使用仓库固定的 Rust 工具链与前端 package scripts。优先运行受影响范围：

```sh
cargo check -p control-service --locked
cargo test -p contracts --locked
pnpm --dir web typecheck
pnpm --dir web test
```

启动真实业务服务需要显式配置数据库、OIDC、消息与对象存储；模型和集群执行可以在外部边界替换。缺少配置必须明确失败，不启动模拟业务产品。

一次性 Kind 开发环境的准备、启动和清理见[本地六服务开发环境](docs/development/local-stack.md)。

## 文档

- [开发约定](AGENTS.md)
- [服务边界](docs/architecture/service-boundaries.md)
- [数据所有权](docs/architecture/data-ownership.md)
- [资源占用与费用](docs/architecture/resource-accounting.md)
- [批准的实验包与执行](docs/architecture/approved-execution.md)
- [访问边界](docs/architecture/access-trust-boundary.md)
- [v3 架构决定](docs/adr/0015-v3-project-work-resource-platform.md)
- [开发与测试](docs/development/README.md)

应用部署与基础组件安装分开。真实 KubeVirt、GPU 和共享集群验证需要对应环境；普通本地测试不能替代实机验证。不得将凭据或用户提交内容放入仓库。
