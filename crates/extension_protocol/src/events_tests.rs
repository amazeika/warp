use serde_json::json;

use super::*;
use crate::methods::ExecutionTarget;

#[test]
fn wire_names_round_trip() {
    for event in EventKind::ALL {
        let encoded = serde_json::to_value(event).expect("event serializes");
        assert_eq!(encoded, json!(event.as_str()));
        let decoded: EventKind = serde_json::from_value(encoded).expect("event decodes");
        assert_eq!(decoded, *event);
    }
}

#[test]
fn panel_action_carries_the_origin_it_started_in() {
    let params = PanelActionParams {
        panel_id: "git".to_owned(),
        item_id: "src/main.rs".to_owned(),
        action_id: "open_diff".to_owned(),
        origin: ActionOrigin {
            workspace_id: "ws-1".to_owned(),
            session_id: Some("ssh-123".to_owned()),
            repository_root: Some("/home/user/project".to_owned()),
            execution_target: ExecutionTarget::Ssh {
                session_id: "ssh-123".to_owned(),
            },
        },
    };
    let encoded = serde_json::to_value(&params).expect("params serialize");
    assert_eq!(
        encoded["origin"]["execution_target"],
        json!({ "kind": "ssh", "session_id": "ssh-123" })
    );
    let decoded: PanelActionParams = serde_json::from_value(encoded).expect("params decode");
    assert_eq!(decoded, params);
}
