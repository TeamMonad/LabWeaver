# 集群内部配置说明

本文面向 LabWeaver 开发、测试与运维成员，说明当前 Kubernetes 基础设施的逻辑拓扑、受控配置和证据边界。它不替代 private inventory、Vault 或运行时验收报告；真实主机地址、SSH 跳板、kubeconfig、证书、磁盘 WWN、容量、VIP、Vault 内容和任何凭据均不得写入本文件或 Git。

## 1. 逻辑拓扑与责任边界

| 逻辑角色 | 数量 | 职责 | 不承担的职责 |
| --- | ---: | --- | --- |
| edge router | 1 | 已有接口上的转发、NAT、防火墙策略和内部 DNS | Kubernetes 工作负载、数据库、业务服务或公网业务暴露 |
| control plane | 1 | Kubernetes API、etcd、调度和集群控制组件 | 普通 LabWeaver 业务工作负载 |
| Worker | 至少 2 | 容器工作负载、Runner/Job、具备 KVM 的 KubeVirt VM | 路由、NAT、手工常驻业务进程 |
| NFS server | 1 | Kubernetes RWX 卷的 NFSv4 导出 | LabWeaver 业务服务与数据库 |

PVE、NetworkManager 连接、接口/IP、WAN/LAN 地址分配、Tailnet 地址分配和公网 DNAT 都不属于 Ansible 管理范围。Ansible 只验证并消费这些已存在的网络前提；外部访问必须经过获批的 Headscale/Tailscale 与内部 Gateway 路径。

## 2. 网络、入口与策略

- 路由器位于 LabWeaver 内网边界，可使用 rocky 并采用密钥登录，可以借助路由器作为跳板使用 rocky + 路由器密钥登录到集群中其他机器上。使用 Cloudflare One 时路由器的访问 IP 为 10.20.0.1，使用 Tailscale 访问时路由器的访问 IP 为 100.63.0.202。
- Kubernetes 集群内部节点 IP ：
    - control plane: 10.20.0.21
    - Worker-01: 10.20.0.22
    - Worker-02: 10.20.0.23
    - NFS server: 10.20.0.24
- Kubernetes Pod CIDR 为 `10.244.0.0/16`，Service CIDR 为 `10.96.0.0/12`。
- Cilium 启用 kube-proxy replacement、Hubble 和 Gateway API；Gateway API 使用 Standard CRD channel `v1.6.0`。
- MetalLB 使用经 private inventory 声明的地址池与 L2 Advertisement。固定 VIP 通过 Gateway 的 `metallb.io/loadBalancerIPs` 基础设施注解分配，不使用 Cilium LB-IPAM 专用的 `spec.addresses`。
- 当前内部 Gateway 为 `labweaver-demo/public-gateway`，仅监听 HTTP；HTTPRoute、后端 Service 与 Host 请求必须在 verify 的隔离命名空间中真实验证。
- 不配置外网 DNAT、开放公网端口或绕过 Access/Headscale 的路径。网络可达性不等于授权；未来业务端点仍须由 AccessGrant 与 Access Service 共同约束。

## 3. 存储与虚拟化

| 能力 | 受控配置 | 运行约束 |
| --- | --- | --- |
| Local Path RWO | `local-path` StorageClass；节点挂载点为 `/var/lib/k8s-local-storage` | SELinux 标签固定为 `container_file_t`；格式化默认禁止 |
| NFS RWX | `nfs-rwx` StorageClass；NFSv4 导出路径为 `/srv/nfs/k8s` | NFS 服务地址、导出权限和网络前提只存在于 private inventory |
| 受控格式化 | 显式 destructive confirmation、精确 WWN 和精确容量 | 拒绝 root、已挂载、含文件系统、分区/堆叠、holder、RAID/LVM/multipath 或身份不匹配的设备 |
| KubeVirt | 仅标记为 `labweaver.io/kubevirt=true` 且通过 `/dev/kvm`、nested virtualization、SELinux 和容量预检的 Worker | `useEmulation: false`；VM 只能使用硬件 KVM，不得用软件模拟作为通过证据 |
| CDI | scratch StorageClass 为 `local-path` | CDI CR 是 scratch 配置的唯一 owner |

## 4. 固定组件基线

| 组件 | 固定版本 |
| --- | --- |
| Kubernetes / CRI-O | `v1.35.6` / `1.35.5` |
| Cilium / MetalLB | `1.19.5` / `0.16.1` |
| Local Path / NFS CSI | `v0.0.36` / `4.13.4` |
| cert-manager | `v1.21.0` |
| KubeVirt / CDI | `v1.8.4` / `v1.65.0` |
| Kyverno | `v1.18.2` |
| etcd 工具 | `3.6.6` |

完整版本锁和测试镜像 digest 以 [`deploy/versions.lock.yml`](../../deploy/versions.lock.yml) 为准。变更组件版本、Chart 或镜像 digest 时，必须更新锁文件、部署报告和相应验收证据，不得使用 `latest`。

## 5. 命名空间、部署入口与证据

基础命名空间为 `labweaver-system`、`labweaver-data`、`labweaver-build`、`labweaver-evaluation` 和 `labweaver-demo`；demo namespace 有受控 ResourceQuota。平台数据服务和六个 LabWeaver 业务服务尚不由这套基础设施 playbook 部署。

当前已实现的 Ansible 控制器入口是 `python tools/ansible.py <preflight|deploy|verify|backup>`；目标中的 `cargo xtask deploy --env <environment>` 仍须以实际实现与验证为准。Ansible 是部署执行层。private deployment input 使用受忽略的 inventory、`group_vars/all/main.yml`、加密的 `group_vars/all/vault.yml` 和 Vault 密码文件。缺任一输入、存在 `REPLACE_*` 占位符或 preflight 失败时必须中止，不得以旧报告或已存在资源伪装成功。

当前可提交的证据包括 Ansible lint、syntax、虚构加密 Vault fixture、存储安全负向 fixture，以及 VM-01a 的受限 E3 基础设施证据。后者证明了限定范围内的 RWO/RWX、硬件 KVM VM 生命周期、内部 Gateway 请求和清理；它不证明 Ansible 首次部署、第二次幂等 replay、Access/Headscale、业务服务或 E4 发布条件。完整状态和 blocker 以 [`docs/status/implementation-status.md`](../status/implementation-status.md) 与 [`docs/testing/evidence/vm-01a-e3-20260713.md`](../testing/evidence/vm-01a-e3-20260713.md) 为准。

## 6. 交接与排障规则

1. 成员先读取 private inventory 的受控副本和当前 component lock，确认目标 cluster UID、变更窗口、rollback owner 与备份位置；这些值不复制进 Git 文档。
2. 先运行 preflight，再运行 deploy、backup 和 verify；verify 失败必须保留 machine-readable diagnostic 与 cleanup 状态，不能将部分通过升级为验收结论。
3. etcd snapshot 只保存在受控备份位置，报告至少绑定 snapshot hash、revision、size、权限、版本和生成身份。
4. 任何需要开放公网端口、扩大 RBAC、放宽 NetworkPolicy、使用 HostPath/privileged/hostNetwork、重格式化磁盘或读取真实 Secret 的操作，必须停止并取得相应的人类安全/运维批准。
