//! Maps a Herdr session snapshot onto the Moshi workspace-tree shape and
//! tracks observed status timestamps.

use std::collections::HashMap;

use crate::herdr::{AgentSession, HerdrAgent, HerdrPane, HerdrTab, Snapshot};
use crate::model::{TreeCapabilities, TreeChild, TreeGroup, TreePane, WorkspaceTree};

/// Tracks when a node first showed its current status, so the tree can expose
/// `statusChangedAt`. Times derived from an observed transition are exact to
/// the polling cadence; a first sighting (typically after a restart) is
/// flagged `approx`, meaning the status began at or before that time.
#[derive(Debug, Default)]
pub struct StatusTracker {
    seen: HashMap<String, SeenStatus>,
}

#[derive(Debug, Clone)]
struct SeenStatus {
    status: String,
    since: f64,
    approx: bool,
}

impl StatusTracker {
    /// Record `status` for node `id` at time `now`, returning `(since, approx)`.
    pub fn observe(&mut self, id: &str, status: Option<&str>, now: f64) -> Option<(f64, bool)> {
        let status = status?;
        match self.seen.get_mut(id) {
            Some(seen) if seen.status == status => Some((seen.since, seen.approx)),
            Some(seen) => {
                seen.status = status.to_string();
                seen.since = now;
                seen.approx = false;
                Some((now, false))
            }
            None => {
                self.seen.insert(
                    id.to_string(),
                    SeenStatus {
                        status: status.to_string(),
                        since: now,
                        approx: true,
                    },
                );
                Some((now, true))
            }
        }
    }
}

/// Build the gateway workspace tree from a Herdr snapshot.
pub fn build_tree(snapshot: &Snapshot, tracker: &mut StatusTracker, now: f64) -> WorkspaceTree {
    let agents_by_pane: HashMap<&str, &HerdrAgent> = snapshot
        .agents
        .iter()
        .map(|agent| (agent.pane_id.as_str(), agent))
        .collect();

    let mut panes_by_tab: HashMap<&str, Vec<&HerdrPane>> = HashMap::new();
    for pane in &snapshot.panes {
        panes_by_tab
            .entry(pane.tab_id.as_str())
            .or_default()
            .push(pane);
    }

    let mut tabs_by_workspace: HashMap<&str, Vec<&HerdrTab>> = HashMap::new();
    for tab in &snapshot.tabs {
        tabs_by_workspace
            .entry(tab.workspace_id.as_str())
            .or_default()
            .push(tab);
    }

    let mut groups = Vec::with_capacity(snapshot.workspaces.len());
    for workspace in &snapshot.workspaces {
        let mut children = Vec::new();
        if let Some(tabs) = tabs_by_workspace.get(workspace.workspace_id.as_str()) {
            for tab in tabs {
                let panes = panes_by_tab
                    .get(tab.tab_id.as_str())
                    .map_or(&[][..], Vec::as_slice);
                children.push(build_child(tab, panes, &agents_by_pane, tracker, now));
            }
        }
        let cwd = children.iter().find_map(|child| child.cwd.clone());
        let (status_since, status_approx) = tracker
            .observe(
                workspace.workspace_id.as_str(),
                workspace.agent_status.as_deref(),
                now,
            )
            .map_or((None, None), |(since, approx)| (Some(since), Some(approx)));
        groups.push(TreeGroup {
            id: workspace.workspace_id.clone(),
            label: workspace.label.clone().unwrap_or_default(),
            focused: workspace.focused,
            agent_status: workspace.agent_status.clone(),
            cwd,
            status_changed_at: status_since,
            status_changed_at_approx: status_approx,
            children,
        });
    }

    WorkspaceTree {
        kind: "herdr".to_string(),
        capabilities: TreeCapabilities {
            pane_list: true,
            pane_focus: "exact".to_string(),
        },
        groups,
    }
}

