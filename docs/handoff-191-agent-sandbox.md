# 交接：#191 沙箱化 Agent 创作与镜像目录

> 本文是任务交接记录，不是产品/架构文档；最终状态应以 PR 与 Issue #191 为准。
> 分支 `feat/191-agent-sandbox`（相对 `51a3618` 共 36 个提交，工作树干净，未推送、未合并）。

## 1. 目标与约束

- 按 Issue #191：沙箱化 Claude Code 创作（消费 #192 已合并的 Resource 准入 + Kubernetes 执行边界）、多 VM base/Harbor 基础镜像目录、管理员镜像上传与目录审计。
- 遵守 `AGENTS.md`：纵向切片、每域唯一权威实现、无消费者兼容层应删除、稳定 diagnostic、不得以 Mock/占位/固定成功替代验证、不自行批准/合并/发布。
- 全部代码、测试、生成器修改由子 agent 执行（最多三个并行、文件所有权不重叠）；共享契约与迁移串行修改。ROLE A 负责协调与集成。

## 2. 已完成（按提交）

### 沙箱执行边界（`904f6a7`…`67cac91`）
- `crates/task-execution`：抽取共享一次性任务生命周期与 `AdmittedExecution`；`crates/contracts/src/supply_chain.rs`、`authoring.rs` 扩展。
- 迁移 `migrations/agent/0011_authoring_sandbox.sql`、`0012_dispatch_actor.sql`：attempt checkpoint、Job bundle、材料、结果/stderr、代次围栏、dispatch actor。
- `services/agent-service/src/sandbox.rs`、`sandbox_process.rs`：每 attempt 一个 Job，namespace `labweaver-authoring`，无 SA token、非 root、只读 rootfs、per-attempt NetworkPolicy + permanent `authoring-default-deny`。
- 准入先落 `TaskExecutionBinding` 再建对象；材料 DLP 后短时 URL 注入只读 `/materials`；authoring 6 工具/60 轮/`bypassPermissions`；advisory LLM review 保持进程内路径；旧 in-process authoring 硬切删除。
- 部署：Ansible/Helm 供给与 RBAC、ADR `docs/adr/0017-sandboxed-agent-authoring-and-image-catalog.md`。
- `b49c251`：崩溃恢复不二次执行（checkpoint 增 result_sha256/size/exit_code）。

### 镜像构建与导出闭环（`39672ec`…`d845cbd`）
- `oci_import.rs` 校验沙箱导出的 OCI layout；`oci_registry.rs` 按 digest 推送并读回确认（`OciRegistryPublisher::{publish,resolve_tag}`、`ResolvedRegistryImage`）。
- 契约：`BuildSource::{Dockerfile, ExportedOci}`；`ExportedOciImage { layout: ArtifactRef, layout_object_key }` 校验 `size_bytes>0`、`object_version`/`store_binding` 非空、key 为合法相对路径；`ExportedOci` 仅允许 `BuildNetworkPolicy::DenyAll`；`InternalAgentRunOutcome.environment_image_export`。
- 流水线 `Import` 阶段 + `ProductionBuildExecutor::import()`：校验→按 digest 推送→tag→`persist_built_candidate`。
- 导出链贯通：预签名上传、receipt export 字段、`freeze_current` 冻结复核、审计与 outcome 暴露；Control 有冻结导出即发 `BuildSource::ExportedOci` 并跳过 generated-context；VM 或重放缺失导出明确拒绝。
- prompt 约定（`claude_code.rs` `AUTHORING_SANDBOX_PROMPT`）：`BUILDKIT_HOST`、仅从 Harbor 拉取、`buildctl build --frontend dockerfile.v0 ... --output type=oci,dest=/workspace/labweaver-export.tar`。
- rootless BuildKit sidecar：可选 `sandbox.buildkit_image` + `sandbox.buildkit_config_map_name`（同时设置且需 `image_pull_secret_name`；部分绑定 fail closed）；namespace 打 `pod-security.kubernetes.io/enforce: privileged` + restricted audit/warn + `labweaver.io/security-exception: rootless-buildkit-no-process-sandbox`；sidecar 用 `seccompProfile`/`appArmorProfile` Unconfined、`SETUID`/`SETGID`、`seLinuxOptions: spc_t`、`allowPrivilegeEscalation: true`、`runAsUser/Group 1000`。
- 已知降级：崩溃恢复路径 `assemble_stored_result` 不携带 export（checkpoint 无 export 列），恢复后 Control 回退 Dockerfile 重构建。

