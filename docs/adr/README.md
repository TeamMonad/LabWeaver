# 架构决定

当前产品与重构方向见 [ADR 0015](0015-v3-project-work-resource-platform.md)。历史 ADR 解释旧实现，不能覆盖当前用户选择。

仍适用的领域行为包括生命周期代次与回收、Outbox 防重、冻结提交、ConsoleCapability 与确定性评测。旧课程时间限制、固定发布证明、禁用 Resource/Evaluation、仅 Mock GPU 和未来兼容层不再适用。

新增 ADR 只记录真实跨域或高风险选择及其代价；任务进度和测试结果放 Issue/PR，不维护第二套状态库。
