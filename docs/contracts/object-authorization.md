# 对象动作与成员授权

Control 保存课程成员和项目成员。Access 保存身份、BFF 会话、AccessGrant 和连接状态。Access 直接读取 Control 的当前成员事实，不保存成员副本。

`Teacher`、`Student` 和 `PlatformAdmin` 表示已验证的平台身份。项目成员的 `objectRole` 表示该项目中的职责。教师身份不能提高只读成员的项目权限。

| objectRole | 项目动作 |
| --- | --- |
| viewer | browse |
| operator | browse、start、stop |
| editor | browse、edit、start、stop、configure_agent、grant_access、view_costs、request_runtime_permissions |
| manager | editor 的动作，以及 manage_members |
| owner | manager 的动作，以及 archive |

平台管理员可以执行被操作契约明确允许的全局管理动作。项目 `manager` 可执行 `manage_members`，但不能管理运行策略或 archive；`owner` 负责项目根所有权，成员 API 不能写入、替换或移除 owner。服务端仍检查身份期限，并记录操作者、操作和诊断。普通项目成员不能通过 `objectRole` 获取未列出的动作。

普通账号的课程范围仍要求当前课程成员。契约明确允许管理员的课程操作使用全局管理员权限。教学内容接口保留其既有角色约束。项目职责不会授予其他课程、其他项目或他人的个人实验内容。

## 课程职责与邀请

Control 保存课程的 `objectRole`。`instructor` 可管理该课程的成员和邀请。`teaching_assistant` 可浏览该课程名单。`participant` 不能浏览名单。新职责不改变 OIDC 角色，也不扩大既有教学内容权限。完整实验批准还要求操作者具有已验证的 Teacher 身份及该课程当前有效的 instructor 成员记录。助教即使同时具有 Teacher、Student 或 PlatformAdmin 身份也不能替代该课程教师。无课程候选仍要求已验证的 Teacher 身份。

1. 用精确校园用户名创建项目或课程成员邀请。
2. 指定 `expiresAt`，明确接受截止时间。
3. 如需限制成员期限，指定独立的 `membershipExpiresAt`。
4. 目标账号从收到的邀请中确认职责。
5. 用原幂等键、`expectedRevision` 和对应 `If-Match` 接受邀请。

项目入口为 `/api/v1/projects/{projectId}/member-invitations`。课程入口为 `/api/v1/courses/{courseId}/member-invitations`。POST 创建邀请。GET 分页读取该范围的邀请。项目要求当前 `manage_members`。课程要求当前教师职责。全局管理员仍可管理。

`GET /api/v1/member-invitations` 只返回当前账号收到的邀请。`GET /api/v1/member-invitations/{invitationId}` 返回本人或当前管理者可读的记录。POST 到该路径下的 `/accept` 接受邀请。POST 到 `/revoke` 取消待处理邀请。取消请求包含原 revision 和原因。

Access 在目录解析前检查管理权限。Control 再检查目标本人、账号状态、截止时间、创建者成员 revision 和目标成员 revision。创建者撤权后再加入不会使旧邀请有效。目标成员已被其他操作修改时，旧邀请返回 409。项目邀请不能授予 owner 或替换项目所有者。课程 instructor 邀请还要求接受者具有已验证的 Teacher 身份。

接受结果、唯一成员事实、变更事件和幂等结果在同一事务提交。事件分别记录实际接受者与授权创建者。传输重放返回原接受记录，不恢复后来已撤销的成员权限。界面须读取当前动作，不把 accepted 当作永久授权。

已接受邀请通过成员入口撤权。项目沿用成员 DELETE。课程使用 `DELETE /api/v1/courses/{courseId}/members/{actorId}`。课程不允许移除最后一名有效教师。助教可访问 `GET /api/v1/courses/{courseId}/members`。普通课程成员不能通过深链接读取名单。

项目、项目成员、课程成员和三类邀请列表统一接受 `cursor` 和 `limit`，默认每页 50 条，最多 100 条。响应为 `{items,nextCursor}`；`nextCursor=null` 表示没有后续页。旧 `page`/`pageSize` 和裸数组响应已移除。游标为不透明、规范化的 base64url JSON，绑定端点、当前账号和项目/课程范围；不能跨范围、跨账号或混用列表游标。错误游标返回 400。每次续页重新检查当前权限；游标不授予权限，撤权后返回 403。

