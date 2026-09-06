use super::*;
use crate::methods::Method;

#[test]
fn wire_names_round_trip() {
    for permission in Permission::ALL {
        let encoded = serde_json::to_value(permission).expect("permission serializes");
        assert_eq!(encoded, serde_json::json!(permission.as_str()));
        let decoded: Permission = serde_json::from_value(encoded).expect("permission decodes");
        assert_eq!(decoded, *permission);
    }
}

#[test]
fn every_permission_has_prompt_copy() {
    for permission in Permission::ALL {
        assert!(
            !permission.description().is_empty(),
            "{permission} has no prompt description"
        );
    }
}

#[test]
fn a_set_reports_exactly_what_was_granted() {
    let granted: PermissionSet = [Permission::WorkspaceRead, Permission::ProcessExecute]
        .into_iter()
        .collect();
    assert!(granted.contains(Permission::WorkspaceRead));
    assert!(!granted.contains(Permission::WorkspaceWrite));
    assert_eq!(granted.len(), 2);
}

#[test]
fn covers_detects_a_manifest_asking_for_more_than_was_granted() {
    let granted: PermissionSet = [Permission::WorkspaceRead].into_iter().collect();
    let unchanged: PermissionSet = [Permission::WorkspaceRead].into_iter().collect();
    let widened: PermissionSet = [Permission::WorkspaceRead, Permission::GitMutate]
        .into_iter()
        .collect();

    assert!(granted.covers(&unchanged));
    assert!(!granted.covers(&widened));
}

#[test]
fn a_method_outside_the_granted_set_is_denied() {
    let granted: PermissionSet = [Permission::WorkspaceRead].into_iter().collect();

    let allowed = Method::WorkspaceGetContext
        .permission()
        .is_none_or(|permission| granted.contains(permission));
    assert!(allowed);

    let denied = Method::ExecutionRun
        .permission()
        .is_none_or(|permission| granted.contains(permission));
    assert!(!denied);
}

#[test]
fn the_handshake_needs_no_grant() {
    let granted = PermissionSet::new();
    assert!(granted.is_empty());
    assert!(Method::ExtensionInitialize.permission().is_none());
}
