# 管理员配置与应用事实

`crates/contracts/src/platform_configuration.rs` 定义非秘密平台设置。Control 是配置 revision、期望配置、应用尝试和已验证配置的唯一写入者。管理员表单和高级 YAML 使用同一更新函数。前端不能维护另一套应用状态。

本切片提供管理 API、持久化和生成 SDK。它不自动执行部署，也不实现 #207 的运行策略消费者。管理设置不包含 Secret。Bundle 登记入口只接收 age 密文，流式计算摘要后丢弃上传内容，配置表只保存绑定元数据。解密身份、明文 Secret 和执行日志不进入此 API。

## 权限与入口

公共入口使用已有 Access BFF 会话。Control 先验证 Access 服务 JWT，再向 Access 请求平台全局授权，最后检查 `platform_admin`。非管理员无法读取或修改配置。浏览器变更继续检查 Origin 和 CSRF。

| 方法与路径 | 生成 SDK | 行为 |
| --- | --- | --- |
| `GET /api/v1/admin/platform-configuration` | `getPlatformConfiguration` | 返回当前事实、初始模板和 ETag |
| `PATCH /api/v1/admin/platform-configuration` | `updatePlatformConfiguration` | 条件追加一个不可变 desired 版本 |
| `GET /api/v1/admin/platform-configuration/versions/{revision}` | `getPlatformConfigurationVersion` | 读取指定历史版本 |
| `POST /api/v1/admin/platform-configuration/versions/{revision}/bundle` | `registerPlatformConfigurationBundle` | 接收密文，计算实际摘要并绑定当前 desired |
| `GET /api/v1/admin/platform-configuration/versions/{revision}/bundle` | `getPlatformConfigurationBundle` | 读取该设置版本最后一次登记；无登记返回404 |
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

1. 取得当前 desired 版本、设置摘要和 aggregate ETag。
2. 在受限本地目录导出、验证并加密对应 Bundle。独立持有解密身份。
3. 用 `registerPlatformConfigurationBundle` 上传实际 `.age` 文件。路径使用 `desired.revision`，query 使用 `expectedRevision` 和 `configurationSha256`；提供对应 If-Match、稳定幂等键与既有浏览器 Origin/CSRF。SDK 的 body 为 `Blob | File`，Content-Type 为 `application/octet-stream`。
4. 核对响应中 `registeredBundle` 与本地导出的摘要。登记会推进 aggregate revision，不创建应用尝试或 applied。丢失响应时用相同文件与幂等键重试，或读取当前事实。
5. 调用 `applyPlatformConfiguration`，只提供最新 aggregate 的 `expectedRevision` 和当前 `desiredRevision`。Control 使用已登记绑定；不接受调用方另填密文摘要。
6. 核对 202 响应中的应用尝试 ID 和 `nextStep`。
7. 由独立获准执行者使用现有部署入口执行，再提交真实阶段与观察事实。

登记只记录收到的密文字节摘要、长度、声明的设置绑定、登记者和时间。服务端不持解密身份，无法验证未解密内容是否符合声明，也不保管可供恢复的归档。操作员须保留原文件及独立密钥，并先在本地完成同源校验。age 前缀检查不能替代解密完整性校验。`seal` 成功或登记成功均不表示生产部署已经获准。管理员应用请求沿用现有角色授权；没有第二套审批状态机或自动执行器。

上传上限为70MiB，包含现有64MiB源目录、tar与age开销。流中断、格式前缀错误、超限或绑定冲突不改变已登记事实。服务端要求声明的设置摘要和版本与当前 desired 完全相同，使用 aggregate fence 拒绝并发覆盖。同一幂等键不能更换文件、绑定或操作者。

`registeredBundle` 只指向当前 desired 最后登记的归档。保存新 desired 后该字段为空，必须重新导出并登记。按历史 desired revision 查询仍可读取最后登记事实。重新登记不会改变在途尝试绑定的原摘要；其结果与当前新登记分别保存。登记 GET 的 ETag 是登记事实 revision，应用请求应使用当前 status 的 aggregate revision。

