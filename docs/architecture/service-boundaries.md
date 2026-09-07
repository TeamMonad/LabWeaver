# 服务边界

六个业务服务独立构建、部署，共享具体基础设施实现。Worker 是所属领域的进程角色，不形成新的业务服务。本文定义 v3 目标边界，实施进度在 Issue #180 的 PR 中维护。

| 服务 | 权威职责 | 不负责 |
| --- | --- | --- |
| Control | 项目、课程、材料、模板版本、完整实验包发布批准 | 环境运行、资源分配、确定性评分 |
| Access | 用户与服务身份接入、成员范围授权、访问授予、会话与撤销 | 环境调度、课程主数据、租约状态 |
| Environment | 环境实例、配置操作、端点、运行资源和回收 | 资源审批、费率、成绩 |
| Agent | 意图解析、隔离候选生成、配置和镜像构建验证、Work 排障 | 自行发布共享实验、确定成绩、平台管理权限 |
| Evaluation | 提交收集、冻结文件和实时 Probe、评测任务、确定性评分与反馈 | 第二套环境平台或发布审批 |
| Resource | 资源目录、配额、管理员审批、租约、GPU 分配授权、用量与费用 | 环境启停、软件配置、支付平台 |

Project 是基本归属，Course 为可选教学关联。Work 与 Experiment 使用同一 Environment 生命周期；用途、执行后端、资源与权限组合表达。一次性任务不需要创建长期环境。

Resource 持久化分配决定和租约；Environment 唯一管理环境 Namespace、Quota、PVC、容器与 VM 对象。Evaluation 管理其获授权执行范围内的 Job。不得由两个服务竞争写同一 Kubernetes 对象字段。

同步查询和即时授权直接调用所属服务。长任务使用所属领域的持久状态、Outbox、幂等与条件更新。复用现有操作 DTO、SSE 和页面组件，不增加 Operation Service 或通用 Saga。

内部 HTTP 通过 Keycloak 服务身份和 TLS，用户范围授权由 Access 独立判断。NATS、SSH、OIDC 和工件完整性按各自实际用途保留，不因删除开发证明机制而取消。

实验包批准与任务执行见[批准的实验包与执行](approved-execution.md)，GPU 分配和费用规则见[资源与计费](resource-accounting.md)。