`GET /api/v1/projects` 按不可变 `project_id ASC` 排序，不再按 `updated_at` 排序；元数据更新不会让项目在翻页时移动。项目/课程成员按不可变 `actor_id ASC` 排序，邀请按不可变 `invitation_id DESC` 排序。范围及当前可见性谓词均在 SQL `LIMIT limit+1` 前生效；末页不签发游标。游标自含边界，不回查上页锚点，因此删除锚点不影响继续读取仍存活的后续行。复用现有项目、成员主键和邀请复合索引，无需新增迁移。

分页是当前事实的 keyset 读取，不是跨 HTTP 请求的数据库快照：新插入且排序在边界之后的行可出现在后续页；在边界之前的新行需刷新首屏。已有行不会因无关行增删、metadata/revision/state 更新而重复或跳过；删除的行不再返回。客户端用服务端 `nextCursor` 续页，不推导 offset，也不把已加载数据当作永久授权。

过期邀请按期限显示 expired，不依赖清理任务。无权读取返回 403。管理员读取不存在的邀请返回 404。状态或 revision 冲突返回 409。过期接受返回 410。请求与 `If-Match` 不一致返回 412。邀请接口使用 `Cache-Control: no-store`。

邀请不停止环境，也不释放资源或费用。课程创建、教学发布、评分及完整邀请界面仍由对应任务完成。本接口不建立第二套成员源。

## 项目 API

1. 使用当前 BFF 会话访问 `GET /api/v1/projects/{projectId}/available-actions`。
2. 从 `actions` 读取当前成员允许的动作。
3. 使用同一项目的实际 API 执行动作。服务端在每次请求中重新授权。

响应包含 `projectId`、`actorId`、`membershipRevision`、`actions` 和 `validUntil`。这些字段用于界面展示，不能作为写请求的授权证据。Project 视图、元数据编辑、成员管理、材料操作、Agent 配置和费用读取使用契约中对应的对象动作。现有实例请求先读取 Environment 的实际归属，再检查该项目的当前成员动作。浏览器参数不能替换对象所属项目。实例所有者在成员移除后也不能继续调用。OpenAPI 的 `x-labweaver-object-action` 记录这些映射。

添加成员沿用 `POST /api/v1/projects/{projectId}/members`、原幂等键和精确用户名解析。`objectRole` 为必填字段，必须明确指定项目职责；`role` 保留教学职责的既有含义。移除成员沿用精确 `expectedRevision` 和原因。Control 在同一事务保存成员变更和 `member_authorization.changed.v1` 项目事件。事件包含目标成员、实际操作者、revision、状态和项目职责。

未获项目范围授权的账号访问项目或 `available-actions` 时返回 403，不泄漏该项目是否存在。平台管理员访问不存在的项目时返回 404。资源读取还须通过其领域的归属检查。403 不能改成空列表或成功响应。

## 撤权期限

成员移除后，新请求读取已提交的 Control 成员状态。已打开连接的撤权上限沿用 Access 原有的 60 秒期限。

- 课程和项目 SSE 绑定原 BFF 会话，每 1 秒检查当前会话和成员。每次发送事件前也检查。成员失效、revision 改变、会话注销、账号停用、身份到期或查询失败结束连接。每次成员检查和事件读取最多等待 5 秒。
- 控制台每 1 秒检查 Control 成员、当前 AccessGrant、BFF 会话和账号状态。检查最多等待 5 秒。转发写入也有 5 秒上限。授权结束时关闭浏览器连接，诊断为 `LW_CONSOLE_AUTHORIZATION_ENDED`。
- SSH Gateway 沿用每 15 秒心跳和 5 秒 HTTP 超时。Access 在心跳中检查当前成员和 AccessGrant。无权会话进入 `terminating`，Gateway 停止目标进程。

事件用于通知观察者。周期检查保证丢失事件时也能收敛。重新加入项目产生新 revision，不能恢复旧 SSE 授权。

撤销访问不表示环境停止，也不表示资源或费用已经释放。本接口不删除 Work 或工作文件。用户需要按已有生命周期入口确认停止和释放结果。

## 数据库切换

