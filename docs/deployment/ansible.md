# 应用部署与基础组件

LabWeaver 使用 Ansible 调用 Helm 和 Kubernetes 模块。应用安装面向已有 Kubernetes；基础组件 bootstrap 是独立、显式的维护操作，应用升级不能隐式重建数据库、OIDC Realm、对象存储或消息流。

部署内容包含 Control、Access、Environment、Agent、Evaluation、Resource 六个业务服务，以及 Web 和访问网关。Worker 是所属服务的执行角色。启用的组件由配置和实际构建清单决定，不按固定镜像数量判断部署是否有效。

## 配置

Inventory 声明目标集群、命名空间、存储类、镜像引用和依赖端点。数据库、NATS、Keycloak、对象存储和 Registry 可以使用已有实例；部署前检查所需能力与权限，缺失配置直接报错。配置样例使用项目相对路径或 Secret 挂载位置，不包含个人机器路径和私有拓扑。

内部 HTTP 使用服务端 TLS 与 Keycloak client credentials。每个服务使用自己的客户端身份，令牌包含目标 audience 和受限客户端角色；网络策略不能替代令牌验证。用户的项目与课程权限仍由 Access 判断。Secret、私钥和客户端口令以文件注入，不进入镜像构建参数、普通配置、日志或 Git。

## 运行边界

Environment 管理环境 Namespace、Quota、PVC 和运行对象；Resource 管理分配审批、租约、GPU 容量和费用。Evaluation 在明确授权的范围执行评测 Job，不接管用户环境的生命周期。应用部署所用身份与这些运行时身份分开配置。

GPU 需要已有设备插件或 KubeVirt mediated device 配置。目录声明的模式和实际资源名称必须匹配；容器时间片申请一个共享份额，不能视为整卡。VM vGPU 必须使用已配置规格。无匹配设备、容量信息过期或设备释放未确认时，不自动降级或归还可分配容量。

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
