/**
 * Helpers for explicit model-input material path selection (#216).
 * Never defaults to the whole package; callers must pass a non-empty selection.
 */

/** Stable, de-duplicated package-relative paths for Create*RunRequest.selectedPaths. */
export function buildSelectedPaths(paths: readonly string[]): string[] {
  const seen = new Set<string>()
  const out: string[] = []
  for (const raw of paths) {
    const path = raw.trim()
    if (!path || seen.has(path)) continue
    seen.add(path)
    out.push(path)
  }
  return out
}

/** True when the user has chosen at least one exact path (client-side gate). */
export function hasRequiredSelectedPaths(paths: readonly string[] | null | undefined): boolean {
  return buildSelectedPaths(paths ?? []).length > 0
}

/** Operator-facing next-step copy for model-input authorization diagnostics. */
export function modelInputSelectionAction(code: string): string | null {
  switch (code) {
    case 'LW_MODEL_INPUT_SELECTION_REQUIRED':
      return '请在材料包中显式勾选至少一份要送入模型的文件；服务端不会默认选择整包。'
    case 'LW_MODEL_INPUT_AUTHORIZATION_DENIED':
      return '任务已受理后的实时授权复核未通过（会话失效、成员权限或对象版本变化等）。请查看任务失败原因；需要时重新登录并新建一次显式选择的生成任务。'
    case 'LW_MODEL_INPUT_AUTHORIZATION_UNAVAILABLE':
      return '模型输入授权复核连接尚未配置完成，当前不能启动新的模型发送。请等待平台部署接线（#203）完成后再试；上传材料成功不等于模型已成功。'
    case 'LW_ACCESS_DENIED':
      return '当前账号没有该项目的模型输入创建权限，请确认项目成员资格后重试。'
    case 'REVISION_CONFLICT':
      return '材料包或策略版本已变化，请刷新页面、重新勾选文件后再启动。'
    default:
      return null
  }
}

/** Short Chinese message override when the server diagnostic is known. */
export function modelInputSelectionMessage(code: string, fallback: string): string {
  switch (code) {
    case 'LW_MODEL_INPUT_SELECTION_REQUIRED':
      return '必须显式选择材料包内的文件路径；缺少、为空、重复或不属于该包版本的路径均被拒绝。'
    case 'LW_MODEL_INPUT_AUTHORIZATION_DENIED':
      return '模型输入授权复核被拒绝，本次不会继续向模型发送未授权内容。'
    case 'LW_MODEL_INPUT_AUTHORIZATION_UNAVAILABLE':
      return '模型输入授权复核不可用，任务无法完成模型发送。'
    default:
      return fallback
  }
}
