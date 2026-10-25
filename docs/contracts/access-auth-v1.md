# Access 身份与会话契约 v1

## OIDC 登录事务

`GET /auth/login?return_to=<门户路径>` 为每次浏览器登录创建独立的 state、PKCE、nonce 和浏览器绑定 Cookie。事务 Cookie 名称为 `__Host-labweaver_oidc_<sha256(state)>`，属性固定为 `Secure; HttpOnly; SameSite=Lax; Path=/`，不设置 `Domain`。事务只允许一次消费，服务端在 PostgreSQL 行锁内同时校验 state、Cookie 绑定和有效期。

同一浏览器的多个标签页使用不同事务 Cookie；Cookie 缺失、重复、跨浏览器或绑定不匹配都不能消费事务。回调失败会清理当前事务，不清理其他标签页事务。

| 条件 | HTTP | diagnosticCode |
| --- | ---: | --- |
| 非法 `return_to` | 400 | `LW_AUTH_OIDC_RETURN_TO_REJECTED` |
| 缺少 state | 401 | `LW_AUTH_OIDC_STATE_REQUIRED` |
| 缺少、重复或不匹配的事务 Cookie | 401 | `LW_AUTH_OIDC_BROWSER_REJECTED` |
| 事务过期、已消费或 state 无效 | 401 | `LW_AUTH_OIDC_STATE_REJECTED` |
| provider 返回拒绝 | 401 | `LW_AUTH_TOKEN_INVALID` |
| code 兑换或 OIDC provider 不可用 | 503 | `LW_AUTH_OIDC_TOKEN_EXCHANGE_FAILED` |

`return_to` 必须是单个 `/` 开头的门户路径，最多2048字节；禁止外部 URL、`//`、反斜杠和控制字符。callback 地址必须是同源、无查询和片段的精确 `/auth/callback`。

## BFF 会话和退出

BFF 会话通过 HttpOnly Cookie 识别，写请求还必须携带允许的 `Origin` 和当前 CSRF token。退出必须先通过该边界；Access 撤销 BFF 会话、清理 Cookie，并终止属于该 BFF 会话的浏览器 Console 连接。provider logout 使用返回的 `logoutUrl` 顶层导航完成，不用后台 fetch 跟随跨源重定向。

`GET /api/v1/auth/csrf` 返回必填的 `actorId`、`csrfToken` 和 `expiresAt`。`actorId` 来自签发 token 的同一个 BFF 会话；客户端在发送写请求前用它确认 token 属于当前页面的 actor。以下保证只适用于门户 BFF 的 `/api/v1` 与 `/auth` 写接口：token 仍按会话校验，即使两个会话属于同一 actor，也不能互换 token；若共享 Cookie 已切换到其他会话，旧 token 的写请求返回 403 `LW_AUTH_CSRF_REJECTED`，不会以替换后的身份写入。客户端不得因这一拒绝自动重放原写请求。

