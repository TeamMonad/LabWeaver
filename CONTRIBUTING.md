# Contributing

## Issue 与分支

每个变更必须关联一个 Issue，并明确 Goal、Non-goals、Owner、Reviewer、依赖、失败行为和验收证据。分支使用：

```text
feature/<issue>-<slug>
fix/<issue>-<slug>
test/<issue>-<slug>
docs/<issue>-<slug>
```

## 提交前验证

```sh
cargo xtask check
```

创建或更新 Pull Request 前，必须先执行 `git fetch origin develop` 和 `git rebase origin/develop`，确认分支基于最新 `develop` 且不包含未说明的 merge commit。

不存在的测试或缺失的依赖必须阻断，不得以空成功脚本、旧报告或 Mock 结果替代。

## 评审

- 创建或更新 PR 后，作者必须使用 `gh pr edit <pr-number> --add-reviewer <github-login>` 显式请求主 Reviewer；不得只等待 CODEOWNERS 自动路由。
- PR 描述必须列出主 Reviewer、验收人、`risk:low`/`risk:medium`/`risk:high` 和是否可 auto-merge。
- 目标分支 `develop` 的常规路径由一名匹配 CODEOWNERS 的人类批准即可满足 GitHub 审批门禁；目标分支 `main` 至少需要两名 Reviewer。
- Contract、Schema、Migration、权限或安全、评分、Agent Tool、CRD 必须由 A 与 B 两名人类审批；涉及测试、部署或运行证据时，D 必须完成 Verify。
- 只有目标为 `develop`、非 Draft、关联 Issue 标记 `risk:low`、无高风险路径、已有匹配 CODEOWNERS 批准、必需 CI 全绿且 Review Thread 全部解决时，作者才可执行 `gh pr merge --auto --squash`。
- 高风险 PR 和所有指向 `main` 的 PR 禁止 auto-merge；`main` 与 Release PR 由人工 squash。作者不得自行批准并合并核心模块。
