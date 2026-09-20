# ADR 0017：沙箱化 Agent 创作与镜像目录

状态：Issue #191 的实施契约；实现与测试见该 Issue 的 PR。人类架构/安全 Review 由 A+B 完成，行为 Verify 由 D 完成。本 ADR 取代 [ADR 0007](0007-claude-code-agent-runtime.md) 中“禁用全部工具并在服务进程内单轮执行”的运行时约束；Claude Code 唯一后端、DLP 出站闸门、候选 Schema/预算/审计契约保持不变。

## 背景

#127 真实验收显示，服务进程内 `--max-turns 1 --tools ""` 的单轮模式无法读文件、执行命令或构建自查，所有失败只能靠平台兜底或人工提示修复；大包还会耗尽输入预算。ADR 0016 已冻结 Resource 准入与统一 Kubernetes 执行边界，并声明 #191 的 Agent sandbox 必须复用该边界，不得形成第二套执行体系。同时 VM 基础镜像被单模板硬绑，镜像/tag 选择缺少目录与审计。

## 决定

### 沙箱执行

- 每次 authoring attempt 一个 Resource Task 租约与一个 Kubernetes Job，命名空间固定为 `labweaver-authoring`，managed-by 为 `agent-service`。Job 使用不可变镜像摘要、非 root、只读 rootfs、无 ServiceAccount token、per-attempt NetworkPolicy 与 namespace 级 permanent default-deny。
- 准入由 `TaskExecutionBinding` 证明：Active 请求 + HandedOff claim + Active lease + 一致 namespace/revision 才能构造；绑定在创建任何集群对象前持久化到 `agent.authoring_sandbox_attempts`。无有效绑定不得创建 workload。
- 分类后的材料经 DLP 闸门后以短时对象存储 URL 注入 init container，并解包到主容器只读 `/materials`；模型在可写 `/workspace` 内使用本地工具（Bash/Read/Write/Edit/Glob/Grep）迭代，最终仍只返回一个候选 JSON。
- 模型版本在 attempt 内通过 `claude --version` 回执校验；结果与有界 stderr 经对象存储回传并按回执哈希校验。业务结果先持久化，再清理执行对象，最后释放 Resource；清理未确认不得释放，重启恢复只清理既有对象、绝不二次执行。
- 硬切：authoring 不再有服务进程内执行路径，配置缺失即启动失败。advisory LLM review 保持无 Resource 准入的进程内路径（ADR 0016 已声明），不参与沙箱准入。

### 构建权威

- 沙箱内 BuildKit 只使用内置 Harbor 中已上传的基础镜像构建，产出 OCI layout/tar 导出到对象存储；构建权威仍在 `build-executor`：导入前先在同一边界完成 OCI 校验（条目/路径/媒体类型/大小/逐 blob digest），再按 digest 推送 Harbor 并注册 `agent.image_artifacts`。沙箱不持有 Harbor 推送凭据。
- 漏洞扫描不重新进入服务契约：ARC-09（#176）已删除 `ImagePolicyEvaluation`/扫描链，本决定不恢复。需要扫描时使用 Harbor 自身在部署侧配置的扫描能力；是否阻断发布由部署策略决定，不产生第二套服务端扫描判定。
- 不引入第二套构建或镜像权威；外部 registry 引用必须先经管理员导入并固定 digest。

### 工具策略与审计

- 每条执行路径的 `tool_policy_sha256` 与其真实策略一致：authoring 为工具启用、60 轮、`bypassPermissions`；advisory review 保持工具禁用单轮。审计保留 prompt/schema/input/output/工具策略/运行身份哈希、用量、取消/超时结果与稳定诊断，不保留原始材料、模型输出或 stderr 正文。

### 镜像与模板目录

- 平台维护内置 Harbor 基础镜像清单与 VM base 目录；`deploy/versions.lock.yml` 的 VM base 由单条改为列表，provider 配置以 `baseDisks[]` 按 binding 解析，digest/容量/`disk_sha256`/格式必须与条目一致，否则 fail closed。
- tag 只在导入/发布边界解析一次并落库；管理员显式“重新固定”并留 append-only 审计，下游只认 digest，不自动跟随 tag。管理员上传 OCI tar/layout 或 registry reference，经校验、配额与影响提示后进入目录；被 release 引用的条目只能停用不能删除。

## 代价与边界

- Agent 服务进程需要访问 Kubernetes API（namespace 级 Role，仅该 namespace 的 Job/Secret/Pod/NetworkPolicy），并持有模型凭据；沙箱通过 NetworkPolicy 与无推送凭据的容器限制其能力。真实 gVisor/Kata 隔离不在本轮部署范围，需实机验证；本轮验证 runc + 容器级 seccomp/capability 限制。
- 每次 attempt 进入 Resource 审批链；批量审批 UI 已存在，但高频 authoring 的预授权策略是后续产品决定，本 ADR 不引入自动审批。
- rootless BuildKit 需要受控的 seccomp/capability 例外（`seccompProfile`/`appArmorProfile` Unconfined、`SETUID`/`SETGID`、`seLinuxOptions: spc_t`），与既有 `platform_buildkit` 角色同类。`labweaver-authoring` 命名空间按既有 builder 的方式使用 `pod-security.kubernetes.io/enforce: privileged` 与 restricted 审计/告警，并标注 `labweaver.io/security-exception`；例外只落在无 token、资源受限、仅可经 attempt 本地 socket 访问的 sidecar 上。该能力当前通过可选 `sandbox.buildkit_image`/`sandbox.buildkit_config_map_name` 启用，部署侧仍需把 rootless 镜像与 Harbor CA ConfigMap 绑定进该命名空间并联调。

## 验证边界

本轮只做代码与本地验证：PostgreSQL 使用 testcontainers，Kubernetes API 在业务链测试中使用进程内 TLS mock，真实 Job 的提交/观察/取消/清理在本地 kind 验证。不执行远端部署、不操作共享集群、不真实扣款。真实 GPU/KubeVirt 能力需对应硬件验证。
