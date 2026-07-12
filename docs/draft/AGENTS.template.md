# 项目实施规则

本文是新项目的通用生产级 `AGENTS.md` 模板。复制到目标项目后，必须先完成“项目配置”一节中的占位项，再开始大规模实现。

本文件约束代理、自动化工具和参与项目的工程师。项目专属的安全策略、合规要求、架构决策和用户明确要求优先于本模板；如果两者冲突，必须记录冲突、采用更严格的要求，并同步更新文档。

## 1. 项目配置

以下信息是项目的单一入口。不得让代理根据目录猜测命令、主入口或发布边界。

```yaml
project:
  name: "<project-name>"
  description: "<one sentence>"
  primary_entrypoint: "<main executable/service/library entrypoint>"
  source_dirs: ["<source-dir>"]
  test_dirs: ["<test-dir>"]
  tool_dirs: ["<tool-dir>"]
  docs_dirs: ["<docs-dir>"]
  private_dirs: ["<ignored private-data-dir>"]

commands:
  format: "<format-check-command>"
  lint: "<lint-command>"
  typecheck: "<typecheck-command>"
  unit_test: "<unit-test-command>"
  integration_test: "<integration-test-command>"
  e2e_test: "<end-to-end-command>"
  package: "<package-command>"
  package_validate: "<package-validation-command>"
  release_gate: "<release-gate-command>"

status:
  source_of_truth: "<status-document>"
  test_matrix: "<test-matrix-document>"
  coverage_matrix: "<coverage-document>"
  release_report_schema: "<machine-readable-report-schema>"
```

如果某项能力不适用于本项目，必须写明 `not_applicable` 及原因，不能留空，也不能让工具隐式跳过。

## 2. 总体原则

- 先确认事实，再作判断；先读取仓库规则、当前实现、测试、依赖和工作区状态，再提出方案或修改代码。
- 先锁定目标、非目标、范围、Owner、主路径、接口、失败行为和验收标准，再开始跨模块实现。
- 修复根因，不用局部补丁、假数据、静态报告或隐藏 fallback 掩盖问题。
- Fail Fast：错误不得静默转换成成功。允许继续的降级必须显式声明、可观察、可测试，并由配置或发布 profile 决定。
- 不把“文件存在、类型存在、接口能调用、进程能启动、窗口存在或报告生成”当作产品能力已经完成。
- 不把 planned、reopened、design-only、fixture-only、contract-only、smoke-only 或未接入主入口的代码标记为完成。
- 保留与当前任务无关的工作区改动；不得使用破坏性 Git 操作覆盖用户工作。
- 大规模重构、公共 API 变更、数据迁移和实验性改动必须在独立分支或独立工作区中进行。
- 文档、代码、测试、报告和状态必须互相一致；状态结论只由实际验证证据支持。

## 3. 生产级实施流程

### 3.1 发现

开始任务前必须检查：

- 根目录和子目录中的 `AGENTS.md`、README、设计文档、契约文档、状态文档和迁移记录；
- 当前分支、工作区改动、依赖锁文件、构建配置、测试入口和发布入口；
- 目标功能的实际 Owner、主入口、调用链、数据格式、权限边界和既有诊断码；
- 已有失败测试、已知 blocker、未完成项和历史兼容要求。

如果实现与文档不一致，先记录真实现状和影响范围，再决定是修代码、修文档还是保持阻断状态。

### 3.2 定义

每个实现任务必须明确：

- Goal：要交付的用户可见能力；
- Non-goals：本次明确不做的范围；
- Owner：唯一负责的模块、服务、程序或 provider；
- Main path：从输入、校验、状态修改到输出的真实调用链；
- Contract：公共 API、schema、权限、错误语义和兼容策略；
- Failure：空输入、边界、非法、重复、冲突、取消、重试、恢复、资源缺失和 provider 不可用时的行为；
- Evidence：测试级别、报告字段、hash、构建身份、package/session identity 和人工验收要求；
- Release gate：必须阻断的条件、允许继续的 warning 和禁止使用的弱证据；
- Documentation：需要同步的设计、状态、测试矩阵、覆盖率、迁移和手册页面。

如果关键取舍不能从仓库事实推导，必须在实施前确认；选择确定后，在本次任务中视为约束，不得中途无记录地改变。

### 3.3 设计

