//! Publication configuration reads must retain object authority without becoming mutations.
use contracts::http::{Method, MutationContract, OperationScopeKind, Security};
use contracts::object_authorization::{ObjectAction, project_operation_action};

#[test]
fn template_options_use_publication_authority_and_read_only_transport()
-> Result<(), Box<dyn std::error::Error>> {
    let options = contracts::operation_contract("getProjectManualTemplateOptions")
        .ok_or("missing options operation")?;
    let publish = contracts::operation_contract("createEnvironmentTemplateRelease")
        .ok_or("missing publication operation")?;
    assert_eq!(options.method, Method::Get);
    assert_eq!(options.mutation, MutationContract::None);
    assert_eq!(options.scope, OperationScopeKind::Project);
    assert_eq!(options.security, Security::Oidc);
    assert_eq!(options.permission, publish.permission);
    assert_eq!(
        project_operation_action(options.operation_id),
        Some(ObjectAction::Edit)
    );
    assert_eq!(
        project_operation_action(options.operation_id),
        project_operation_action(publish.operation_id)
    );
    let internal = contracts::operation_contract("getManualTemplateRuntimeOptions")
        .ok_or("missing internal read")?;
    assert_eq!(internal.security, Security::ServiceJwt);
    assert_eq!(internal.permission, "environment.work.read");
    Ok(())
}
