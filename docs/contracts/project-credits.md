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

两个列表使用 `page` 与 `pageSize`。默认 25，最大 100。响应含 `hasMore`。账目按创建时间和 ID 倒序排列。预占含真实业务目标、付款项目、业务键和调用服务身份。

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

## 计算与存储估价

`estimateProjectResourceCost` 使用 `POST /api/v1/projects/{projectId}/resource-cost-estimate`。权限与 Credits 读取相同。请求提供资源规格及独立的 `computeFrom/computeUntil`、`storageFrom/storageUntil` 半开区间。停止计算后保留的存储可使用更长区间。该读取没有资金或容量副作用。

响应 `ResourceCostEstimate` 保存输入、每条费率 ID、revision、价格、quantity、quantityDivisor 及 CREDITS 总额。价格由 Resource 当前费率取出；GPU 模式由真实目录解析。缺费率、覆盖缺口、混用币种、数值溢出或未知 GPU 类别明确失败，不显示为免费。

内部 `createEstimatedCreditHold` 使用 `POST /internal/v1/credits/estimated-holds`，要求 `resource.credits.hold` 与已配置的工作负载 owner。请求仅提供 requestId 和 expectedEstimate。Resource 从已保存的申请或批准读取资源规格与时限，在同一事务验证范围、重新计算价格、预占账户并保存不可变快照。Access 不能调用此入口。批准后改变规格或费率变化导致报价不一致时，返回 `409/LW_RESOURCE_ESTIMATE_CHANGED`；调用方重新显示真实估价并取得原流程要求的授权。

当前绑定 `businessKey=metered-usage` 的上限覆盖申请的完整计算和存储时限。两段开始时间必须尚未到达。相同输入和 Idempotency-Key 丢响应重放返回原结果；换重试键也只取得同一申请的原快照。再次读取当前预占状态仍使用 holds 查询，创建响应表示当时的接受结果。

共享 Task 的内部 ack 已强制按批准的 CPU、内存、存储和 GPU 与最大时限预占。预占、报价快照和执行授权在同一事务提交。缺费率或欠额时保持尚未启动，不能取得执行授权。Agent 沙箱和 Evaluation 沿其既有 ack 入口消费同一规则。明确配置的零价仍写零额预占与报价，不以缺配置推断免费。

独立 LLM token、BuildKit 构建、环境新建与续期的全部消费入口，以及预留产品的期间预付仍由后续 #210/#225 接线完成。这个切片不能代替这些入口的验收。

## 毫秒数量

每条已知用量和费用明细携带 `quantityDivisor`。基础单位仍是 millicore-second、byte-second 或 GPU-unit-second；数量除以该字段后才是基础单位值。真实 Environment 与共享 Task 计量使用1000。例如400毫秒的1单位 GPU记录 quantity=400、quantityDivisor=1000，对应0.4 GPU-unit-second。

历史整秒数量使用除数1。金额不使用浮点数；估价、结算及费用契约校验共用精确整数公式，六位小数的中点舍入取偶数。按费率边界分段后记录每段数量和费率。不得先把毫秒截断成整秒。运营方的每秒价格不随此修正换算或自动改变。

运行时间或 token 未知仍保留 unknown。用户已确认核实期限为首次未知事实之后1小时。模型token/cache价格与最高允许预占仍未指定具体值，不能用外币预算推定Credits价格。


## 未知用量的一小时期限

Resource使用数据库UTC毫秒时间保存首次未知事实的 `unknownSince`。`reconciliationDueAt` 固定为该时刻加1小时；同键或换键重试保留原起点。管理员无需再选择1h/24h。Resource0018从旧不可变unknown账目恢复最早时刻，不采用最后重试或预占创建时间。

到期处理释放剩余预占，保存 `expiredUnknownAt`，保留measurementUnknown和历史账目。账户可用余额恢复，但未知用量仍未核实；unknownCount仍可大于零。前端应显示“待核实，预占已释放”，不能显示“免费”或把实际费用填零。该释放只处理资金，不宣称运行对象、持久盘或外部调用已经结束。

