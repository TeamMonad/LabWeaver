# 当前用户通知

Rust 真源为 `contracts::notifications` 与 `http::OPERATIONS`。Control 保存通知和已读事实，不新增服务，也不拥有资源、费用或账号处置状态。

## 公共接口

| HTTP | SDK operation | 返回 |
| --- | --- | --- |
| `GET /api/v1/notifications?cursor=…&limit=50` | `listNotifications` | `NotificationPage`，复用 `CursorPage<Notification>` |
| `GET /api/v1/notifications/unread-count` | `getNotificationUnreadCount` | `NotificationUnreadCount` |
| `PUT /api/v1/notifications/{notificationId}/read` | `markNotificationRead` | `Notification` |

Access 使用真实浏览器会话鉴别调用者，写请求沿用 Origin/CSRF 检查，Control 再向 Access 验证会话。客户端不能提供 recipient。所有平台角色均可读取自己的通知；同项目的其他成员、平台管理员都不能借这三个接口读取别人的通知。

分页按数据库单调 ordinal 倒序，游标为上页最后一条的 ordinal。`limit` 默认 50、范围 1–100；只在确有后页时返回 `nextCursor`。游标不携带授权，改写游标也不会扩大 recipient 或当前成员范围。新通知不挤入已经翻过的区间。空列表与未读数零均为正常结果。

每次列表、计数和已读写入都检查当前 active 项目成员与到期时间。撤权后列表不返回这些通知，计数不含它们，已读请求返回 404。标记已读是集合赋值幂等操作，不要求新幂等键，保留首次 `readAt`。通知没有撤权能力，也不构成对象授权；`objectUrl` 仅为门户路径与对象 ID，打开后仍经现有对象授权。

申请结果和申请时短租期提示指向 `/researcher/resources?projectId=…&requestId=…`，不会把被拒绝的申请错误地指向尚不存在的环境。Environment 租约通知指向现有 `/researcher/environments?projectId=…&environmentId=…`；Task 租约仍指向原资源申请。前端通知入口应保留这些 query 参数，并在资源页定位指定 `requestId`，不把通知当作授权凭据。

`Notification.kind` 为 `resource_decision`、`lease_changed`、`lease_expiry_reminder` 或 `short_lease`。`observedAt` 和 `deadline` 是对应 Owner 事件的观察时间及租约期限，不是查询时的新授权或计费结果。摘要使用固定非受限文案，不复制申请原因、Owner diagnostic、私人内容、金额或评测数据。当前费用、资源状态和最新期限应查询对象自己的 API。停止/撤销租约始终不表述为资源已释放。

## 领域事件、幂等与提醒

部署拥有 `LABWEAVER_RESOURCE_EVENTS` stream 和 `control-notifications-v1` durable consumer，filter 为 `labweaver.resource.>`，显式 ACK，最多 10 次交付。已有 `resource.request.*`、`resource.lease.*` Outbox 事件包含真实 `eventId`、project/course、requester、request/lease ID、revision、sequence、观察时间及租约起止时间；不需要第二次查询拼接非原子状态。申请提交事件仅产生短租期提示；批准/拒绝产生决策通知；租约事件产生状态变更通知。

Control 先校验 CloudEvent 的 owner/source/type/schema/项目绑定及领域契约，再以 eventId 保存 inbox。相同事件重放不新增通知；同一 eventId 的不同字节报 `LW_NOTIFICATION_EVENT_CONFLICT`。聚合 sequence 水位防止乱序事件覆盖较新事实。数据库提交后才 ACK；暂时数据库失败 NAK，重试有界；非法事件或交付耗尽 TERM 并记录不含 payload 的诊断。消费者不发布资源操作、不修改账务。

提醒读取实际进程加载的 `lifecycle.expiryReminderSeconds`，默认 `[86400,3600]`。仅适用于已有 Owner 起止时间的 active/expiring 租约。按 UTC 绝对时间 `expiresAt - window` 计算，不使用服务器本地时区或夏令时墙钟。公共时间戳仍只接受契约规定的 UTC 毫秒格式；前端可将其转换为用户时区显示。

