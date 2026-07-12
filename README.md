# LabWeaver

LabWeaver 是面向教学实验和科研工作的 Agent 驱动云原生实验平台。

当前仓库处于 Sprint 1 Foundation 初始化阶段。设计基线位于 `docs/draft/`；实际完成度仅以 `docs/status/implementation-status.md`、当前提交和可复现测试证据为准。

## 开发入口（planned，pending PR #21）

当前分支尚未包含 Rust workspace、`Makefile` 或服务进程。以下命令属于待合并的 PR #21，不得在当前提交中视为可执行入口：

```sh
make check
```

PR #21 合并并在目标提交重新验证前，`LABWEAVER_BIND_ADDR`、服务启动和健康端点均为 planned。

## 文档入口

- `docs/architecture/c4.md`：系统上下文与容器边界；
- `docs/architecture/service-boundaries.md`：服务职责和依赖规则；
- `docs/architecture/data-ownership.md`：权威数据所有权；
- `docs/status/implementation-status.md`：实现状态事实源；
- `docs/testing/test-plan.md`：测试和证据计划；
- `docs/process/scrum.md`：GitHub Scrum 操作约束。