fn build_child(
    tab: &HerdrTab,
    panes: &[&HerdrPane],
    agents_by_pane: &HashMap<&str, &HerdrAgent>,
    tracker: &mut StatusTracker,
    now: f64,
) -> TreeChild {
    let mut tree_panes = Vec::with_capacity(panes.len());
    for pane in panes {
        let (status_since, status_approx) = tracker
            .observe(&pane.pane_id, pane.agent_status.as_deref(), now)
            .map_or((None, None), |(since, approx)| (Some(since), Some(approx)));
        tree_panes.push(TreePane {
            id: pane.pane_id.clone(),
            label: pane
                .label
                .clone()
                .or_else(|| pane.terminal_title.clone())
                .unwrap_or_default(),
            focused: pane.focused,
            agent: pane.agent.clone(),
            agent_status: pane.agent_status.clone(),
            session_id: session_value(&pane.agent_session),
            title: None,
            model: None,
            context_remaining: None,
            cwd: pane.cwd.clone().or_else(|| pane.foreground_cwd.clone()),
            command: None,
            state_change_order: agents_by_pane
                .get(pane.pane_id.as_str())
                .and_then(|agent| agent.state_change_seq),
            status_changed_at: status_since,
            status_changed_at_approx: status_approx,
        });
    }

    let agent_pane = tree_panes.iter().find(|pane| pane.agent.is_some());
    let agent = agent_pane.and_then(|pane| pane.agent.clone());
    let session_id = agent_pane.and_then(|pane| pane.session_id.clone());
    let title = agent_pane.and_then(|pane| pane.title.clone());
    let model = agent_pane.and_then(|pane| pane.model.clone());
    let context_remaining = agent_pane.and_then(|pane| pane.context_remaining);
    let agent_pane_count = tree_panes
        .iter()
        .filter(|pane| pane.agent.is_some())
        .count() as u32;
    let state_change_order = tree_panes
        .iter()
        .filter_map(|pane| pane.state_change_order)
        .max();
    let cwd = tree_panes.iter().find_map(|pane| pane.cwd.clone());

    let (status_since, status_approx) = tracker
        .observe(&tab.tab_id, tab.agent_status.as_deref(), now)
        .map_or((None, None), |(since, approx)| (Some(since), Some(approx)));

    TreeChild {
        id: tab.tab_id.clone(),
        label: tab.label.clone().unwrap_or_default(),
        focused: tab.focused,
        agent_status: tab.agent_status.clone(),
        agent,
        session_id,
        title,
        model,
        context_remaining,
        cwd,
        command: None,
        pane_count: tab.pane_count.unwrap_or(tree_panes.len() as u32),
        agent_pane_count,
        state_change_order,
        status_changed_at: status_since,
        status_changed_at_approx: status_approx,
        panes: tree_panes,
    }
}

fn session_value(session: &Option<AgentSession>) -> Option<String> {
    session
        .as_ref()
        .map(|session| session.value.clone())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Snapshot {
        serde_json::from_str(include_str!("../tests/fixtures/herdr-snapshot.json"))
            .expect("fixture parses")
    }

    #[test]
    fn builds_tree_from_herdr_snapshot() {
        let mut tracker = StatusTracker::default();
        let tree = build_tree(&fixture(), &mut tracker, 100.0);

        assert_eq!(tree.kind, "herdr");
        assert!(tree.capabilities.pane_list);
        assert_eq!(tree.capabilities.pane_focus, "exact");
        assert_eq!(tree.groups.len(), 2);

        let first = &tree.groups[0];
        assert_eq!(first.id, "wA");
        assert_eq!(first.label, "app-moshi");
        assert!(first.focused);
        assert_eq!(first.agent_status.as_deref(), Some("working"));
        assert_eq!(first.cwd.as_deref(), Some("/home/user/Code/app-moshi"));
        assert_eq!(first.status_changed_at, Some(100.0));
        assert_eq!(first.children.len(), 2);

        let agent_tab = &first.children[0];
        assert_eq!(agent_tab.id, "wA:t1");
        assert_eq!(agent_tab.agent.as_deref(), Some("claude"));
        assert_eq!(agent_tab.session_id.as_deref(), Some("sess-claude-1"));
        assert_eq!(agent_tab.pane_count, 2);
        assert_eq!(agent_tab.agent_pane_count, 1);
        assert_eq!(agent_tab.state_change_order, Some(42));
        assert_eq!(agent_tab.status_changed_at, Some(100.0));
        assert_eq!(agent_tab.status_changed_at_approx, Some(true));
        assert_eq!(agent_tab.panes.len(), 2);

        let shell_tab = &first.children[1];
        assert_eq!(shell_tab.id, "wA:t2");
        assert_eq!(shell_tab.agent, None);
        assert_eq!(shell_tab.session_id, None);

        let pi_tab = &tree.groups[1].children[0];
        assert_eq!(pi_tab.id, "wB:t1");
        assert_eq!(pi_tab.agent.as_deref(), Some("pi"));
        assert_eq!(
            pi_tab.session_id.as_deref(),
            Some("/home/user/.pi/agent/sessions/x.jsonl")
        );
    }

    #[test]
    fn status_tracker_flags_first_sighting_and_transitions() {
        let mut tracker = StatusTracker::default();

        assert_eq!(
            tracker.observe("p1", Some("working"), 10.0),
            Some((10.0, true))
        );
        assert_eq!(
            tracker.observe("p1", Some("working"), 20.0),
            Some((10.0, true))
        );
        assert_eq!(
            tracker.observe("p1", Some("idle"), 30.0),
            Some((30.0, false))
        );
        assert_eq!(
            tracker.observe("p1", Some("idle"), 40.0),
            Some((30.0, false))
        );
        assert_eq!(tracker.observe("p1", None, 50.0), None);
    }
}
