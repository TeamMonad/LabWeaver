# 管理员配置与应用事实

`crates/contracts/src/platform_configuration.rs` 定义非秘密平台设置。Control 是配置 revision、期望配置、应用尝试和已验证配置的唯一写入者。管理员表单和高级 YAML 使用同一更新函数。前端不能维护另一套应用状态。

本切片提供管理 API、持久化和生成 SDK。它不自动执行部署，也不实现 #207 的运行策略消费者。Secret 仍由加密 Bundle 提供，不进入此 API、配置表或普通日志。

## 权限与入口

公共入口使用已有 Access BFF 会话。Control 先验证 Access 服务 JWT，再向 Access 请求平台全局授权，最后检查 `platform_admin`。非管理员无法读取或修改配置。浏览器变更继续检查 Origin 和 CSRF。

| 方法与路径 | 生成 SDK | 行为 |
| --- | --- | --- |
| `GET /api/v1/admin/platform-configuration` | `getPlatformConfiguration` | 返回当前事实、初始模板和 ETag |
| `PATCH /api/v1/admin/platform-configuration` | `updatePlatformConfiguration` | 条件追加一个不可变 desired 版本 |
| `GET /api/v1/admin/platform-configuration/versions/{revision}` | `getPlatformConfigurationVersion` | 读取指定历史版本 |
| `POST /api/v1/admin/platform-configuration/applies` | `applyPlatformConfiguration` | 返回 202，记录应用意图，不执行部署 |
| `GET /api/v1/admin/platform-configuration/applies/{applyId}` | `getPlatformConfigurationApply` | 读取应用尝试及其失败事实 |

`previousApplyId` 链接上一次尝试。重试或刷新后，管理员仍能读取原失败尝试。配置 revision 是不连续递增值，不能用加一猜测下一条历史版本。

独立执行者使用 `PUT /internal/v1/platform-configuration/applies/{applyId}` 报告事实。该入口验证服务 JWT 的 issuer、audience、签名、期限、配置的客户端范围和 `control.platform_configuration.apply` 权限。Access 网关身份即使带该权限也不能报告。浏览器 SDK 不包含此内部入口。

本 PR 没有配置生产执行者身份或授予其权限。需要已有获准的执行身份和部署窗口，执行者才能调用此入口。缺少身份或权限时，入口明确拒绝。不要让浏览器持有部署服务 Token。

## 编辑同一配置

1. 调用 `getPlatformConfiguration`。
2. 如果 `desired` 存在，将其 `configuration` 复制到编辑草稿。
3. 如果尚未配置，将响应中的 `defaults` 复制到编辑草稿。
4. 修改完整配置。
5. 将当前响应的 `revision` 写入 `expectedRevision`。
6. 用当前 ETag 设置 `If-Match`。
7. 使用稳定的 `Idempotency-Key` 提交更新。

未配置时 revision 为 0，ETag 为 `"rev-0"`。JSON 和 YAML 必须包含同一个 `UpdatePlatformConfigurationRequest`：

```yaml
expectedRevision: 0
configuration:
  lifecycle:
    readOnlyRetentionSeconds: 604800
    detailedLogRetentionSeconds: 2592000
    expiryReminderSeconds: [86400, 3600]
    preemptionGraceSeconds: 60
  runtimePolicy:
    defaultContainerUid: 0
    allowedCapabilities: []
    preauthorizations: []
    privilegedAllowed: false
    privilegedExecutionLocation: null
  builderPolicy:
    capabilities: []
    allowPrivilegeEscalation: false
    entitlements: []
```

JSON 使用 `application/json`。高级 YAML 使用 `application/yaml`。两个入口接受同一结构和 revision 栅栏。未知字段、重复键和非法权限声明会失败，错误响应不回显输入。

更新替换整个非秘密配置。所有设置组及其必需字段都必须提供，避免省略 builder 或 runtime 设置时将其重置。前端先复制服务端当前值，再修改字段，不补本地业务默认值。

`If-Match` 与 `expectedRevision` 不一致时，响应为 412。服务端 revision 已变化时，响应为 409。此时重新读取当前配置，再比较和提交。不要静默覆盖另一位管理员的新修改。

相同幂等键和请求返回原结果。不同 actor 或请求不能复用该键。回放结果是原响应快照，前端不能用较小的 revision 覆盖已收到的较新事实。

## 设置与影响

默认值为 7 天只读保留、30 天详细日志、24h/1h 到期提醒和 60 秒抢占宽限。留存与提醒必须在 1 秒到 365 天之间。提醒最多 16 个且不能重复。抢占宽限必须在 1 到 300 秒之间。

这些值目前作为配置保存。相应数据清理、通知和抢占消费者尚未接入。保存配置不会删除文件、发送提醒、收取费用或停止 Work。缩短留存、修改运行策略或 builder 策略的实际影响，需要后续消费者和部署验证落实。

