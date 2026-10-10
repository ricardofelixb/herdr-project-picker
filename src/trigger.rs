//! Decides whether a `pane.created` event is a user-made split.
//!
//! Herdr 0.9.3 gives `pane.created` no origin: new tabs, workspaces, layouts,
//! plugin panes and splits all emit it with the same payload. The rules below
//! are heuristics over what the payload and `pane list` do expose.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    pub workspace_id: String,
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
pub fn created_pane(event_json: &str) -> Option<PaneInfo> {
    #[derive(Deserialize)]
    struct Envelope {
        event: String,
        data: Data,
    }
    #[derive(Deserialize)]
    struct Data {
        pane: Option<PaneInfo>,
    }
    let envelope: Envelope = serde_json::from_str(event_json).ok()?;
    if envelope.event != "pane_created" {
        return None;
    }
    envelope.data.pane
}

/// Reads the panes out of `herdr pane list` output.
pub fn listed_panes(_output: &str) -> Option<Vec<PaneInfo>> {
    todo!()
}

/// `tab_panes` is every pane of the workspace, as printed by `pane list`.
pub fn decide(created: &PaneInfo, tab_panes: &[PaneInfo]) -> Decision {
    if created.label.is_some() {
        return Decision::Skip(Skip::Labeled);
    }
    if created.agent.is_some() {
        return Decision::Skip(Skip::Agent);
    }
    if !created.focused {
        return Decision::Skip(Skip::Unfocused);
    }
    // A tab's first pane has the lowest number in it, even when it is split
    // before this hook gets to list the panes.
    let Some(number) = pane_number(&created.pane_id) else {
        return Decision::Skip(Skip::InitialPane);
    };
    let has_older_sibling = tab_panes
        .iter()
        .filter(|pane| pane.tab_id == created.tab_id)
        .filter_map(|pane| pane_number(&pane.pane_id))
        .any(|other| other < number);
    if has_older_sibling {
        Decision::Open
    } else {
        Decision::Skip(Skip::InitialPane)
    }
}

/// `w1:p12` -> 12.
fn pane_number(pane_id: &str) -> Option<u64> {
    pane_id.rsplit(':').next()?.strip_prefix('p')?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str, tab: &str) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            workspace_id: "w1".into(),
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
    fn pane_list_output_is_read_and_rejects_anything_else() {
        let output = r#"{"id":"cli","result":{"type":"pane_list","panes":[{"pane_id":"w1:p1","workspace_id":"w1","tab_id":"w1:t1","focused":false},{"pane_id":"w1:p2","workspace_id":"w1","tab_id":"w1:t1","focused":true,"label":"x"}]}}"#;
        let panes = listed_panes(output).unwrap();
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[1].label.as_deref(), Some("x"));
        assert!(listed_panes(r#"{"result":{}}"#).is_none());
        assert!(listed_panes("oops").is_none());
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
