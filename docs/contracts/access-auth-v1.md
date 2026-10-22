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
