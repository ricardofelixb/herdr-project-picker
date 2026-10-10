//! Decides whether a `pane.created` event is a user-made split.
//!
//! Herdr 0.9.3 gives `pane.created` no origin: new tabs, workspaces, layouts,
//! plugin panes and splits all emit it with the same payload. The rules below
//! are heuristics over what the payload and `pane list` do expose.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    pub tab_id: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Open,
    Skip(Skip),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Skip {
    /// The pane is unfocused, as panes created by `herdr pane split` are by default.
    Unfocused,
    /// Plugin panes carry their manifest title as a manual label.
    Labeled,
    /// An agent already owns the pane.
    Agent,
    /// The pane is the first pane of its tab, or its id is not understood.
    InitialPane,
}

/// Returns the created pane when `event_json` is a `pane_created` envelope.
pub fn created_pane(_event_json: &str) -> Option<PaneInfo> {
    todo!()
}

/// `tab_panes` is every pane of the workspace, as printed by `pane list`.
pub fn decide(_created: &PaneInfo, _tab_panes: &[PaneInfo]) -> Decision {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str, tab: &str) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            tab_id: tab.into(),
            focused: true,
            label: None,
            agent: None,
        }
    }

    fn event(kind: &str, p: &PaneInfo) -> String {
        format!(
            r#"{{"event":"{kind}","data":{{"type":"{kind}","pane":{{"pane_id":"{}","tab_id":"{}","workspace_id":"w1","focused":{},"agent_status":"unknown"}}}}}}"#,
            p.pane_id, p.tab_id, p.focused
        )
    }

    #[test]
    fn parses_only_pane_created_events() {
        let p = pane("w1:p2", "w1:t1");
        let parsed = created_pane(&event("pane_created", &p)).unwrap();
        assert_eq!(parsed.pane_id, "w1:p2");
        assert!(parsed.focused);
        assert!(created_pane(&event("pane_focused", &p)).is_none());
        assert!(created_pane(r#"{"event":"tab_created","data":{"type":"tab_created"}}"#).is_none());
        assert!(created_pane("not json").is_none());
    }

    #[test]
    fn focused_pane_beside_an_older_pane_is_a_split() {
        let root = pane("w1:p1", "w1:t1");
        let split = pane("w1:p2", "w1:t1");
        assert_eq!(decide(&split, &[root, split.clone()]), Decision::Open);
    }

    #[test]
    fn only_pane_of_a_tab_is_a_new_tab() {
        let other_tab = pane("w1:p1", "w1:t1");
        let new_tab = pane("w1:p2", "w1:t2");
        assert_eq!(
            decide(&new_tab, &[other_tab, new_tab.clone()]),
            Decision::Skip(Skip::InitialPane)
        );
    }

    #[test]
    fn root_pane_stays_initial_when_split_before_the_hook_runs() {
        let root = pane("w1:p2", "w1:t2");
        let raced_split = pane("w1:p3", "w1:t2");
        assert_eq!(
            decide(&root, &[root.clone(), raced_split]),
            Decision::Skip(Skip::InitialPane)
        );
    }

    #[test]
    fn pane_numbers_compare_numerically() {
        let nine = pane("w1:p9", "w1:t1");
        let ten = pane("w1:p10", "w1:t1");
        let all = [ten.clone(), nine.clone()];
        assert_eq!(decide(&ten, &all), Decision::Open);
        assert_eq!(decide(&nine, &all), Decision::Skip(Skip::InitialPane));
    }

    #[test]
    fn unparsable_ids_are_never_triggered() {
        let root = pane("w1:p1", "w1:t1");
        let odd = pane("w1:x", "w1:t1");
        assert_eq!(
            decide(&odd, &[root, odd.clone()]),
            Decision::Skip(Skip::InitialPane)
        );
    }

    #[test]
    fn unfocused_labeled_and_agent_panes_are_skipped() {
        let root = pane("w1:p1", "w1:t1");
        let mut p = pane("w1:p2", "w1:t1");
        p.focused = false;
        assert_eq!(decide(&p, &[root.clone(), p.clone()]), Decision::Skip(Skip::Unfocused));
        let mut p = pane("w1:p2", "w1:t1");
        p.label = Some("Picker".into());
        assert_eq!(decide(&p, &[root.clone(), p.clone()]), Decision::Skip(Skip::Labeled));
        let mut p = pane("w1:p2", "w1:t1");
        p.agent = Some("claude".into());
        assert_eq!(decide(&p, &[root, p.clone()]), Decision::Skip(Skip::Agent));
    }
}
