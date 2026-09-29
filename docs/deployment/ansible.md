# 应用部署与基础组件

LabWeaver 使用 Ansible 调用 Helm 和 Kubernetes 模块。应用安装面向已有 Kubernetes；基础组件 bootstrap 是独立、显式的维护操作，应用升级不能隐式重建数据库、OIDC Realm、对象存储或消息流。

部署内容包含 Control、Access、Environment、Agent、Evaluation、Resource 六个业务服务，以及 Web 和访问网关。Worker 是所属服务的执行角色。启用的组件由配置和实际构建清单决定，不按固定镜像数量判断部署是否有效。

## 配置

Inventory 声明目标集群、命名空间、存储类、镜像引用和依赖端点。数据库、NATS、Keycloak、对象存储和 Registry 可以使用已有实例；部署前检查所需能力与权限，缺失配置直接报错。配置样例使用项目相对路径或 Secret 挂载位置，不包含个人机器路径和私有拓扑。

内部 HTTP 使用服务端 TLS 与 Keycloak client credentials。每个服务使用自己的客户端身份，令牌包含目标 audience 和受限客户端角色；网络策略不能替代令牌验证。用户的项目与课程权限仍由 Access 判断。Secret、私钥和客户端口令以文件注入，不进入镜像构建参数、普通配置、日志或 Git。

## 运行边界

Environment 管理环境 Namespace、Quota、PVC 和运行对象；Resource 管理分配审批、租约、GPU 容量和费用。Evaluation 在明确授权的范围执行评测 Job，不接管用户环境的生命周期。应用部署所用身份与这些运行时身份分开配置。

GPU 需要已有设备插件或 KubeVirt mediated device 配置。目录声明的模式和实际资源名称必须匹配；容器时间片申请一个共享份额，不能视为整卡。VM vGPU 必须使用已配置规格。无匹配设备、容量信息过期或设备释放未确认时，不自动降级或归还可分配容量。

## GPU 设备插件与 vGPU

`80-install-addons.yml` 的 `gpu_device_plugin` 角色默认关闭，启用时必须显式给出互不重叠的节点集合。独占节点暴露 `nvidia.com/gpu`；共享节点使用 NVIDIA time-slicing，并在 `renameByDefault` 下暴露 `nvidia.com/gpu.shared`。角色使用 `runtimeClassName: nvidia`、只读 `/dev` 与 PCI sysfs，并把 termination log 写入 Pod 可写的 `emptyDir`，因此不会因 `/dev/termination-log` 的只读 hostPath 失败。CDI 模式同时把节点驱动根只读挂载到 `/driver-root`，并把 `/var/run/cdi` 作为可写 hostPath；宿主机直接安装驱动时设置 `gpu_device_plugin_driver_root=/`，driver-container 布局使用 `/run/nvidia/driver`。不要把同一节点放进两个集合。

只对目标节点运行角色时使用现有 playbook 的 tag：

```sh
ansible-playbook -i deploy/ansible/inventories/v1/hosts.yml \
  deploy/ansible/playbooks/80-install-addons.yml \
  --tags gpu-device-plugin \
  -e gpu_device_plugin_enabled=true \
  -e 'gpu_device_plugin_exclusive_nodes=["<exclusive-node>"]' \
  -e 'gpu_device_plugin_shared_nodes=["<shared-node>"]' \
  -e gpu_device_plugin_shared_replicas=10
```

KubeVirt mediated device 只在主机实际创建对应 `mdev_supported_types` 后启用。`kubevirt-mdev` tag 只执行 CR 配置校验和 patch，不重装 KubeVirt；`kubevirt_mediated_devices_configuration` 的每项必须同时有 `nodeSelector` 和 `mediatedDeviceTypes`，并与全局 `kubevirt_mediated_device_types`、`permittedHostDevices.mediatedDevices` 成套配置。V100DX 配置示例使用 `nvidia-195` 至 `nvidia-199`，资源名分别为 `nvidia.com/grid-v100dx-2q` 至 `nvidia.com/grid-v100dx-32q`，节点选择器必须替换为实时节点标签：

```sh
ansible-playbook -i deploy/ansible/inventories/v1/hosts.yml \
  deploy/ansible/playbooks/70-install-kubevirt.yml \
  --tags kubevirt-mdev \
  -e @deploy/ansible/inventories/v1/gpu-mdev-private.yml
```

已有集群的 GPU seed 如果仍使用旧的 `nvidia-cuda-primary-v1` allocation binding，应先在管理员 GPU 目录界面新增正确 `nvidia.com/gpu` revision，再同步部署输入；不要直接改数据库或清空 seed。新安装可直接使用当前锁定输入。

## FastAPI-DLS（可选）

FastAPI-DLS 角色默认关闭。启用时部署固定摘要的 2.x 镜像，后端只绑定 Pod loopback，TLS 由同 Pod 的非 root nginx sidecar 终止，Service 为 ClusterIP 的 443 端口；NetworkPolicy 只允许带有 `labweaver.io/managed=true`、`labweaver.io/environment=true`、`app.kubernetes.io/name=labweaver-vm-runtime` 的环境 Namespace 中、同时带有 `app=runtime` 与 `labweaver.io/gpu-mode=vm_vgpu` 的 VM Pod 访问 8443。没有公网 Ingress。DLS 的 SQLite 数据库与签名根证书持久化在同一 PVC 的 `/app/database` 和 `/app/cert`；`fastapi-dls-tls` Secret 的 `ca.crt` 是 nginx TLS 信任材料，不能当作 DLS 签名 root CA。