普通容器 capabilities 使用 [Linux UAPI 名称](https://github.com/torvalds/linux/blob/master/include/uapi/linux/capability.h)，不接受 `ALL` 或未知名称。项目/课程预授权不能超过平台允许清单，同一 scope 不能重复。特权准入必须声明专用节点池及其 selector/taint，或明确的 VM guest provider binding。仅声明 namespace 不构成执行位置。

配置解析只验证声明和范围，不能证明节点已经专用或 VM guest 可用。#207 消费者必须验证实际对象授权、执行位置、Pod Security 和调度边界。此接口不会放宽 namespace 标签。

`runtimePolicy` 与 `builderPolicy` 独立。更改特权运行或 runtime capabilities 不会增加 builder capabilities、提权或 entitlements。`impact` 比较 desired 与最后验证的 applied，分别提示运行、构建和生命周期设置变化。它不提供虚构的受影响资源数量。

## 应用与失败恢复

1. 取得当前 `desired.revision`。
2. 用获准方式准备对应加密 Bundle，并计算该密文文件的 SHA-256。
3. 调用 `applyPlatformConfiguration`，提供 aggregate 的 `expectedRevision`、当前 `desiredRevision` 和 `encryptedBundleSha256`。
4. 核对 202 响应中的应用尝试 ID 和 `nextStep`。
5. 由独立获准执行者使用现有部署入口执行。
6. 由执行者提交真实的阶段与观察事实。

管理版本已通过 [Bundle 导入/导出入口](../deployment/encrypted-bundle.md#管理设置导出与导入) 进入整体加密归档。导入生成同一 `UpdatePlatformConfigurationRequest`，使用目标安装的 revision，保留现有管理员鉴权与冲突语义。导入不复制源安装的 applied 或执行历史，也不自动部署。

派生管理版本提供给获准消费者。执行者必须核对所执行 Bundle 与不可变 desired 快照，不能只把请求摘要抄成已观察结果。配置保存不会自动改变所有服务的运行策略，消费者和真实执行观察器仍须接入后才能宣称配置已生效。

执行报告使用尝试自身的 `expectedRevision` 和对应 `If-Match`，而非 aggregate 的 revision。执行者首次报告后绑定其客户端身份。其他执行者、迟到报告、乱序阶段和不同请求的幂等键复用均会失败。

正常阶段为 `requested`、`validating`、`applying`、`verifying`、`succeeded`。执行者不能直接从 requested 报告 succeeded。成功报告必须同时匹配原 desired 的 canonical JSON 摘要和原加密 Bundle 摘要，才更新 applied、验证时间和 Bundle 观察值。

执行期间允许管理员保存新 desired。当前尝试仍绑定原 desired。原尝试成功后，applied 指向原版本，新 desired 继续保留为未应用期望。

失败报告使用有界 `failureReason`，并保留 `failedStage` 和 `nextStep`。不得上传 Token、私钥或原始执行日志。旧版本和旧尝试在数据库中保留，重试不会覆盖它们。

| 字段或状态 | 管理员看到的含义 |
| --- | --- |
| `appliedState: unconfigured` | 尚无完整验证结果 |
| `appliedState: verified` | 最近一次完整验证通过 |
| `appliedState: unverified` | 当前外部结果待核对，applied 只是最后验证版本 |
| `fix_configuration_and_retry` | 修复配置或依赖，再提交新尝试 |
| `reconcile_external_result` | 先核对外部真实状态，再继续获准执行 |
| `execute_approved_deployment` | 等待既有获准部署入口执行 |

Applying 或 verifying 阶段失败后，applied 保留最后验证版本，同时状态变为 unverified。新尝试不会清除此状态，只有匹配事实的完整成功验证才能清除。API 不自动回滚集群、数据库迁移或远端明文材料。

## 持久化与部署顺序

`control/0015_platform_configuration.sql` 只创建新表，不修改上传、任务或旧业务数据。编号 0014 留给 #197。部署前先组合完成 #197/#203 的 migration catalog，再按统一递增顺序执行。不要在后续还需补 0014 的环境中先执行 0015。

本 PR 没有在远端执行迁移。故障时先查看实际 migration ledger 和目录，不在服务启动时修补未知 schema，也不删除配置历史。

## Grok 接入

Grok 使用上述生成 SDK 和真实服务端事实。界面显示当前 desired、最后验证 applied、appliedState、失败类别、下一步和设置影响。表单与 YAML 共用同一 revision，不手改生成文件。

界面先说明中文原因和恢复动作，再允许复制 code/requestId。失败后保留草稿，冲突后让管理员比较新旧修改。应用状态显示为真实阶段，未知进度不显示百分比。只在破坏性重启或资源影响明确后安排确认，不把保存草稿当部署成功。

Grok 可将版本 API 的完整 JSON 交给获准导出操作。导入文件使用现有更新 SDK，先核对目标安装当前 revision，冲突时保留草稿。部署服务身份不交给浏览器。age 私钥由操作员独立持有，不进入配置 API。生成 SDK 继续从真源产生。

尚未完成的范围：管理界面与真实后端的集成验证、Resource/基础设施 profile 整体归并、首装管理版本初始化、实际部署观察器、默认值消费者，以及 #207 运行授权与执行边界。#203 保持开放。
