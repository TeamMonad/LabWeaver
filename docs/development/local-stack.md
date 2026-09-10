# 本地六服务开发环境

`tools/local_dev.py` 管理一次性的 Kind 开发环境，运行 Control、Access、Environment、Agent、Evaluation、Resource，以及 PostgreSQL、Keycloak、NATS 和 MinIO。应用使用仓库的容器构建与 Helm 模板；业务数据和服务调用经过真实后端。

## 准备与运行

需要可用的 Linux Docker 引擎、Docker Buildx、Kind、kubectl、Helm、Python、OpenSSL 和 OpenSSH 的 `ssh-keygen`。首次运行会拉取基础镜像并编译 Rust 与 Web 镜像，应预留足够的磁盘、内存和构建时间。固定版本的基础镜像在服务启动前由各 Kind 节点按 digest 预拉取，下载不会占用服务启动的等待时间。已有应用构建缓存会被复用。

在仓库根目录运行：

```sh
python -m pip install -r tools/requirements-local-dev.txt
python tools/local_dev.py up
python tools/local_dev.py status
python tools/local_dev.py down
```

以 `up` 输出的实际访问地址为准，不假设固定端口。生成的运行配置和凭据保存在 Git 忽略的项目目录中，不应提交或分享。

本地 MinIO 工件桶在首次创建时通过固定版本的 `mc mb --with-lock` 启用 Object Lock，并单独校验桶的锁能力和版本控制。Bootstrap 不设置固定的桶默认保留期；Evaluation freeze 会为每个对象写入 Governance 模式和 `retainUntil`。如果已有桶缺少 Object Lock，启动会立即失败并保留原桶，不删除、重建或迁移其中的数据；需要使用本次运行拥有的本地环境重新执行 `down` 后再 `up`。

入口使用 `127.0.0.1.nip.io`，启动时要求该域名仅解析到回环地址。集群内部由专用 Kind 的 DNS 将相同域名解析到入口服务，登录、上传和服务请求因此使用同一 HTTPS 地址；不修改宿主机 hosts。若网络 DNS 拒绝这类回环域名，启动会报告错误。

HTTPS 证书由本次运行的本地 CA 签发。命令不自动修改宿主机或浏览器的全局信任设置；交互使用前，需在自己选择的开发浏览器中信任该 CA。自动化测试也应只在隔离测试环境中配置这份信任，不关闭证书或主机名校验。

## 所有权与清理

每次启动分配独立的集群、镜像仓库及运行标识，并使用专用 kubeconfig。命令不切换用户默认 Kubernetes 上下文。`down` 根据本次运行记录检查资源与进程所有权，删除该开发环境及其数据；不要将需要长期保存的数据放入一次性环境。

启动失败会尝试清理已创建的资源。清理失败时保留状态，先根据具体错误恢复相应工具或进程，再运行 `down`。不要通过删除状态文件绕过所有权检查，也不要用全局 Docker 清理命令代替此入口。

## 验证范围

本地入口用于业务开发和 CPU 容器流程。模型调用和镜像构建依赖必须明确配置，不能把未配置的外部能力视为成功。默认关闭镜像构建执行器；本地镜像仓库只分发平台镜像，不提供 Harbor 管理接口或 BuildKit 构建能力。默认不发起付费模型请求、真实扣款或远端部署。

管理员批准不含 GPU 的资源申请时，填写当前 Environment 配置中的执行后端绑定。本地配置使用 `kubernetes-work-local-hostpath`，来自 `deploy/config/environment-providers.local-hostpath.example.json` 中容器条目的 `binding`。该文件与其他部署共用 Provider 配置数组格式，本地只配置容器后端；GPU 目录为空不会阻止 CPU 申请。其他部署应使用自己的实际绑定。

本地 Kind 的 `standard` StorageClass 使用 `ReadWriteOnce` 工作区访问模式；运行时 Pod 和冻结 worker 在单节点上共享同一个工作区 PVC。生产 NFS 配置在 `environment-providers.json.example` 中显式使用 `ReadWriteMany`。

业务集成测试可显式运行 `python tools/local_dev.py up --external-fixtures`。该测试配置仅替换外部 Claude 进程和 NATS 构建执行器响应，保留真实业务服务、身份校验、数据库、工件绑定与审批流程。启动时会从 Web Containerfile 的 `work-runtime-fixture` 阶段构建并推送一次带有 `/opt/labweaver/workspace-seed` 的 Nginx 镜像，供构建执行器夹具复制；生产 Web 镜像仍使用完整的 `runtime` 阶段。构建执行器夹具只在运行拥有的本地 Registry 内复制这个预构建 OCI 镜像，不读取候选生成的 Dockerfile 或执行实际镜像构建，因此该模式不能验证模型质量或实际镜像构建能力；启动输出与本地状态会标明启用了这些夹具。普通 `up` 不启用夹具。

服务就绪后，可在本次 Kind 集群内运行浏览器测试：

```sh
python tools/local_dev_e2e.py run --project teacher
```

测试入口构建与当前 Playwright 版本匹配的浏览器镜像，推送到本次本地镜像仓库，并启动临时 Job。它从本次运行读取地址、CA 和测试账户，把 CA 信任限制在测试容器内；账户密码通过临时 Secret 文件传入。命令返回浏览器测试的退出码，并清理自己创建的 Job、Secret 和网络策略。可重复使用 `--project` 选择其他角色测试，或用 `--grep` 限定测试名称；不会启动新的集群或修改默认 kubeconfig。

Kind 环境不提供真实 KubeVirt、GPU、vGPU 或生产网络隔离验收。对应功能需在具备相应设备、插件和网络策略实现的环境中另行验证。费用页面的核算结果也不表示已经执行支付。

本地构建另外提供通用评测运行镜像，包含现有 C++17 和 Ansible Probe 执行工具，并把构建得到的 digest 写入 Control 的评测运行配置。Evaluation 协调器使用业务服务镜像执行冻结任务，两者分别配置。
