# LabWeaver

LabWeaver 是面向教学实验和科研工作的 Agent 驱动云原生实验平台。

当前仓库处于 Sprint 1 Foundation 初始化阶段。设计基线位于 `docs/draft/`；实际完成度仅以 `docs/status/implementation-status.md`、当前提交和可复现测试证据为准。

## 开发入口

```sh
cargo xtask check
```

服务启动前必须显式设置 `LABWEAVER_BIND_ADDR`。缺失或非法配置会阻断启动，不会自动回退到隐式地址。

## 文档入口

- `docs/requirements/README.md`：可测试需求基线、用户旅程、3C 用户故事和验收项；
- `docs/architecture/c4.md`：系统上下文与容器边界；
- `docs/architecture/service-boundaries.md`：服务职责和依赖规则；
- `docs/architecture/data-ownership.md`：权威数据所有权；
- `docs/contracts/environment-lifecycle-v1alpha1.md`：Experiment / Work 环境生命周期正式契约（拟议，未实现）；
- `docs/adr/0001-environment-lifecycle-domain.md`：环境生命周期跨域决策（待人工评审）；
- `docs/adr/0003-nats-subject-and-delivery-contract.md`：NATS Subject 与投递语义；
- `docs/contracts/nats-event-contract-v1.md`：NATS v1 事件目录；
- `docs/status/implementation-status.md`：实现状态事实源；
- `docs/testing/test-plan.md`：测试和证据计划；
- `docs/process/scrum.md`：GitHub Scrum 操作约束；
- `docs/deployment/ansible.md`：Kubernetes 基础设施的私有配置、预检、部署、验证和备份流程。