Control 的 `0017_object_authorization.sql` 建立唯一成员表。`0018_membership_governance.sql` 保存课程职责和成员邀请。每个账号在同一课程只保存一条成员事实。旧重复角色记录会阻止迁移，不能静默丢弃。Control `0014` 属于材料上传，`0015` 属于平台配置，`0016` 属于平台配置 Bundle，`0019` 提供现有配置、发布和批准的只读投影。组合 catalog 时保留全部有序条目。

本次使用 Sprint5 已确认的无旧接口兼容规则。Access 旧成员表的创建和写入路径已移除。Access `0006` 只提供当前会话 hash、actor、期限和已验证的平台角色。Access `0007` 将 Control 的账号读取限定为 actor_id、username、display_name 和 disabled_at。Resource 仅获得账号状态列、当前会话安全投影及 Control 授权投影的读取权限。Control 和 Resource 无法通过这些读取获取原始会话 Cookie、加密 CSRF、Token、sid 或 issuer subject。非空旧数据库的 catalog 身份不匹配时必须停止。普通应用启动不能清空、复制或静默修复旧成员数据。

当前验证使用新建的合成 PostgreSQL。生产切换需要维护者批准的操作窗口。本代码变更没有执行生产迁移、数据清空或真实账号授权。

## 运行准入与批准绑定

平台运行策略使用 #203 的唯一配置类型。仅 `appliedState=verified` 且存在 applied 快照时允许新准入。保存 desired 草稿不改变有效策略。开始应用后状态变为 unverified，历史 applied 快照不能证明当前外部状态。创建、启动、重启、重置和重试入口在 Access 检查该状态。不确定应用时仍允许停止和恢复入口，不自动停止现有实例。

Control 在现有完整 `AuthoringApproval` 中保存实际课程教师及其成员 revision。批准同时绑定实验包、环境、评测候选的精确 revision 与镜像产物。发布继承这份批准，不另建发布批准。Resource 按真实 release ID 和版本读取 Control 保存的 `EnvironmentSpec`，重新计算规范化 SHA-256；请求方不能提交替代哈希。候选的 `policyRevision` 属于 LLM 策略，不能代替平台运行配置版本。

Resource 在现有 `ResourceApproval` 内保存 `runtimeAuthorization`。它绑定请求 ID、代次、付款项目、实际课程、目标 release、配置身份、运行权限及已验证 applied 的 revision 和哈希。审批事务在写入 claim、lease 和 Outbox 前重新检查。幂等摘要包含这份绑定。商业审批没有该字段时不授予运行权限。

兼容 Container Work 模板升级使用批准内的 `configuration.permissionProjectionSha256` 和
`templateFamilyId`：Control 是发布、撤回和模板族的事实来源，Resource 重新计算目标的
`EnvironmentSpec::permission_projection()`，要求资源、网络、安全、入口、provider 和端口全部相等，
并保持原 teacher_scope、permissions、平台策略 revision/SHA 与 request/claim/lease 身份。
release ID/version 和完整配置 SHA 保留为原批准审计身份，不再单独阻止同族兼容 Work 升级。
Experiment 与 VM 仍要求原精确 release；资源/权限变化不能借旧批准执行。

cutover 不改历史批准：缺少投影的旧批准读取为旧格式，但在任何新的授权检查中 fail-closed，
返回 `LW_RUNTIME_AUTHORIZATION_REAPPROVAL_REQUIRED` 和重新提交资源申请的中文原因。
不新增历史投影表或回填迁移；v1 可在另行授权的维护窗口清空重建，普通启动不能清空。


项目预授权只匹配相同项目。课程预授权只适用于该课程已获完整教师批准的 Experiment。付款项目仍检查请求者的当前项目动作，课程仍检查当前成员。无关课程、任务或他人的 Work 不能借用这份批准。平台允许清单始终是上限，教师批准不能扩大它。