- 公共契约先于具体实现；先定义所有者、输入输出、生命周期、版本、权限、诊断和迁移，再实现内部细节。
- 设计文档描述目标、边界和契约；状态文档描述当前实现、证据和 blocker；不要把未来计划写成现有能力。
- 每个模块必须能够从设计走到契约、公共 API、数据格式、主入口、测试场景、发布门禁和使用手册。
- 跨模块能力必须通过稳定接口、版本化 schema、事件、效果或受控 service 暴露，不得直接访问另一个模块的内部状态。
- 需要新增抽象时，先确认已有模式不能满足需求；不得为单一实现堆叠无必要的层次。

### 3.4 实现

- 主路径必须真正调用设计指定的 Owner/provider；禁止旁路调用 reducer、测试 helper、headless provider、内部命令或直接修改状态。
- 权威状态只能通过统一的事务、事件、mutation、effect 或 action 路径修改；失败时不得提交部分状态。
- 异步任务可以执行 IO、计算或等待，但不能依赖任务完成顺序改变权威状态。结果必须在明确的边界进入有序状态更新路径。
- 每个资源、session、provider、插件和后台任务都必须有明确的创建、使用、取消、恢复、保存、加载和关闭行为。
- 任何兼容行为、fallback、重试和自动修复都必须声明条件、优先级、最终选择和失败上限；不能使用“尽量成功”的隐式策略。
- 公共 API、配置、CLI、数据格式或协议发生变化时，必须同时更新契约、迁移说明、兼容测试和发布门禁。

### 3.5 验证

验证必须从便宜到昂贵逐层执行：格式化和静态检查、单元测试、负向测试、集成测试、真实主路径、端到端测试、打包/安装验证、平台验证和发布门禁。

验证失败、超时、报告无效、构建身份不匹配或依赖缺失都必须保留为明确 blocker；不能用局部通过、旧产物、缓存、静态报告或重新命名来替代失败证据。

### 3.6 对账与交付

完成实现后必须同步：

- 当前状态和完成度；
- 测试矩阵、覆盖率和发布检查；
- 公共 API、schema、迁移和兼容说明；
- 操作手册、开发手册和最近的索引；
- 已知限制、未完成范围、阻塞原因和下一步所需证据。

交付说明必须区分“已实现”“已验证”“仅有局部证据”和“仍然阻断”，不得拔高结论。

## 4. 架构与模块边界

- 模块保持单一职责；依赖方向必须明确，低层契约不能依赖高层 UI、具体平台、业务 provider 或工具实现。
- 核心层不持有编辑器控件、窗口对象、GPU/audio 原生句柄、文件描述符、网络连接、provider secret 或具体后端对象。
- 跨边界接口只传递稳定 ID、版本化 DTO、受限字符串、hash、section reference、诊断和有界 payload。
- 不跨边界传递内部对象所有权、裸指针、trait/object handle、未审计的 Debug 对象或隐含生命周期。
- 调试接口返回快照、报告或只读视图；不能通过调试接口绕过权限、事务、审计、状态机或发布门禁。
- 生产入口、测试入口、离线工具和开发 helper 必须明确区分；测试 helper 不能被生产主路径隐式复用。

## 5. 公共接口、Provider 与插件

### 5.1 接口要求

每个公共接口必须说明：

- 输入约束、输出语义、错误类型和稳定诊断码；
- 线程/并发模型、取消、超时和重试策略；
- 所有权、生命周期和资源释放责任；
- 权限、capability、数据出境和敏感字段策略；
- schema/API 版本、兼容范围、迁移方式和废弃计划；
- 单元、负向、集成和发布门禁测试。

### 5.2 Provider 选择

- Provider 必须由 manifest、配置、service registry 或明确 binding 选择。
- 不能按注册顺序、排序后的第一个实现、环境碰巧存在的实现或缺失时的隐式默认值选择。
- 缺 binding、多个冲突 binding、能力不足、权限不足、版本不匹配、hash 不匹配或 profile 不兼容必须返回 blocking diagnostic。
- 公开的选择 API 和发布门禁必须使用同一套选择语义，不能出现不同调用入口得出不同结果。

### 5.3 插件生命周期

插件/扩展必须声明版本、构建身份、能力、权限、依赖、冲突策略、启用范围、发布资格和诊断来源。

