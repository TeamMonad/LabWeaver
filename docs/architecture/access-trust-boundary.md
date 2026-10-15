# 访问与内部身份边界

本文定义 v3 访问设计。已执行的本地验证及尚未接通的路径在 Issue #180 的 PR 中说明。

## 身份与授权

Keycloak 验证用户和服务身份；Access 验证项目、课程、环境、端点和租约范围。网络可达、服务账户身份与用户拥有资源是三种不同判断，不得互相替代。

内部服务使用独立 Keycloak 服务账户通过 client credentials 获取短期访问令牌。接收方验证签名、issuer、audience、有效期和调用者权限；不得相信客户端自行填入的 actor、role 或 principal。已有 BFF 会话与受限代理用户上下文仍需 Access 查询当前成员和资源授权。

每个接收服务使用自己的 audience，调用方按目标获取并缓存令牌；Keycloak 的 audience mapper、角色与 scope 必须显式配置，不能通过让所有服务接受同一个 audience 来省略配置。Access 转发 Resource 用户请求时同时携带服务令牌和短期签名用户委托，两者缺一不可。Environment 与 Evaluation 的用量写入分别限定到各自拥有的环境或任务，具备一般服务身份不代表可以代写另一领域的用量。

接收方从 `resource_access[audience].roles` 读取服务调用权限。Keycloak 签发的角色受到服务账户角色与调用方角色范围映射的共同限制，再由 audience resolve mapper 生成允许访问的目标 audience。client credentials 请求不把业务权限当作 OAuth scope，也不依赖额外的 audience 参数；客户端收到令牌后仍检查目标 audience，接收方再检查调用者及所需角色。配置依据 [Keycloak 服务账户与 audience 文档](https://www.keycloak.org/docs/latest/server_admin/index.html)，不以开启 Full Scope Allowed 代替权限映射。

调用账户自身不会由 audience resolve mapper 自动加入 `aud`。对于 Access 网关、Environment 执行器这类同一领域账户调用本领域服务的路径，依据实际自身目标角色授权配置显式 Audience mapper；其他账户不因此增加目标 audience。

令牌获取与内部 HTTP 使用服务端 TLS；凭据来自 Secret 文件，不写入 URL、命令参数或日志。不保留固定 SPIFFE principal、假 mTLS 参数或关闭验证的生产 fallback。保留 NATS 与 SSH 各自实际需要的认证和加密。

## OIDC 登录事务

浏览器以顶层导航访问 `GET /auth/login?return_to=<门户路径>`。Access 为每次登录生成独立的 state、PKCE verifier、nonce 和浏览器秘密，并将事务加密保存在 PostgreSQL。浏览器秘密只通过 `__Host-labweaver_oidc_<state的SHA-256>` Cookie 发送；Cookie 使用 `Path=/; Secure; HttpOnly; SameSite=Lax`，不设置 Domain。有效期由 `browser.oidc_transaction_ttl_seconds` 控制（60–900 秒，示例为300秒）。多个标签页分别保留各自 Cookie。

回调在数据库行锁内核对 state、浏览器秘密和到期时间，然后原子地标记事务已消费。缺少、伪造或重复事务 Cookie 时返回 `401 / LW_AUTH_OIDC_BROWSER_REJECTED`，不消费另一浏览器的事务。过期和已消费事务返回 `401 / LW_AUTH_OIDC_STATE_REJECTED`。通过浏览器校验后，身份提供方拒绝、缺少 code 或兑换失败也会使本次事务结束；用户须重新发起登录。成功与失败回调只删除本次 Cookie，其他标签页可以继续。登录与回调响应均使用 `Cache-Control: no-store` 和 `Referrer-Policy: no-referrer`。

`return_to` 只接受以单个 `/` 开头、最多2048字节的门户路径，可包含查询和片段，例如 `/researcher/workspaces?project=example#active`。Access 在保存事务前和最终重定向前分别校验目标。外部 URL、`//` 网络路径、反斜杠及控制字符返回 `400 / LW_AUTH_OIDC_RETURN_TO_REJECTED`，且不会创建事务。省略参数时返回配置的门户地址。配置的 callback 必须使用 `/auth/callback`，没有查询、片段或用户信息；默认门户地址必须与 callback 同源。Keycloak 须注册精确 callback，不能使用通配符。两条成功重定向均返回 HTTP 307。

登录不创建计费资源或删除 Work 数据。发布这一变更会使更新前尚未完成的登录事务失效；这些用户重新发起登录即可。既有 BFF 会话不受事务格式更新影响。

隔离浏览器集成用例复用 Access 的生产路由、临时真实 Keycloak 和 PostgreSQL。先安装仓库锁定的 Web 测试依赖与 Chromium，再显式运行：

```sh
pnpm --dir web install --frozen-lockfile
pnpm --dir web exec playwright install chromium
cargo test -p access-service --bin access-service chromium_oidc_navigation_enforces_browser_binding_and_session_lifecycle --locked -- --ignored --nocapture
```

该用例需要 Node、Docker 和 Playwright Chromium。它在 `https://localhost` 门户与 `https://127.0.0.1` Keycloak 之间完成原生跨站导航，覆盖浏览器隔离、多标签、重放、过期、返回目标、刷新和 CSRF/provider 退出。返回页只用于检查登录导航，不能替代 Vue 控制台旅程。截图写入系统临时目录。浏览器仅对临时夹具的自签名证书启用测试信任选项，Access 的 OIDC/服务客户端仍执行严格 TLS 验证。

浏览器可在 HTTP/2 请求中将 Cookie 拆成多个字段。Access 从全部字段读取唯一的 BFF session Cookie；重复 session Cookie 不选取任何值。另一个标签的登录事务 Cookie 不会遮挡已建立的 BFF 会话。

OIDC callback verifies the signed ID token, including its `exp`, before creating a
local browser session. The BFF session then uses the configured
`browser.session_ttl_seconds` as its absolute lifetime and
`browser.session_idle_ttl_seconds` for idle renewal; an ID-token expiry does not
shorten that already authenticated local session. Back-channel logout and
explicit local revocation remain authoritative, and every request still loads
current actor membership and authorization state. This follows the OpenID
Connect Core requirement that an expired ID token cannot be accepted while
recognizing that ID-token expiry is unrelated to the OP authenticated-session
lifetime ([ID Token](https://openid.net/specs/openid-connect-core-1_0.html#IDToken));
logout-token validation and RP session clearing follow
[OpenID Connect Back-Channel Logout](https://openid.net/specs/openid-connect-backchannel-1_0.html#BackchannelLogoutValidation).

## 冻结请求的模型输入上下文

Access 在真实 freeze POST 完成 BFF、Origin/CSRF、环境 grant、项目 Edit 和可选课程授权后，使用同一会话与实际 decision 生成 `x-labweaver-model-input-authorization-context`。六字段来自可信 session/actor、grant scope、固定 `freezeSubmission` 和最早有效上界；session 摘要使用规范 UUID 文本的 SHA-256。上界取项目/课程 decision、BFF absolute/idle、grant 的最小值，复用既有 decision TTL；有效判定保留原精度，传输时间向下截断至毫秒。客户端该 header 不在转发白名单，服务端只发送一个 canonical JSON。

该上下文是首次证据，不是模型输入许可或 Agent 永久截止。Evaluation 保存首次 context，合法重放不替换它；旧的缺上下文记录保持缺失。Source/Rubric 可见性、外发权限和每次实际发送的当前授权仍由各权威入口复核。

## 浏览器与 SSH

浏览器通过 `GET/POST /api/v1/projects/{projectId}/llm-egress-policies/active/rubric-rules` 访问 Control 的 rubric 规则入口。Access 复用公共 operation 契约要求教师身份及当前项目 Browse（GET）或 Edit（POST）；写请求沿用 Origin/CSRF 校验。身份、项目动作或 CSRF 拒绝时不转发。Control 继续负责当前课程 instructor、rubric 精确版本、可见性与模型许可；Access 不生成这些权威事实。`If-Match`、幂等键与响应 ETag 沿既有代理规则透传。

Access 默认 API 错误使用公共 `ProblemDetails` 契约，返回 `application/problem+json`。响应保留原 HTTP 状态和 `diagnosticCode`，包含安全的 `type/title/detail/instance`、与 `x-request-id` 一致的 `requestId`、关联 `traceId` 和 `retryable`。Gateway 心跳和关闭重试的专用 412 正文仍保持 `diagnosticCode/currentRevision/state` 三字段 JSON，满足既有严格消费契约。缺少或与当前会话不匹配的 CSRF 仍拒绝写入；403 不允许原输入直接重试，客户端须刷新当前会话后由用户重新提交。

浏览器退出先向 BFF 提交带 Origin 和 CSRF 的请求。Access 撤销本地会话及控制台连接、清理会话 Cookie，并返回身份提供方的完整退出地址；浏览器随后进行顶层导航完成 [OIDC RP-Initiated Logout](https://openid.net/specs/openid-connect-rpinitiated-1_0.html)。跨源退出不能用后台 fetch 跟随重定向代替。退出响应不允许缓存，页面在会话失效时清除本地登录状态并提示重新登录。

SSH Gateway 会话绑定独立的 AccessGrant、密钥和端点，而非 BFF session。BFF 退出不自动撤销全部 SSH 授权；SSH 按 AccessGrant 到期、显式撤销及成员/环境授权变化的既定规则收尾。验证 OIDC 登录不能冒充已验证真实 SSH 传输：后者需要临时 Gateway/sshd、受控服务 JWT/TLS、真实目标端点和 grant 撤销事件。

Web Terminal、noVNC 和 SSH/SFTP 共享 AccessGrant 的范围、到期和撤销规则。浏览器保留会话、Origin、CSRF 和一次性 ConsoleCapability 检查；Environment 提供真实端点与运行状态，客户端不能选择任意目标地址。

连接时重新检查授权与环境代次；到期或撤销后禁止新连接，已有连接由服务端撤销流程终止。缓存和页面状态不能延长授权。资源不存在、身份服务或授权存储不可用时拒绝访问，并返回可排查错误。

## 不可信执行

用户可以修改自己的实验源码、guest 配置和获授权的软件环境；不能修改共享模板、隐藏测试、其他用户存储或平台凭据。安全实验按明确场景允许 guest root 或实验内网络，控制面与跨用户边界继续保留。

Environment 的 Work 配置执行入口仅需查询 Pod 与调用 `pods/exec`，不因此取得 Namespace 管理权限。kube-rs 使用 WebSocket GET 建立 exec 连接；RBAC 保留 `get` 与 `create`，因为 Kubernetes 1.35 在原有请求授权之外增加了 `create` 检查。参见 [Kubernetes 的 exec 授权变更](https://github.com/kubernetes/kubernetes/pull/134577)。

容器时间片 GPU 目录必须提示共享限制，不能将其描述成具有独占显存或故障隔离。需要更强边界的工作负载选择实际支持的独占或虚拟设备规格；缺少所需后端时拒绝启动。