管理版本已通过 [Bundle 导入/导出入口](../deployment/encrypted-bundle.md#管理设置导出与导入) 进入整体加密归档。导入生成同一 `UpdatePlatformConfigurationRequest`，使用目标安装的 revision，保留现有管理员鉴权与冲突语义。导入不复制源安装的 applied 或执行历史，也不自动部署。

派生管理版本提供给获准消费者。执行者必须核对所执行 Bundle 与不可变 desired 快照，不能只把请求摘要抄成已观察结果。配置保存不会自动改变所有服务的运行策略，消费者和真实执行观察器仍须接入后才能宣称配置已生效。

执行报告使用尝试自身的 `expectedRevision` 和对应 `If-Match`，而非 aggregate 的 revision。执行者首次报告后绑定其客户端身份。其他执行者、迟到报告、乱序阶段和不同请求的幂等键复用均会失败。

正常阶段为 `requested`、`validating`、`applying`、`verifying`、`succeeded`。执行者不能直接从 requested 报告 succeeded。成功报告必须同时匹配原 desired 的 canonical JSON 摘要和原加密 Bundle 摘要，且与 Control 本进程实际加载的快照一致，才更新 applied、验证时间和 Bundle 观察值。没有加载快照、旧进程仍加载旧版或密文来源不同时返回 `LW_PLATFORM_CONFIGURATION_OBSERVATION_MISMATCH`；复制请求中的摘要不能通过。

### Control 启动加载

Bundle `exec` 对同一次 age 输入制作有上限的私有快照，计算实际密文摘要，再认证解密和验证目录。对于 `managed_version`，它生成非秘密的 `platform-configuration-source.json`，记录原 revision、设置摘要、实际密文摘要及原 Bundle 的 namespace/releaseName。仅解析或渲染目录不会生成此来源文件。未保存的 `initial_settings` 也不会产生已加载版本事实。

`exec` 为消费者设置 `LABWEAVER_PLATFORM_CONFIGURATION_VERSION_FILE` 与 `LABWEAVER_PLATFORM_CONFIGURATION_SOURCE_FILE`，并清除同名环境覆盖。Control 启动入口在连接外部依赖前读取这两个普通 `0600` 文件，拒绝符号链接、未知或重复字段、超限和不匹配绑定。它复用原 Rust 类型校验，并从实际读取的设置重算 canonical 摘要。两个变量必须同时存在；均不存在时，服务仍可读取、编辑和报告失败，但不能接受新的成功报告。

Control 将读取结果固定在同一服务实例中。修改投影文件或保存新的 desired 不会替换运行进程的快照。加载新版本需通过获准路径启动替换实例；读取失败不会修改旧实例或数据库中的最后验证版本。原内部 report 从该实例读取快照，HTTP body 不能设置它。成功报告丢失响应时，相同幂等请求仍返回原结果；旧结果回放不触发再次应用。

来源文件是获准部署路径交付的非秘密输入，不是签名证书或实际节点/namespace 的运行观察。该检查只闭合 Control 的加载与数据库发布边界。#199 的运行准入消费者继续从同一已验证 applied 记录读取策略；其授权、成员和 ResourceApproval 规则不变。其他服务、实际实例和安装位置仍需执行者独立验证。来源文件或命令退出成功本身不足以提交完整部署成功报告。

现有 Helm 部署尚未挂载这两个新输入。不要仅设置环境变量并指向缺失路径；后续安装接线必须从已验证 Bundle 交付同一版本和来源文件，再按实际部署范围验收。此增量不新增公共 DTO、服务、审批状态或配置仓库。

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

`control/0015_platform_configuration.sql` 创建管理配置表，其文件与摘要保持原样。`0018_platform_configuration_bundles.sql` 追加不可变登记表和版本查询索引。0014属于#197，0016和已发表的0017属于#199。部署前由父协调组合 catalog，再按统一递增顺序执行。不要在仍需补齐旧编号的环境中先执行后续迁移。

本 PR 没有在远端执行迁移。故障时先查看实际 migration ledger 和目录，不在服务启动时修补未知 schema，也不删除配置历史。

## Grok 接入

Grok 使用上述生成 SDK 和真实服务端事实。界面显示当前 desired、最后验证 applied、appliedState、失败类别、下一步和设置影响。表单与 YAML 共用同一 revision，不手改生成文件。

界面先说明中文原因和恢复动作，再允许复制 code/requestId。失败后保留草稿，冲突后让管理员比较新旧修改。应用状态显示为真实阶段，未知进度不显示百分比。只在破坏性重启或资源影响明确后安排确认，不把保存草稿当部署成功。

Grok 可将版本 API 的完整 JSON 交给获准导出操作。归档选择使用实际 `.age` 文件，登记 query 从当前 desired/aggregate 事实取得，替换原手填 digest 控件。界面从 `registeredBundle` 或版本登记 GET 读取实际密文摘要，显示登记者、时间和设置绑定；没有登记时不允许应用。登记成功只表示可查询文件事实，不显示为 applied 或另造审批状态。

新 desired 会清除登记指针，界面提示重新导出和登记。409/412 后保留所选文件及草稿，刷新并比较当前状态；相同请求重试保留原幂等键和文件，不重新seal制造不同密文。对丢失响应的重试结果，仍遵守现有较新revision不得被旧响应覆盖的规则。

导入文件使用现有更新 SDK，先核对目标安装当前 revision，冲突时保留草稿。部署服务身份不交给浏览器。age 私钥由操作员独立持有，不进入配置 API。生成 SDK 位于 `web/src/generated/contracts`，继续从真源产生。

首装可用 Bundle 的 `settings-init` 准备未保存配置。它复用本接口的初始模板与完整请求，expectedRevision 为0，不制造作者或配置历史。真实管理员首次提交后，Control 才创建 desired 版本。已有配置时必须比较当前值，不能把首装流程用于覆盖或重置。

## 本地浏览器集成验证

仓库提供生成 SDK 到真实 Control HTTP 路由和临时 PostgreSQL 的目标测试。它使用现有签名服务鉴权 fixture，仅在外部 issuer 和 Access 授权决策边界使用合成依赖。浏览器上传实际 age 密文 `File`，检查摘要、版本冲突、幂等回放、权限拒绝、应用请求及失败历史。测试不执行部署，不覆盖 Access 会话/CSRF 或管理员页面。

1. 安装 Rust、Node 22、Web 的锁定依赖、age 1.3.2 和可用的 Docker。
2. 在 `web` 目录运行 `pnpm exec playwright install chromium`。已有 Chromium 时，可通过 `LABWEAVER_TEST_CHROMIUM` 指定可执行文件。
3. 在仓库根目录运行 `cargo test -p control-service --test platform_configuration browser_sdk_registers_real_ciphertext_and_reads_durable_failure --locked -- --ignored --exact --test-threads=1`。

测试在 loopback 随机端口启动 Control 与 Vite，不连接共享集群。临时目录保存合成身份、密文和浏览器输入，并在测试结束时清理。测试只回报有界诊断，不输出 Token、私钥或请求正文。Playwright CI 单独运行此目标；普通 Rust 测试将其标为 ignored，不能将 ignored 计作通过。

### 原生登录和 CSRF

另一个目标从真实 Keycloak 表单进入 Access HTTPS 门户。它使用实际 BFF 会话、CSRF、流式转发、Control API 和 PostgreSQL。Access 与 Control 的服务 JWT 也由临时 Keycloak 服务账户签发，服务客户端校验临时 CA。测试账户和密钥均为合成输入；测试结束后回收容器和临时目录。

1. 准备上述依赖。
2. 运行 `cargo test -p access-service --bin access-service browser_proxy_tests::platform_configuration_browser_tests::chromium_login_and_csrf_reach_real_platform_configuration --locked -- --ignored --exact --test-threads=1`。
3. 核对登录、缺失或错误 CSRF、错误 Origin、非管理员、实际文件登记、刷新与退出结果。测试使用明确标记的 SDK 页面，不将测试页面称作产品控制台。

管理员 App 另有独立目标。在包含管理员配置页面的 Web 工作树上，将 `LABWEAVER_TEST_WEB_ROOT` 设为该工作树的 `web` 绝对路径，然后运行 `cargo test -p access-service --bin access-service browser_proxy_tests::platform_configuration_browser_tests::chromium_login_and_csrf_reach_platform_configuration_page --locked -- --ignored --exact --test-threads=1`。它从受保护路由开始，经过原生登录后操作实际页面；不注入服务 Token、手造会话 Cookie 或替换配置 API。

`LABWEAVER_TEST_CHROMIUM` 可指定已有 Chromium。默认截图保存在测试临时目录中；需要保留时，将 `LABWEAVER_TEST_BROWSER_EVIDENCE_DIR` 设为测试专用输出目录。截图不包含密码、Token 或解密身份。CI 显式运行原生 SDK/BFF 目标；App 目标需要已组合的 Web 源码，普通 Rust 的 ignored 不表示通过。

测试 Keycloak 使用固定发行版和独立 CA，JVM heap 为128–512MiB。该限制只用于临时测试实例，配置方式见 [Keycloak 容器说明](https://www.keycloak.org/server/containers#_specifying_different_memory_settings)。此目标不连接共享集群，不执行部署或产生 applied 成功事实。

### 实际 Bundle 加载验证

准备 age、Helm 3.21.3、Python/PyYAML 和本仓库的 `cargo build -p xtask --locked`。然后运行：

```sh
cargo test -p control-service --test platform_configuration_loaded \
  actual_bundle_load_is_required_for_success_and_failure_keeps_effective \
  --locked -- --ignored --exact --test-threads=1
```

该目标使用新生成的合成 age 身份与完整 Bundle，执行原解密和派生入口，再由实际 Control API 和 PostgreSQL 保存结果。它验证未加载、另一份密文、旧进程、文件变化、失败保留最后验证版本与重试。外部 JWT issuer/Access 决策为测试边界；没有真实集群或其他服务的部署观察。CI 显式执行此目标，普通 Rust 的 ignored 不能计作通过。

尚未完成的范围：旧 PKI/NATS 签发 workspace、其他服务的实际部署观察、默认值消费者、Helm 加载输入接线，以及 #207 运行授权与执行边界。