- 租期恰好等于窗口：提醒在开始时间到期。
- 租期短于任一窗口：仅合并一个立即短租期提示；仍保留较小窗口能覆盖的提醒。
- 申请刚提交且请求租期短于窗口：立即记录申请时长提示，不伪造批准期限。
- 同一 lease/deadline/window 只提醒一次，即使 expiring 等后续事件再次携带同一时间窗口。
- 续期改变期限时取消旧期限提醒；撤销/到期取消剩余提醒；迟到旧事件不恢复它们。
- 查询或发邮件时再次比较当前数据库时间；已经过期的普通到期提醒不再作为“即将到期”暴露或发送，历史租约变更和短租期提示仍可查询。
- 新 desired 配置不改变正在运行进程或既有通知。更换认证 Bundle 后的新实例对新事件使用实际加载值。

## 可选邮件

唯一配置源为加密 Bundle `notifications.smtp.*`，见[部署说明](../deployment/encrypted-bundle.md)。不提供该段时邮件状态 `disabled`，站内通知、未读与已读完整工作。配置错误在 Bundle 校验阶段拒绝并报告具体字段；运行时仍校验 Secret 文件和 TLS 配置。

收件人来自真实 OIDC callback 的 `email`，且 `email_verified` 必须是布尔 `true`。每次登录覆盖；缺失、未验证或被 IdP 撤回时清空旧值。不接受客户端地址、username 推导、Bundle 地址映射。PII 单独存于 Access；Control 仅可 SELECT `access.notification_recipients` 视图，不能读取底表或写地址；普通 actor/session API 不增加邮箱字段。

`emailState`：`disabled`、`pending`、`sent`、`failed`、`exhausted`、`no_verified_email`、`delivery_unknown`。没有已验证邮箱是终态 `no_verified_email`，无诊断、不重试、不告警。明确 SMTP 4xx 临时拒绝才重试，间隔 30 秒、最多 5 次，耗尽为 `exhausted`；5xx 永久拒绝立即记终态 `failed`，不再发送。单次 SMTP 总超时 10 秒；超时、连接断开或进程崩溃留下超过 60 秒的 sending claim 均记终态 `delivery_unknown`，诊断 `LW_NOTIFICATION_SMTP_DELIVERY_UNKNOWN`，不自动接管重发。`emailDetail` 显示“邮件可能已送达，系统无法确认；为避免重复发送，不会自动重发。”多副本用 `SKIP LOCKED`、claim ID 与回写 fence，网络调用不持数据库锁。

邮件采用 at-most-once 而不是 exactly-once：结果未知时保留站内通知和可见未知状态，不重新发送，也不谎称已送达。邮件仅含非受限摘要和 HTTPS 对象链接；SMTP 强制 TLS/STARTTLS。运维从通知 API 检查 emailState/emailDiagnostic/emailDetail，遇到未知状态核对 relay 日志；不要重置数据库队列或重复执行资源、账务操作来重试邮件。Control 收到关停信号后停止领取并最多等待 20 秒完成已领取通知/邮件；强制终止后的事件靠未 ACK 的 JetStream 重放恢复，邮件不明结果仍保留未知终态。

## 错误

- 400 `LW_NOTIFICATION_INVALID`：游标、limit 或输入格式非法。
- 404 `LW_NOTIFICATION_NOT_FOUND`：通知不存在、不是当前 recipient 或已撤权。
- 503 `LW_NOTIFICATION_PERSISTENCE_FAILED`：通知存储暂不可用，可重试。
- `LW_NOTIFICATION_EVENT_CONFLICT`：内部事件 ID 冲突，终止投递，不向普通用户泄露 payload。

抢占、欠费 7 天保留和账号停用的 Owner 事件分别由 #225、#209/#228 补齐后接入本模块的同一投影入口。当前不预建不存在的 subject、payload 或消费者分支。事件必须含稳定 eventId、真实 recipient、对象/项目标识、Owner sequence/revision、观察时间、真实期限/处置阶段和下一步，不能将停止请求当作已释放事实。
