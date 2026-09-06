use serde_json::json;

use super::*;

#[test]
fn wire_names_round_trip() {
    for method in Method::ALL {
        let encoded = serde_json::to_value(method).expect("method serializes");
        assert_eq!(encoded, json!(method.as_str()));
        let decoded: Method = serde_json::from_value(encoded).expect("method decodes");
        assert_eq!(decoded, *method);
    }
}

#[test]
fn every_method_except_the_handshake_is_gated() {
    for method in Method::ALL {
        match method {
            Method::ExtensionInitialize => {
                assert!(method.permission().is_none());
                assert!(method.capability().is_none());
            }
            other => {
                assert!(
                    other.permission().is_some(),
                    "{other} has no gating permission"
                );
                assert!(
                    other.capability().is_some(),
                    "{other} has no gating capability"
                );
            }
        }
    }
}

#[test]
fn execution_target_distinguishes_local_from_a_named_session() {
    let local = serde_json::to_value(ExecutionTarget::Local).expect("target serializes");
    assert_eq!(local, json!({ "kind": "local" }));

    let remote = serde_json::to_value(ExecutionTarget::Ssh {
        session_id: "ssh-123".to_owned(),
    })
    .expect("target serializes");
    assert_eq!(remote, json!({ "kind": "ssh", "session_id": "ssh-123" }));
}

#[test]
fn execution_run_params_have_no_shell_string_form() {
    let params: ExecutionRunParams = serde_json::from_value(json!({
        "target": { "kind": "local" },
        "cwd": "/home/user/project",
        "executable": "git",
        "args": ["status", "--porcelain=v2", "-z"]
    }))
    .expect("params decode");
    assert_eq!(params.args, ["status", "--porcelain=v2", "-z"]);

    let with_shell = serde_json::from_value::<ExecutionRunParams>(json!({
        "target": { "kind": "local" },
        "cwd": "/home/user/project",
        "executable": "git",
        "command": "git status | rm -rf /"
    }));
    assert!(with_shell.is_err(), "a shell string field must not decode");
}

#[test]
fn workspace_context_reports_the_documented_shape() {
    let context = WorkspaceContext {
        workspace_id: "abc".to_owned(),
        cwd: "/home/user/project".to_owned(),
        repository_root: Some("/home/user/project".to_owned()),
        active_pane_id: None,
        session_id: Some("ssh-123".to_owned()),
        execution_target: ExecutionTarget::Ssh {
            session_id: "ssh-123".to_owned(),
        },
    };
    let encoded = serde_json::to_value(&context).expect("context serializes");
    assert_eq!(encoded["workspace_id"], json!("abc"));
    assert_eq!(encoded["cwd"], json!("/home/user/project"));
    assert_eq!(
        encoded["execution_target"],
        json!({ "kind": "ssh", "session_id": "ssh-123" })
    );
    assert!(
        encoded.get("active_pane_id").is_none(),
        "absent fields are omitted rather than sent as null"
    );
}

#[test]
fn panel_state_nests_rows_and_their_actions() {
    let state = PanelViewState {
        panel_id: "git".to_owned(),
        status: PanelStatus::Ready,
        sections: vec![PanelSection {
            id: "git.changes".to_owned(),
            title: "Changes".to_owned(),
            badge: Some("4".to_owned()),
            items: vec![PanelItem {
                id: "src/main.rs".to_owned(),
                label: "src/main.rs".to_owned(),
                description: None,
                icon: Some("modified".to_owned()),
                badge: None,
                actions: vec![PanelItemAction {
                    id: "open_diff".to_owned(),
                    title: "Open diff".to_owned(),
                    icon: None,
                    destructive: false,
                }],
                children: Vec::new(),
                expanded: false,
            }],
        }],
    };
    let encoded = serde_json::to_value(&state).expect("state serializes");
    assert_eq!(encoded["status"], json!({ "state": "ready" }));
    assert_eq!(
        encoded["sections"][0]["items"][0]["actions"][0]["id"],
        json!("open_diff")
    );
    let decoded: PanelViewState = serde_json::from_value(encoded).expect("state decodes");
    assert_eq!(decoded, state);
}

#[test]
fn panel_status_carries_its_message_for_empty_and_error_states() {
    let empty = serde_json::to_value(PanelStatus::Empty {
        message: "No changes".to_owned(),
    })
    .expect("status serializes");
    assert_eq!(empty, json!({ "state": "empty", "message": "No changes" }));

    let failed = serde_json::to_value(PanelStatus::Error {
        message: "not a repository".to_owned(),
    })
    .expect("status serializes");
    assert_eq!(
        failed,
        json!({ "state": "error", "message": "not a repository" })
    );
}

#[test]
fn cancelled_dialogs_are_representable() {
    let cancelled: DialogInputResult =
        serde_json::from_value(json!({})).expect("an empty result decodes");
    assert_eq!(cancelled.value, None);

    let chosen: DialogSelectResult =
        serde_json::from_value(json!({ "selected_id": "main" })).expect("result decodes");
    assert_eq!(chosen.selected_id.as_deref(), Some("main"));
}