### VM base 目录（`4c04422`…`d9588a3`）
- Control：`VirtualMachineBaseCatalog`（`providerBinding`/`storageClassBinding`/`maxBases`/`maxCapacityBytes`/`bases[]`），按 binding 解析，未知 binding/digest 漂移/重复 binding/超限 fail closed；`validate_authoring_artifact` 走 `config.virtual_machine_bases.resolve`。
- Environment：`KubeVirtBaseDiskBinding`（11 字段）+ `KubeVirtProviderConfiguration::new(trust, base_disks, ssh, budget)`；plan 对 digest/容量/格式漂移与未登记 binding 返回 `SecurityPostureInvalid`；DataVolume source/storage class/cloud-init 用户来自条目；`guest_user` 从 `KubeVirtSshBootstrap` 移除，改按 base 解析（`vm_guest_user` 要求所有条目一致）；注解 `labweaver.io/base-disk-sha256` 用 `disk_sha256`。
- 部署：`deploy/versions.lock.yml` `platform_vm_bases` 两条（ubuntu-24.04-v1 10Gi、cirros-0.6-v1 1Gi，artifact_id `...0003`/`...0004`）；Ansible 上限 `platform_application_vm_base_max_count: 8`、`platform_application_vm_base_max_capacity_bytes: 137438953472`、逐条校验、CDI loop、provider `baseDisks` 与 lock 逐条比对（`PLATFORM_VM_BASE_CATALOG_MISMATCH`）。
- `disk_sha256` 语义已实测：containerdisk 内磁盘文件 sha256（ubuntu `disk/disk.img`、cirros `disk/downloaded`）。

### 平台镜像目录与管理员 API（`210684c`、`9fcf37b`、`9fd56ed`）
- 迁移 `migrations/agent/0013_platform_image_catalog.sql`（+ `migrations/catalog.yaml` id 13）：`platform_image_catalog` + append-only `platform_image_catalog_audit`。
- `services/agent-service/src/platform_images.rs`：`PgPlatformImageCatalog::{register,repin,disable,active_list,list}`（digest compare-and-set、审计同事务、被引用条目只停用不删除）；`PlatformImageRegistry`（单一 registry host 限定、tag→digest、拒绝 index/未确认 digest）；`PlatformImageRegistryError`/`PlatformImageStoreError` 稳定诊断 `LW_PLATFORM_IMAGE_*`。
- Agent 内部 API（`api.rs`，沿用 `agent.control.invoke`）：`POST/GET /internal/v1/platform-images`、`/{id}/repin`、`/{id}/disable`；repin 服务端复用已存 reference（调用方不能换仓库），陈旧 digest 409；未配置 registry 时写操作 503、列表仍可用。
- 沙箱 prompt 每次 attempt 读取 `active_list()` 并追加 `binding (container|vm) @ sha256:…` 清单；advisory 路径空清单；目录读取失败 fail closed。
- API 模式部署新增可选 `platform_registry`（registry/ca/username/password 文件，启动时校验）；`deploy/config/agent-control-plane.yaml.example` 尚未加入该块。

## 3. 本轮验证结果（本地）

- `cargo check -p agent-service --all-targets --locked`、`cargo test -p agent-service`（53 lib + 集成，含 `platform_images_postgres` 2 项：目录真库测试 + 管理员 HTTP 全流程测试）、`cargo clippy -p agent-service --all-targets -- -D warnings`、`cargo fmt --all -- --check`、`cargo xtask contracts check` 全绿。
- 早前已跑绿：control-service（7 lib + 9 Postgres + messaging + agent_run_api）、environment-service（47 lib + provider 10 + postgres）。
- 未验证：kind/实机（sidecar uid/fsGroup/emptyDir 0777 假设、Unconfined seccomp 在 baseline/privileged 下可用性、Harbor CA 与 rootless 镜像 digest 绑定、真实构建导出与导入 digest 闭合、清理计数）；无集群时 `ProductionBuildExecutor::import()`/`publish` 无集成测试。

## 4. 剩余工作与下一跳