加载前校验 descriptor、binary/source hash、工具链或 ABI 兼容性、依赖闭包和许可证。卸载前必须停止调用、清理 callback、释放资源、保存或迁移 opaque state，并生成卸载结果。

运行中热替换只有在契约明确支持并且生命周期、状态迁移、并发和回滚均有证据时才允许；否则默认不支持。

## 6. 状态、并发、保存与恢复

- 权威状态必须有唯一来源；UI、缓存、报告和日志不能反过来成为状态真源。
- 异步结果按显式顺序进入状态更新；不得依赖 task completion order、wall-clock time、线程调度或未排序回调。
- 状态 snapshot 必须包含继续执行所需的 ID 生成器、事件队列、延迟队列、等待项、事务/MutationLog、效果记录和必要的 provider state。
- load/restore 必须验证 schema、版本、hash、依赖、身份和完整性；任何校验失败都不得部分提交。
- replay 必须消费已验证的记录结果，不重新请求 live provider、不依赖当前环境偶然状态，也不允许未记录的外部输入改变结果。
- 重复调用、乱序输入、重复 tick、回退 tick、取消后回调和过期异步结果必须有明确的拒绝或幂等规则。

## 7. 数据格式、构建产物与安全

- 类型或 schema 是数据格式的唯一真源；文本描述、生成代码和二进制容器必须由同一契约校验。
- 所有 ID、section、prefix、layer、URI、映射和优先级都必须校验非空、合法、唯一和无冲突。
- Builder 和 reader 都要拒绝重复 ID、schema/codec/hash 冲突、同名竞争、越权 overlay 和不完整 section；不能读取第一条并静默忽略其余内容。
- 数据格式必须有显式版本和迁移链；不支持的版本、缺失迁移器和迁移后 hash 不一致必须阻断。
- 构建和发布产物只引用项目内相对路径或受控 locator，不写入本地绝对路径、用户名、私有环境值或未授权外部资源。
- 报告、package、save、replay、日志和 Git 不得包含 secret、token、密钥、商业正文、原始媒体、完整规则书、脚本文本、bytecode 或可复原 payload。
- 敏感原始数据只允许存放在明确的 ignored/private 目录；提交到仓库的内容只能是经过审计的 schema、manifest、hash、尺寸、计数、coverage 和 diagnostic。
- 加密、密钥和访问控制只能通过明确的 provider/descriptor/权限契约表达；仓库不得内置发布密钥或绕过访问控制的实现。

## 8. 可观测性与错误处理

- 每个关键事件使用稳定的 `event` 名称、domain/category、诊断码、状态、计数、step、版本或 hash。
- 日志记录选择、映射、资源生命周期、fallback、队列、缓存、provider、session 和失败原因；不记录业务 payload、secret、原生句柄、完整对象 dump 或绝对路径。
- 日志不参与权威状态、save、replay、业务 hash 或发布结论；machine-readable report 与人类可读日志分离。
- 低级别日志队列丢弃时累计计数并写入独立 critical path；高等级错误不能依赖普通日志队列成功。
- 只由拥有根因或最终处置权的边界记录最终错误；中间层返回带上下文的错误，不沿调用栈重复记录同一故障。
- `WARN` 只能表示允许继续的显式降级；不可继续、完整性失败、权限失败、身份不匹配和发布阻断使用明确的 blocking/error 结果。
- crash artifact、诊断转储和敏感 trace 默认 local-private，不进入 package、报告、Git 或自动上传，除非另有明确安全策略。

## 9. 证据等级与完成状态

建议使用统一证据等级：

| 等级 | 可证明内容 |
| --- | --- |
| E0 | 文件、类型、配置、接口或报告存在；不能证明行为完成 |
| E1 | 局部单元测试、fixture 或静态契约验证通过 |
| E2 | 跨模块调用、构建、package、保存/恢复或 replay 链路通过 |
| E3 | 真实主程序、真实输入、真实输出、宿主消费、同一运行身份和产品场景闭合 |
| E4 | 跨环境/平台、长流程、恢复、性能、资源规模、发布包和人工 signoff 闭合 |

每个能力必须在状态中写明：Owner、主入口、证据等级、测试/报告 ID、当前限制和阻断条件。产品完成所需最低等级由项目配置定义；没有满足最低等级不得标记 `done`。

## 10. 测试与 Release Gate