运行时代理 `/connect/{endpointGrantId}/` 不使用 BFF CSRF token；它与门户的 origin 隔离见 [#297](https://github.com/TeamMonad/LabWeaver/issues/297)。在 #297 完成之前，运行时内容与门户同源，这是已知风险。

SSH Gateway 会话不与 BFF Cookie 混用。它绑定独立 AccessGrant、SSH key 和 endpoint；BFF 退出不撤销仍独立有效的 SSH grant。grant 到期、显式撤销、成员撤销或环境授权变化必须使新连接失败，并由既有 Gateway 终止已建立连接。

## 内部服务 JWT

内部 HTTP 请求必须同时满足服务端 TLS 和 JWT 验证。接收方使用配置的 issuer、目标 audience、允许算法和 JWKS，验证签名、`exp`、`nbf`、`azp`、非空 subject 以及 `resource_access[<audience>].roles` 中的必需权限。失败统一映射为服务鉴权拒绝；网络可达或客户端自填 actor/role/principal 不能替代该验证。服务令牌不是浏览器 BFF 会话，也不能被另一目标 audience 复用。

默认 API 错误使用公共 `ProblemDetails` (`application/problem+json`)，保留 HTTP 状态、`diagnosticCode`、关联 request/trace ID 与 retry 分类。Gateway 专用 412 冲突响应继续使用 `diagnosticCode/currentRevision/state` 三字段正文，不能将通用 ProblemDetails 字段添加到该严格消费者契约。

## 验证入口

增量协议和服务 JWT 测试：

```sh
cargo test -p auth --test service_auth --locked
cargo test -p access-service --test postgres oidc_binding_is_checked_before_atomic_consumption_and_expiry --locked
```

真实浏览器边界入口和依赖要求见 [`docs/usage.md`](../usage.md)。测试夹具不能替代生产 SSH/sshd 传输验收。

## 运行时 origin 隔离与浏览器交接

`EndpointGrant.connectUrl` 仍为门户相对路径 `/connect/{endpointGrantId}/`，前端继续使用
`window.open(connectUrl)`。入口不在门户 origin 返回运行时内容。Access 必须配置
`browser.runtime_base_origin`（例如 `https://runtime.example.org`），每个 endpoint grant 使用独立
主机 `g-<grant UUID 去掉连字符>.runtime.example.org`。UUIDv7 的随机部分提供不可猜测的 DNS 安全
标识；不能把多个 grant 放回同一 host、仅靠 Cookie Path 隔离脚本。门户主机不得等于基础域或位于
其通配子域内；缺配置、非 HTTPS、非 DNS 基础域均拒绝启动，不回退同源。

交接先导航到该 grant 主机的 `/runtime-bootstrap/{grant}`，设置随机的 host-only、Secure、HttpOnly、
SameSite=Lax 绑定 Cookie，再返回门户 `?handoff=true#binding=…`。绑定只在 fragment 中，不进入
HTTP 请求目标或默认访问日志。门户可信 nonce-CSP 脚本立即清除 fragment，并以当前 BFF 会话、
门户 Origin 和 CSRF 保护的 JSON POST 发送绑定；返回的一次性交接 URL 再由顶层导航打开。
浏览器绑定与交接 token 仅以散列保存。PostgreSQL 同一权威时间签发 `issued_at`、至多 30 秒的
handoff 和至多五分钟且不超过 session 截止的 capability；条件 UPDATE 原子兑换，并发与顺序重放
都只有第一次成功。交接响应不缓存，使用 `Referrer-Policy: no-referrer`，隔离 opener。

运行时导航到固定的 `/runtime-handoff/{grant}#token=…`。服务器只收到不含秘密的 grant 路径，
返回每个 grant 完全相同的页面：no-store、no-referrer、严格 CSP，仅放行它自己内联脚本的 SHA-256
hash（不允许任意 inline 或额外 nonce）。页面立即清除 fragment，以 CORS 模式的同源 fetch POST
body 提交 token，在 no-referrer 策略下仍保留自己的 Origin；成功的 303 之后再导航到应用根路径。
POST 必须来自该 grant 自己的运行时 Origin，绑定 Cookie、PG 时钟及原子一次性条件仍全部验证。
成功后 303 到 `/connect/{grant}/`。旧的 GET `/runtime-handoff/{grant}/{token}` 兑换路径已删除。
兑换后设置 `__Secure-labweaver_runtime` capability Cookie：HttpOnly、Secure、SameSite=Lax，无 Domain，
host-only 绑定该 grant 的独立主机，Path 限定 `/connect/{grant}/`。每次请求同时校验主机中的 grant、
路径中的 grant、capability 中的 grant/actor/session、数据库期限、有效 BFF session、当前
AccessGrant/EndpointGrant、成员动作和真实 Environment 权威状态。退出、授权撤销或到期后下一次
请求立即拒绝，不访问上游。capability 过期后从门户重新打开，不复制交接 URL 续期。

统一规范化 authority（主机小写、去尾点、去默认端口），分别判定已配置门户、精确 grant 主机或拒绝。
HTTP/1 Host 与 HTTP/2 URI authority 冲突、缺失、未知主机一律拒绝，不信任 `X-Forwarded-Host`。
运行时 host 不提供 `/api/v1`、`/auth` 或 Console。门户 API 拒绝运行时 Origin，不开放 CORS。
运行时跨 origin 请求即使来自同站的另一个 grant、浏览器自动附带目标 Cookie，仍拒绝并且不访问上游；
Fetch Metadata 同时拒绝省略 Origin 的跨主机 no-cors 资源请求，顶层交接导航仍可完成。
写方法要求该 grant 自己的规范化 HTTPS Origin 和 Edit 动作，不使用门户 CSRF；门户交接 POST 本身
仍须门户 CSRF。响应带 `Cross-Origin-Resource-Policy: same-origin`、opener 隔离与
`Origin-Agent-Cluster: ?1`，运行时文档保持 origin-keyed，不依赖 Cookie Path 或同站属性隔离脚本。
上游不接收 BFF/capability Cookie 或 Authorization，Set-Cookie 不转发；根相对重定向保持当前 host
及 grant 路径，外部重定向拒绝。BFF `__Host-labweaver_session` 保持 host-only，只发给门户。

| 条件 | HTTP | diagnosticCode |
| --- | ---: | --- |
| 错误 host/Origin 或在门户尝试代理运行时写请求 | 403 | `LW_ACCESS_RUNTIME_ORIGIN_REJECTED` |
| 交接/绑定/capability 缺失、错误、重放、过期或跨 grant | 403 | `LW_ACCESS_RUNTIME_CAPABILITY_REJECTED` |
| 已消费 capability 对应的 BFF 会话退出/到期 | 401 | `LW_AUTH_SESSION_REJECTED` |
| grant 已撤销/到期、成员或端点不可用 | 403 | `LW_ACCESS_RUNTIME_DENIED` |

回归入口（HTTP 测试运行真实 Access、Environment resolver 与 PostgreSQL；只有 IdP 和运行时应用是边界夹具）：

```sh
cargo test -p access-service runtime_handoff_isolated_bound_one_time_and_live_revocation --locked
pnpm --dir web install --frozen-lockfile
pnpm --dir web exec playwright install chromium
cargo test -p access-service chromium_runtime_script_cannot_use_portal_session --locked -- --ignored --nocapture
python -m unittest discover -s tests/ansible -p test_runtime_origin.py
```

Chromium 使用同站、不同主机的两个真实 grant（不同项目/环境），确认 A 页面脚本对 B 的读、写及
no-cors 请求均不能访问应用上游，即使浏览器自动附带 B 的 Cookie；同时验证 HTTP/2、门户会话
隔离和请求/访问日志目标不含实际 token/binding。CI 的 `playwright-ci` 显式执行浏览器门禁；普通 Rust 测试
不要求 Node/Chromium。临时 TLS 夹具不能替代真实 DNS/集群证书验收。

真实 Nginx 日志回归：`cargo test -p access-service runtime_handoff_credentials_never_enter_real_nginx_fault_logs --locked -- --nocapture`。
测试使用 Kind 的 `deploy/config/local-dev-portal.nginx.conf` 与 Web Containerfile 固定的同版 Nginx，
真实签发 token/binding 后注入连接拒绝与响应超时；access/error 两类日志均不能含实际秘密，同时
必须保留 502/504、grant 和上游故障原因。不通过降低 warn 级别或关闭运行时访问日志掩盖泄漏。