1. **Control 管理端网关（下一步优先）**
   - `crates/contracts/src/http.rs` 增目录请求/响应类型与 `OPERATIONS` 条目（管理员 scope），随后 `cargo xtask contracts generate` 并提交 `schemas/contracts/v1/**`。
   - `services/control-service/src/clients.rs` 增客户端方法转发 `/internal/v1/platform-images*`。
   - `services/control-service/src/api.rs` 增 `/api/v1/admin/images`（`PlatformRole::PlatformAdmin` 校验、被 release 引用影响提示）；引用计数来源为 Control 自己的 release 数据。
2. **Access 与 Web**：`services/access-service/src/main.rs:285 control_browser_router()` 增代理路由；Web 管理页（注册/重固定/停用 + 影响提示 + YAML 高级入口）。
3. **VM/容器 OCI tar/layout 上传与 lazy 导入**：接收上传→`oci_import::parse_oci_layout`→按 digest 推送 Harbor→`PgPlatformImageCatalog::register`；当前仅支持 registry reference。
4. **部署接线**：把 `platform_registry` 块加入 `deploy/config/agent-control-plane.yaml.example`，并把 `harbor-ca.crt`/`harbor-username`/`harbor-password` 加入 `deploy/config/platform-bundle-manifest.json` 的 `agent-service-secrets` 与 Ansible 供给。注意 `tests/ansible/test_fixtures.py:1116` 会扫描示例文件中的 `/etc/labweaver/secrets/...` locator（包括注释），新增路径必须先在 manifest 声明。
5. **kind/实机验证**：见第 3 节未验证清单；另外验证 prompt 清单注入生效、真实 Harbor tag 解析与重固定审计。

## 5. 环境与工具注意事项

- `/home` 磁盘满：统一 `CARGO_TARGET_DIR=/tmp/opencode/lw-target`，cargo 命令加 `--offline`。
- `cargo xtask contracts generate` 不接受 `--offline`；`contracts check` 可用。
- `tests/ansible` 无法运行（`python3.12: No module named pytest`），另有 3 项既有缺 `python-dotenv` 的环境性失败；属环境限制。
- 不得 amend 已成功提交；lint 修复用新提交。
- `services/agent-service/src/claude_code.rs` 的 `generate_authoring`/`generate_scoped` 已带 `platform_images` 参数；调用点 `run_store.rs` 两处从 `PgPlatformImageCatalog::active_list()` 读取，测试调用点在 `tests/claude_code_runtime.rs`。
- `PlatformImageRegistry::for_test` 为 `#[doc(hidden)]` 测试接缝（允许 http base），生产 `new` 强制 https；集成测试用它 + 假 registry + testcontainers。

## 6. 关键文件索引

- 契约：`crates/contracts/src/supply_chain.rs`、`http.rs`、`events.rs`、`authoring.rs`；`crates/contracts/src/schema.rs`（schema 清单）。
- Agent：`services/agent-service/src/{claude_code.rs,platform_images.rs,oci_registry.rs,oci_import.rs,build_executor.rs,build_pipeline.rs,sandbox.rs,sandbox_process.rs,api.rs,run_store.rs,main.rs}`。
- Environment：`services/environment-service/src/{kubevirt_provider.rs,process.rs}`。
- Control：`services/control-service/src/{lib.rs,clients.rs,api.rs}`。
- 迁移：`migrations/agent/0011…0013`、`migrations/catalog.yaml`。
- 部署：`deploy/versions.lock.yml`、`deploy/ansible/roles/platform_application/{tasks,defaults}`、`deploy/config/{control-plane,environment-providers,build-executor,agent-control-plane}.yaml.example`、`deploy/config/platform-bundle-manifest.json`。
- 测试：`services/agent-service/tests/{platform_images_postgres.rs,claude_code_runtime.rs,build_store_postgres.rs,build_pipeline.rs,service_auth.rs,support/mod.rs}`、`services/control-service/tests/{postgres.rs,agent_run_api.rs,messaging.rs}`、`services/environment-service/tests/{kubevirt_provider.rs,postgres.rs}`、`tests/ansible/test_fixtures.py`。
- ADR：`docs/adr/0017-sandboxed-agent-authoring-and-image-catalog.md`（含管理员 API 与部署待办说明）。