`EnvironmentSpec.security` 分别声明 `runAsUser`、`privileged`、`capabilities`、`allowPrivilegeEscalation`、`readOnlyRootFilesystem` 和 `seccomp`。运行权限从本版本实际声明的 capabilities 与非特权模式穷尽投影，不能把平台允许清单或 scope 预授权的最大能力复制给实例；完整规格的 canonical 身份同时约束 UID、rootfs、提权和 seccomp。`runAsUser=null` 只消费同一 verified applied receipt 的 `defaultContainerUid`，不得拼接另一版本默认值。当前普通容器支持 root 和获准的有限 capabilities，但仍拒绝 privileged、unconfined seccomp 与更广内核权限；VM 不因此取得容器权限。声明增加、版本变化或平台换版都须重新经过原授权判定，不自动扩大旧批准。

普通切片的 `allowPrivilegeEscalation` 只接受 `false`/`null`，缺省和 null 均规范为 false；true 由唯一
Rust spec 语义校验拒绝，而不是由 Provider 降权执行。消费授权时，executor 从可信 spec/release、同版本
receipt 与部署配置重新生成完整可信计划。所有带对象 Apply 和计算动作的接收对象列表必须与可信计划
数量、顺序及内容相同，不接受携带计算对象的稀疏计划或有序子集；省略 NetworkPolicy、ResourceQuota、
LimitRange 等约束对象也会拒绝。接收列表使用同一 canonical 函数重算 SHA 后再比对可信 SHA，不能仅靠
自报摘要或 securityContext 相等取得执行权。完整 Pod/Deployment 安全面、Namespace PSA 和其他对象都
逐一核对；缺少、多出、重排或内容漂移均在 Kubernetes 副作用前拒绝。

`GET /api/v1/resource-requests/{requestId}/runtime-authorization` 通过现有 Access 服务鉴权及用户范围返回当前有效绑定，使用 `Cache-Control: no-store`。它重新读取当前请求、未过期的原 ResourceApproval、当前成员、Control 配置和已验证策略。请求换代、配置换版、发布撤回或范围失效会拒绝读取。已验证 applied 版本变更使旧绑定返回 `LW_RUNTIME_POLICY_CHANGED`，需要按新版本重新判定。desired 草稿不使旧绑定失效。响应不能替代 #207 在实际创建和实例换代时的检查。

未验证策略或授权输入不可用返回 503。无实际教师批准、成员范围不符或超出平台允许范围返回 403。旧绑定或配置不匹配返回 409。停止与恢复仍检查对象动作；恢复过程若创建新实例，#207 必须重新检查新准入。

Resource 提供 `current_environment_authorization(pool, request_id, expected_lease_id)`，供内部 Environment resolver 读取原批准的执行依据。Work 传入已有 lease ID；Experiment 可从原 request ID 查到该请求的唯一 lease。读取要求 request 和 lease 已 active、lease 与原批准均未过期或撤销，claim 属于该 request 且仍在有效分配阶段。它沿 lease→claim→claim.approval_id 读取精确原批准，不选择另一份“最新批准”。不会写入新批准或分配资源。

返回的 `CurrentEnvironmentAuthorization` 仅是本次已验证事实，包含原 `RuntimeAuthorizationBinding`、现有 `EnvironmentLeaseAuthorization`、approval ID/截止时间、provider binding 和同一已验证 `PlatformConfigurationVersion`。request/target、项目、课程、owner、release ID/version、容量和配置身份来自原事实。内部 resolver 必须逐项比对实际对象，使用原 target.environmentId，不能内部生成新 ID 后复用批准。执行截止时间同时受 lease.expiresAt 和 approvalValidUntil 限制。

内部 resolver 从该返回版本的 runtime_policy 读取默认 UID 和特权落点，不能随后读取另一版本来拼接授权。它由 #207 提供，并沿现有 Environment 服务认证边界调用；公开 GET 不替代服务身份。`LW_RUNTIME_LEASE_NOT_ACTIVE` 返回 409，表示租约尚未分配完成、已失效或原链不可用。此读取不把原容量批准升级为额外权限。

## 交付边界

这份契约定义项目动作和当前成员撤权。运行策略的唯一类型由 #203 的 `platform_configuration.rs` 提供。#207 负责将获准配置落实到创建、启动、重启和实例替换。成员动作不能代替平台允许清单、配置版本或运行批准。

共享 Work 的领域所有者检查、运行批准消费者、完整课程业务，以及 Grok 邀请界面仍须完成各自接线和真实流程验证。项目 `available-actions` 不表示这些消费者已交付。只有接入相同授权判断的对象接口才可据此开放操作。
