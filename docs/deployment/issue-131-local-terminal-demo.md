# Issue #131 本地 Container 终端演示

本手册重放真实的同源 AccessGrant、Environment 权威校验和 Kubernetes
exec PTY 路径。它不会启动 Fixture，也不会保存终端 transcript、截图、
录像或 Trace。

当前 ex3 的 rootless BuildKit 仍可能受本地主机 cgroup 边界阻塞。使用
已有 digest 镜像的演示可以证明 Container+xterm 交互，但不能替代同源
七镜像 package、Ansible Verify 或 Release Gate。

## 前置条件

- `labweaver-ex3` kind node 为 Ready。
- Keycloak、Access、Environment、container-executor、Web 和目标 runtime
  Pod 为 Ready。
- 本机已安装 Docker、`kubectl`、Node.js、pnpm 和仓库锁定的 Playwright。
- `demo-student-login` Secret 只存在于本地演示集群。

确认状态：

```sh
kubectl --kubeconfig "$KUBECONFIG" get nodes
kubectl --kubeconfig "$KUBECONFIG" \
  -n labweaver-system get deploy \
  access-service environment-service container-executor web
kubectl --kubeconfig "$KUBECONFIG" \
  -n lw-env-00000000-0000-7000-8000-000000000401 get pod runtime
```

## 暴露本地门户

门户和 Keycloak 使用同一个本地 443 转发器。该容器不持有凭据：

```sh
docker run --rm -d \
  --name labweaver-portal-forward \
  --network kind \
  -p 443:443 \
  alpine/socat \
  tcp-listen:443,fork,reuseaddr \
  tcp-connect:labweaver-ex3-control-plane:30537
```

验证两个入口：

```sh
curl --fail --silent --show-error --insecure \
  --resolve portal.labweaver.internal:443:127.0.0.1 \
  https://portal.labweaver.internal/
curl --fail --silent --show-error --insecure \
  --resolve keycloak.labweaver.internal:443:127.0.0.1 \
  https://keycloak.labweaver.internal/realms/workloads/.well-known/openid-configuration
```

## 准备私有登录输入

密码只能写入被忽略的 `.private`，不得出现在命令参数、日志或报告：

```sh
install -d -m 700 .private/ex3-131-demo
kubectl --kubeconfig "$KUBECONFIG" \
  -n labweaver-system get secret demo-student-login \
  -o jsonpath='{.data.password}' \
  | base64 -d > .private/ex3-131-demo/student-password
chmod 600 .private/ex3-131-demo/student-password

export LABWEAVER_STUDENT_USERNAME="$(
  kubectl --kubeconfig "$KUBECONFIG" \
    -n labweaver-system get secret demo-student-login \
    -o jsonpath='{.data.username}' | base64 -d
)"
export LABWEAVER_STUDENT_PASSWORD_FILE="$PWD/.private/ex3-131-demo/student-password"
```

## 重放

```sh
export LABWEAVER_DATA_MODE=live
export LABWEAVER_EXTERNAL_WEB_SERVER=true
export LABWEAVER_BASE_URL=https://portal.labweaver.internal
export LABWEAVER_E2E_CONTAINER_ENVIRONMENT_ID=00000000-0000-7000-8000-000000000401

pnpm --dir web test:e2e:terminal-demo -- --headed
```

演示依次执行：

1. 真实 Keycloak 学生登录；
2. 加载 Ready Container；
3. 撤销残留的活动授权并签发新 AccessGrant；
4. xterm 写入 `/workspace/.issue-131-demo`；
5. 断开并手动重连，验证 workspace 后写入 reconnect marker；
6. 切换全屏以触发 fit/resize；
7. 撤销授权，并验证旧 terminal URL 无法再次连接。

Playwright case 显式关闭 screenshot、video 和 trace。它只在内存中识别
固定 ACK，不把 stdin、stdout 或 terminal transcript 写入 evidence。

## 演示后检查

```sh
kubectl --kubeconfig "$KUBECONFIG" \
  -n lw-env-00000000-0000-7000-8000-000000000401 \
  exec pod/runtime -c runtime -- \
  test -s /workspace/.issue-131-demo
kubectl --kubeconfig "$KUBECONFIG" \
  -n lw-env-00000000-0000-7000-8000-000000000401 \
  exec pod/runtime -c runtime -- \
  test -s /workspace/.issue-131-reconnect
```

结束本机转发：

```sh
docker stop labweaver-portal-forward
```

若此集群或 private bundle 曾出现在非受控输出中，应回收整个本地集群并
重新生成 bundle；不得只轮换演示用户后将旧集群提升为安全或 Release
证据。