### 10.1 必测场景

所有重要功能至少覆盖：

- 正常输入、空输入、边界值和超大输入；
- 非法格式、缺字段、重复 ID、冲突配置、权限拒绝和版本不兼容；
- provider 不可用、资源缺失、网络/IO 失败、超时、取消和重试；
- 重复调用、并发调用、乱序结果、恢复、save/load 和 replay；
- 真实主程序从输入到输出的端到端路径；
- 失败时无部分提交、无错误状态变化、无错误报告和无可发布产物；
- 日志、报告、package 和 artifact 的敏感信息脱敏。

### 10.2 验收规则

- Release Gate 是发布前唯一权威检查入口；编辑器、CLI、CI 和人工流程调用同一 validator。
- 每个检查声明 ID、输入、Owner、阻断条件、允许 warning、证据字段、source reference 和期望输出。
- 报告必须是版本化、machine-readable、可重复解析的格式；失败原因必须包含稳定 diagnostic code。
- 文件存在、接口存在、进程启动、窗口存在、截图 hash 变化、host consumed trace、静态 HTML、外部 expected route 或报告文件存在，单独都不能证明产品行为完成。
- Gate 必须拒绝无效报告、身份断裂、旧产物、弱证据升级、未声明 fallback、路径泄露和 payload 泄露。

## 11. 可复现构建与验证

- 每个 checkout/worktree 使用独立且可识别的构建和 artifact 根目录。
- 构建身份至少绑定当前提交或工作区状态、源码/manifest hash、依赖锁 hash、工具链 fingerprint、配置、target、profile 和 feature fingerprint。
- 动态 fixture、插件、生成代码和缓存产物必须校验构建身份；身份不匹配时强制重建，不能只检查文件是否存在。
- 报告只记录相对 artifact path、role、hash、byte size 和 build identity，不写本地绝对路径。
- 验证命令必须使用当前 checkout 的依赖和产物；不得混入其他 worktree、全局安装、旧二进制或共享缓存中的不明文件。
- 命令超时、部分测试通过、单包通过或旧报告存在都不能替代完整验证。

## 12. 文档与状态治理

- 设计文档写目标、边界和契约；实现文档写调用链、数据流和失败语义；状态文档写真实完成度和证据。
- 每个实现工作项完成后同步更新状态、测试矩阵、coverage、Release Gate、迁移说明、README/索引和手册。
- 新增模块必须同时接入构建系统、主入口、测试矩阵、可观测性覆盖、发布门禁和用户文档。
- 文档示例必须使用跨平台、可复制的命令；项目脚本优先使用跨平台实现，禁止把个人机器路径写入仓库。
- 文档不得使用营销语言，不得把 planned work、roadmap、接口草图或 fixture 结果写成 implemented behavior。
- 中文技术文档应保持自然、准确、简洁；保留 API、type、command、schema 和文件名等技术术语，不改变其语义。

## 13. 交付报告格式

每次实现或修复交付时，必须说明：

1. 目标和实际完成范围；
2. 关键代码、契约、数据格式和文档变化；
3. 主路径是否真实接通；
4. 执行过的验证命令及结果；
5. 证据等级、报告 ID、构建/package/session identity；
6. 已知 blocker、限制、未覆盖场景和允许的 warning；
7. 后续工作及其前置条件。

如果验证未完成或环境阻塞，必须明确写出“未验证”或“blocked”，不能以“基本完成”“应该可用”或静态检查通过代替结论。

## 14. 项目落地检查清单

新项目首次使用本模板时，必须完成：

- [ ] 填写项目配置和所有验证命令；
- [ ] 指定设计、契约、实现、状态、测试和手册的文档入口；
- [ ] 定义状态标签、证据等级、诊断码和 Release Gate report schema；
- [ ] 定义源码、测试、工具、构建产物和 private/ignored 数据边界；
- [ ] 建立格式化、静态检查、单元、集成、端到端和打包验证入口；
- [ ] 建立负向测试矩阵和敏感信息泄露检查；
- [ ] 建立构建身份、artifact hash 和旧产物隔离策略；
- [ ] 为每个正式模块登记 Owner、主入口、公共契约、测试和发布检查；
- [ ] 确认 planned/reopened/fixture-only 不能计入完成度；
- [ ] 首次完整验证通过后，记录基线报告和已知限制。