迟到的已确认费用全部由平台承担，不重新冻结或扣收项目余额。已知结算入口本身检查期限，并在同一账务事务先执行到期释放；它不能依赖后台循环恰好先处理。原eventId换重试键也只入账一次。在期限前已核实并结算的费用保持正常规则，不因后来的时钟边界重新退款或收费。

Resource现有后台循环每次最多处理100个候选，重启或重复处理不重复释放。释放、已知核实和账目遵守账户先于预占的锁顺序。报价和完整价格配置均不因本期限修改。模型价格、全部模型/BuildKit/环境消费入口和预留产品仍按剩余验收推进。

## BuildKit 构建费用

Dockerfile 构建使用 `build_second`，费率还必须指定真实 `builderBinding`。`unitQuantity` 表示价格对应的构建秒数，费用单位为 `CREDITS`。现有 BuildKit 没有执行请求中的 CPU/内存上限，因此这些声明不能换算为已使用的 CPU/内存时。没有匹配费率时拒绝新构建；本功能不写入默认费率或初始余额。

| 操作 | 接口 | 调用范围 |
| --- | --- | --- |
| estimateProjectBuildCost | POST /api/v1/projects/{projectId}/build-cost-estimate | 当前项目成员或平台管理员 |
| getProjectBuildCost | GET /api/v1/projects/{projectId}/build-costs/{buildRequestId}/attempts/{attempt} | 同上 |
| admitBuildCost | POST /internal/v1/build-cost-admissions | 已配置 Agent，resource.credits.hold |
| getBuildCost | GET /internal/v1/build-costs/{buildRequestId}/attempts/{attempt}?leaseToken={leaseToken} | 同上，原 nonce |
| observeBuildCost | POST /internal/v1/build-cost-observations | 已配置 Agent，resource.usage.record |
| claimBuildCostReconciliations | GET /internal/v1/build-cost-reconciliations | 已配置 Agent，resource.credits.hold |

估价输入是 `builderBinding`、`maxDurationMilliseconds` 和 `from`。执行预占从 Agent 已保存的真实 BuildRequest 取得项目、builder 和最大时长，拒绝旧 attempt、不同 nonce、失效租约或已取消任务。Resource 在自己的 SQLx 事务内提交估价快照、真实 BuildRequest/attempt 预占和执行授权；Agent 随后在自己的本地事务再次检查取消与执行 fence，事务外才调用 BuildKit。这是跨进程准入协议，两端不是一个远端原子事务。

每个 hold 具有一个真实目标：`requestId` 或 `buildRequestId` + `buildAttempt`。构建 hold 的 `requestId` 为 null，不伪造 ResourceRequest、Task 或 Environment。构建记录保留执行截止时间、原 stage 身份、估价、用量与费用明细。预占以 build/attempt 去重，并校验不可变命令、截止时间和 nonce；观察按业务身份去重，允许 Unknown 后报告 Known，不能更改已结算事实。

实际费用取唯一且身份匹配的 BuildKit history 的起止时间，以毫秒分子、`quantityDivisor=1000` 计算，跨版本按边界分段，和估价共用精确计价函数。缺失边界或低于当前毫秒精度的区间保留待核实状态和观察到的原时间，不补零。已知时间遇到缺失历史费率时保留时间事实，费用继续待核实；管理员补齐覆盖后按原事实重试。

丢失执行响应后只查询原 stage，不能重放 Solve。未知事实首次出现后一小时释放剩余预占，重试不延长；迟到的已知费用和超过预占的尾部由平台承担。原构建费用可在清理或新 attempt 后查询和结算，查询不能恢复工作流或发布镜像。Resource 到期维护不依赖 NATS Outbox；Agent 每轮最多读取 25 条到期原 grant，服务端为同条查询保留一分钟间隔。未知记录和费用待核实状态在资金释放后仍保留。