使用私有变量启用角色。角色首次部署时在 backend loopback 生成 client token，写入
`fastapi-dls-client-token` Secret 的 `client-token` 键，并把持久化的 DLS signing root
写入 `fastapi-dls-signing-root` Secret 的 `ca.crt` 键。已有 Secret 会保留，token 内容不写入变量文件、镜像或日志：

`fastapi_dls_token_expire_days` 控制短期认证会话 token；FastAPI-DLS 的 client `.tok`
由服务端按其长期有效期生成，角色不会在每次重跑时轮换已有 Secret。

```sh
ansible-playbook -i deploy/ansible/inventories/v1/hosts.yml \
  deploy/ansible/playbooks/80-install-addons.yml \
  --tags fastapi-dls \
  -e fastapi_dls_enabled=true
```

外部 NVIDIA license server 仍可用，把 `fastapi_dls_client_license_url` 设为审核过的 HTTPS 主机地址，并保持 FastAPI-DLS 角色关闭；外部模式下由管理员提供对应 token Secret 引用。DLS 内部地址由 `fastapi_dls_url` 和 `fastapi_dls_service_port` 生成，当前服务端口固定为 443。管理员可在 backend 容器 loopback 上读取 client token 或 DLS signing root；guest 的 `gridd-unlock-patcher` 使用 DLS signing root，不能使用 nginx TLS CA：

```sh
kubectl -n labweaver-gpu-license exec deploy/fastapi-dls -c fastapi-dls -- \
  curl -fsS http://127.0.0.1:8080/-/config/root-certificate
kubectl -n labweaver-gpu-license exec deploy/fastapi-dls -c fastapi-dls -- \
  curl -fsS http://127.0.0.1:8080/-/client-token
```

上述管理端点只对管理员的 `kubectl exec` loopback 可见；guest 通过 TLS proxy 只能访问 `/auth/v1/` 和 `/leasing/v1/`。580.x guest 驱动还需要按 guest 镜像维护流程应用 `gridd-unlock-patcher`，host 驱动不在该角色范围内。

## 主机防火墙

节点主机防火墙由 **Cilium Host Firewall** 承担，不再使用 firewalld。`cluster_network` 角色在 Cilium Helm values 中启用 `hostFirewall.enabled: true`，并应用 `CiliumClusterwideNetworkPolicy`（`host-firewall-policy.yml.j2`）作为节点防火墙策略：集群节点间与 health 流量整体放行，外部放行 SSH（22）、公网入口（80/443）、WireGuard 管理网（51820/udp）与 ICMP echo。策略在 Cilium 安装后立即应用，`hostFirewall` 启用但尚未有策略选中节点时保持默认放行，因此不会在应用策略前中断管理通道。

节点同时使用 `bpf.masquerade: true` 与 `bpf.hostLegacyRouting: false`，数据面完全走 eBPF，不再依赖 iptables-nft 链。这样 firewalld/iptables 的 flush 不会破坏 pod 与 Gateway 流量，也不需要启动排序或 Cilium 自动恢复逻辑。节点上不安装、不启用 firewalld；`rocky_common` 不再需要“禁用 firewalld”。

`router_services`（边缘路由器 WAN/LAN NAT 的旧 profile）仍使用 firewalld，仅用于 dev/edge-router 拓扑；当前集群 inventory 通过 `labweaver_skip_router_services: true` 跳过。如需完全去除 firewalld，应单独迁移该 profile。

## 公网域名与证书

`82-public-ingress.yml` 用 cert-manager 为公网域名签发并挂载 TLS 证书，默认 `selfsigned`（本地自签名，可离线使用），通过 `public_ingress_tls_mode: acme` 切换为 Let's Encrypt。通配符证书必须走 DNS-01，本仓使用 Cloudflare solver；`acme` 依赖控制器能直连 `acme-v02.api.letsencrypt.org` 与 `api.cloudflare.com`。

发布的主机名由 `public_ingress_routes` 声明：根域与 `portal.` 指向 `labweaver-system/web:8080`，`keycloak.` 指向 `keycloak-system/labweaver-keycloak-http:8080`，`harbor.` 指向 `harbor/harbor:80`。角色创建独立的 `labweaver-public` Gateway（Cilium，专用 VIP）。节点自身公网地址无法直接命中 Gateway VIP：Cilium 的 tc-ingress eBPF 在 netfilter DNAT 之前解析 VIP，因此 firewalld/iptables 的 forward-port 不可用；改为 `hostNetwork` HAProxy DaemonSet 绑定节点 80/443 并转发到 VIP，走 socket 路径由 Cilium socket LB 解析。不改动既有内部 Gateway。

Cloudflare API Token 只从 root-only locator（`/var/lib/labweaver/.private/tls/cloudflare.env`）读取并直接写入 `cert-manager` 命名空间的 Secret，使用 `no_log`，不进入 Git、日志或报告。证书与 Gateway 就绪后角色会 readback `Certificate Ready` 与 Gateway VIP 才通过。

## 维护

升级先校验配置、渲染模板并检查数据库迁移，再应用目标应用版本。v3 迁移支持空数据库初始化；服务启动不会清空旧数据。不兼容旧数据库时应停止并单独安排数据处理，不能通过自动删除或隐藏迁移继续启动。

回滚使用明确的应用版本与可用的数据库恢复方案。应用回滚不等于数据库回滚；停止异常的环境和待核实用量不能自动当作已释放或免费。共享基础组件的数据清理须另行确认具体对象、归属和恢复方式。

本地测试与模板验证不替代对应真实环境的验证。最终启动命令、配置样例和模板验证结果随部署入口更新；未验证的 KubeVirt、GPU 与集群条件在 PR 中说明。
