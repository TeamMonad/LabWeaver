# 项目 Credits

Resource 保存项目账户、预占和不可变账目。金额使用现有 FixedDecimal 六位字符串，单位固定为 `CREDITS`。不使用 JavaScript 浮点数计算金额。

## 账户与费用

| 字段 | 含义 |
| --- | --- |
| balance | 已入账余额，包含尚未结算的预占 |
| reserved | 已预占但尚未扣收的金额 |
| available | balance 减 reserved，只有此金额可供新预占 |
| spent | 已扣收金额，减去已冲正的扣收 |
| pending | 已预占且没有未知标记的金额 |
| unknown | 有未知用量标记的剩余预占上限，不是确认的实际费用 |
| unknownCount | 尚未核实的预占记录数，释放余额后仍可大于零 |
| platformAbsorbed | 超出剩余预占的已知费用，由平台承担 |

未创建的账户读取为零。管理员首次发放或预占事务创建账户，不从历史费用推导初始余额。预算提醒与账户分别保存。预算不能替代账户准入。

项目为其 Work 和课程实验付款。小组必须指定一个成员项目。此接口不会自动分账。小组成员资格与付款项目选择由 #199/#219 的权威接口检查。

## 用户与管理员 API

用户通过现有 Access BFF 会话调用。Access 查询当前项目成员事实，Resource 校验 Access 服务 JWT 和签名用户委托。非成员不能读其他项目账目。平台管理员可以读全部项目。

| 操作 | 接口 | 权限 |
| --- | --- | --- |
| getProjectCredits | GET /api/v1/projects/{projectId}/credits | 项目成员或平台管理员 |
| listProjectCreditLedger | GET /api/v1/projects/{projectId}/credits/ledger | 同上 |
| listProjectCreditHolds | GET /api/v1/projects/{projectId}/credits/holds | 同上 |
| createCreditAdminOperation | POST /api/v1/resource/credits/operations | 平台管理员 |

两个列表使用 `page` 与 `pageSize`。默认 25，最大 100。响应含 `hasMore`。账目按创建时间和 ID 倒序排列。预占含 requestId、付款项目、业务键和调用服务身份。

管理员写操作必须提供 `Idempotency-Key` 和非空 reason。角色由签名身份提供，不从请求体读取。

```json
{"action":"grant","projectId":"01900000-0000-7000-8000-000000000001","amount":"20.000000","reason":"课程合成额度"}
```

- grant 只接受正金额，项目增加余额，平台对手账目减少同额。
- transfer 接受正金额和 destinationProjectId，同一事务写出款和入款。出款不能使用已预占资金。
- adjust 接受非零正数或负数。负数不能使余额低于预占。
- reverse 接受原 entryId 和 reason，追加原操作全部双方的反向账目。原记录保留。重复冲正拒绝。

预占与释放记录不能冲正。扣收冲正只退款，不重新打开预占，也不删除平台尾部成本记录。资金已被消费或预占时，发放或转移冲正可能被拒绝。失败不留下单边账目。

## 内部预占与结算

| 操作 | 接口 | 服务权限 |
| --- | --- | --- |
| createCreditHold | POST /internal/v1/credits/holds | resource.credits.hold |
| resolveCreditHold | POST /internal/v1/credits/holds/{holdId}/resolve | resource.credits.resolve |

接口要求现有服务 JWT。调用者还必须属于已配置的 Environment 或 Task owner 客户端。Access 网关不能调用。Environment 只预占环境申请，Task owner 只预占 Task 申请。付款项目必须匹配 Resource 保存的 requestId。结算和释放必须匹配预占的 ownerClientId。

createCreditHold 接受 projectId、requestId、businessKey 与正 amount。调用方使用正式估价的上限，不接受浏览器报价。账户行锁把余额检查、预占、账目与幂等结果放在同一事务。相同申请与业务键只能创建一个预占。终态申请不能新建预占。

resolveCreditHold 使用稳定幂等键。settle 还要求稳定 eventId，同一源事件换 HTTP 重试键也不能重复扣收。

```json
{"action":"settle","eventId":"01900000-0000-7000-8000-000000000002","amount":"3.000000","finalize":true}
```

settle 最多扣收剩余预占。超出的金额写 platformAbsorbed。finalize 为 true 时，释放未用预占。释放后的迟到已知费用全部由平台承担。释放和结算的乱序不能产生负余额。

release 必须提供原因，释放剩余预占。unknown 必须提供原因，不扣费、不把用量视为零。measurementUnknown 独立于预占终态。释放未知预占后，unknownCount 仍显示待核实记录。最终已知结算清除此标记。

现有费率结算只对显式绑定 `businessKey=metered-usage` 的申请扣收 Credits。费用计算、原费用记录、预占扣收和预算更新共用 SQLx 事务。非 CREDITS 费率不能扣收此预占。无预占的历史路径保持费用计算语义，不隐式扣钱包。

Resource 在 Task owner 确认释放、Environment 实际容量回收及未交接容量回收后，释放申请的剩余预占。发起停止或进入 expiring 不代表回收完成。未知标记保留，迟到计量仍可对账。

## 诊断与重试

| 状态 | 诊断 | 处理 |
| --- | --- | --- |
| 400 | LW_CREDITS_INVALID / LW_CREDITS_PRECISION | 修正金额、原因、业务键或幂等键 |
| 403 | LW_CREDITS_SCOPE_DENIED | 检查项目、角色或预占 owner |
| 404 | LW_CREDITS_NOT_FOUND | 查询原申请或预占，不新建替代账单 |
| 409 | LW_CREDITS_INSUFFICIENT | 管理员发放额度，或先完成实际资源回收 |
| 409 | LW_CREDITS_CONFLICT | 查询原操作，不换键重复扣收 |
| 503 | LW_CREDITS_STORAGE / LW_CREDITS_PERSISTENCE | 恢复数据库后重试相同输入与键 |

HTTP 错误使用 RFC 9457，并含 requestId。服务 JWT 拒绝沿用现有鉴权诊断。

## 集成边界

#209 提供真实账户、预占与账务 API。#210 提供各类费率、可信上限估价、续期预占、未知用量核实期限和模型限额。#200 的自动准入需要 #209/#210 接口组合，不以预算提醒授权消费。本增量不声称所有资源创建都已强制扣款。

#199 独立迁移 Control/Access 成员真源。Credits 沿现有授权接口，不写成员表。#203 独立管理配置 Bundle 与 platform_configuration 符号。Grok 消费真源生成的 SDK，不手改生成文件或增加模拟后端。

本地数据库验证只使用隔离合成账户。生产迁移、支付、账本调整、部署与发布需要各自授权。浏览器完整用户流程留给 Grok 联调与 Sprint 验收。

现有 Resource 后台循环还回收终态申请的遗漏预占。它只处理没有容量或容量已 released 的申请。#200 deadline worker 的取消记录可由此恢复。expiring 和未释放容量不会触发退款。重复恢复不新增扣收或再次退款。
