# 对象动作与成员授权

Control 保存课程成员和项目成员。Access 保存身份、BFF 会话、AccessGrant 和连接状态。Access 直接读取 Control 的当前成员事实，不保存成员副本。

`Teacher`、`Student` 和 `PlatformAdmin` 表示已验证的平台身份。项目成员的 `objectRole` 表示该项目中的职责。教师身份不能提高只读成员的项目权限。

| objectRole | 项目动作 |
| --- | --- |
| viewer | browse |
| operator | browse、start、stop |
| editor | browse、edit、start、stop、configure_agent、grant_access、view_costs、request_runtime_permissions |
| manager | editor 的动作，以及 manage_members |
| owner | manager 的动作。创建项目时由 Control 保存 |

平台管理员可以执行全局管理动作。服务端仍检查身份期限，并记录操作者、操作和诊断。普通项目成员不能通过 `objectRole` 获取 `manage_runtime_policy`。成员 API 不能写入 `owner`，也不能覆盖或移除项目所有者的成员记录。

普通账号的课程范围仍要求当前课程成员。契约明确允许管理员的课程操作使用全局管理员权限。教学内容接口保留其既有角色约束。项目职责不会授予其他课程、其他项目或他人的个人实验内容。

## 项目 API

1. 使用当前 BFF 会话访问 `GET /api/v1/projects/{projectId}/available-actions`。
2. 从 `actions` 读取当前成员允许的动作。
3. 使用同一项目的实际 API 执行动作。服务端在每次请求中重新授权。

响应包含 `projectId`、`actorId`、`membershipRevision`、`actions` 和 `validUntil`。这些字段用于界面展示，不能作为写请求的授权证据。Project 视图、元数据编辑、成员管理、材料操作、Agent 配置和费用读取使用契约中对应的对象动作。现有实例请求先读取 Environment 的实际归属，再检查该项目的当前成员动作。浏览器参数不能替换对象所属项目。实例所有者在成员移除后也不能继续调用。OpenAPI 的 `x-labweaver-object-action` 记录这些映射。

添加成员沿用 `POST /api/v1/projects/{projectId}/members`、原幂等键和精确用户名解析。新增 `objectRole`。未提供该字段时保存 `viewer`。`role` 保留教学职责的既有含义。移除成员沿用精确 `expectedRevision` 和原因。Control 在同一事务保存成员变更和 `member_authorization.changed.v1` 项目事件。事件包含目标成员、实际操作者、revision、状态和项目职责。

未获项目范围授权的账号访问项目或 `available-actions` 时返回 403，不泄漏该项目是否存在。平台管理员访问不存在的项目时返回 404。资源读取还须通过其领域的归属检查。403 不能改成空列表或成功响应。

## 撤权期限

成员移除后，新请求读取已提交的 Control 成员状态。已打开连接的撤权上限沿用 Access 原有的 60 秒期限。

- 课程和项目 SSE 绑定原 BFF 会话，每 1 秒检查当前会话和成员。每次发送事件前也检查。成员失效、revision 改变、会话注销、账号停用、身份到期或查询失败结束连接。每次成员检查和事件读取最多等待 5 秒。
- 控制台每 1 秒检查 Control 成员、当前 AccessGrant、BFF 会话和账号状态。检查最多等待 5 秒。转发写入也有 5 秒上限。授权结束时关闭浏览器连接，诊断为 `LW_CONSOLE_AUTHORIZATION_ENDED`。
- SSH Gateway 沿用每 15 秒心跳和 5 秒 HTTP 超时。Access 在心跳中检查当前成员和 AccessGrant。无权会话进入 `terminating`，Gateway 停止目标进程。

事件用于通知观察者。周期检查保证丢失事件时也能收敛。重新加入项目产生新 revision，不能恢复旧 SSE 授权。

撤销访问不表示环境停止，也不表示资源或费用已经释放。本接口不删除 Work 或工作文件。用户需要按已有生命周期入口确认停止和释放结果。

## 数据库切换

Control 的 `0016_object_authorization.sql` 建立唯一成员表。Control `0014` 属于材料上传，`0015` 属于平台配置。组合 catalog 时保留三个有序条目。

本次使用 Sprint5 已确认的无旧接口兼容规则。Access 旧成员表的创建和写入路径已移除。Access `0006` 只提供当前会话 hash、actor 和期限。Control 无法通过该视图获取原始会话 Cookie、加密 CSRF、Token、sid 或 issuer subject。非空旧数据库的 catalog 身份不匹配时必须停止。普通应用启动不能清空、复制或静默修复旧成员数据。

当前验证使用新建的合成 PostgreSQL。生产切换需要维护者批准的操作窗口。本代码变更没有执行生产迁移、数据清空或真实账号授权。

## 交付边界

这份契约定义项目动作和当前成员撤权。运行策略的唯一类型由 #203 的 `platform_configuration.rs` 提供。#207 负责将获准配置落实到创建、启动、重启和实例替换。成员动作不能代替平台允许清单、配置版本或运行批准。

共享 Work 的领域所有者检查、运行批准消费者、课程助教和邀请管理，以及 Grok 界面仍须完成各自接线和真实流程验证。项目 `available-actions` 不表示这些消费者已交付。只有接入相同授权判断的对象接口才可据此开放操作。
