//! Current runtime binding read through the existing Access delegation boundary.
use super::{
    ResourceApiError, ResourceApiState, ResourceCallerPrincipal, authorize, scoped_or_admin,
};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use contracts::ResourceRequestId;

pub(super) async fn get_binding(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Path(id): Path<ResourceRequestId>,
) -> Result<Response, ResourceApiError> {
    authorize(&principal);
    let request = state.store.load(id).await?;
    scoped_or_admin(&principal, request.requester_id)?;
    let binding =
        crate::runtime_authorization::current_authorization(&state.store.pool(), &request)
            .await
            .map_err(crate::store::ResourceStoreError::from)?;
    let mut response = Json(binding).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(super) fn status(error: &crate::runtime_authorization::RuntimeBindingError) -> StatusCode {
    use crate::runtime_authorization::RuntimeBindingError;
    use contracts::runtime_authorization::RuntimeAuthorizationError;
    match error {
        RuntimeBindingError::Unavailable
        | RuntimeBindingError::Authorization(RuntimeAuthorizationError::PolicyNotVerified) => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        RuntimeBindingError::ScopeDenied
        | RuntimeBindingError::Authorization(
            RuntimeAuthorizationError::TeacherApprovalRequired
            | RuntimeAuthorizationError::OutsidePlatformRange,
        ) => StatusCode::FORBIDDEN,
        RuntimeBindingError::Authorization(_) => StatusCode::CONFLICT,
    }
}

pub(super) fn detail(error: &crate::runtime_authorization::RuntimeBindingError) -> &'static str {
    use crate::runtime_authorization::RuntimeBindingError;
    use contracts::runtime_authorization::RuntimeAuthorizationError;
    match error {
        RuntimeBindingError::Unavailable => "当前授权信息不可用，请稍后重试或联系管理员。",
        RuntimeBindingError::ScopeDenied => {
            "当前账号没有该项目或课程所需的权限，请联系成员管理者。"
        }
        RuntimeBindingError::Authorization(reason) => match reason {
            RuntimeAuthorizationError::PolicyNotVerified => {
                "平台运行配置尚未完成验证，请等待配置验证或联系管理员。停止和恢复入口仍可使用。"
            }
            RuntimeAuthorizationError::PolicyChanged => {
                "平台运行策略已变更，请按当前策略重新申请运行授权。"
            }
            RuntimeAuthorizationError::ConfigurationMismatch => {
                "运行授权与当前请求或环境配置不一致，请重新审核当前配置。"
            }
            RuntimeAuthorizationError::TeacherApprovalRequired => {
                "该实验缺少对应课程教师的完整批准，请联系课程教师审核。"
            }
            RuntimeAuthorizationError::OutsidePlatformRange => {
                "申请的运行权限超出平台允许范围，请调整环境配置或联系管理员。"
            }
            RuntimeAuthorizationError::ApprovalRequired => {
                "该环境缺少当前有效的运行授权，请重新申请审批。"
            }
            RuntimeAuthorizationError::TargetNotEnvironment => {
                "该请求不属于环境，不能使用环境运行授权。"
            }
        },
    }
}
