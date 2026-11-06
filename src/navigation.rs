//! Presentation-only inventory. Never infer task ownership or attach by a label.
use crate::model::{Run, Target};

/// Bounded case-insensitive subsequence ranking. Exact fields beat substrings,
/// which beat fuzzy matches; whitespace-separated terms may span fields.
pub fn match_score(query: &str, fields: &[&str]) -> Option<usize> {
    let query = query.chars().take(256).collect::<String>().to_lowercase();
    let fields: Vec<String> = fields
        .iter()
        .map(|field| field.chars().take(4096).collect::<String>().to_lowercase())
        .collect();
    let mut total = 0;
    for term in query.split_whitespace() {
        let best = fields
            .iter()
            .filter_map(|field| {
                if field == term {
                    return Some(0);
                }
                if let Some(index) = field.find(term) {
                    return Some(100 + field[..index].chars().count());
                }
                let mut wanted = term.chars().peekable();
                let (mut first, mut last, mut matched, mut boundaries) = (0, 0, 0, 0);
                let mut boundary = true;
                for (index, ch) in field.chars().enumerate() {
                    if wanted.peek() == Some(&ch) {
                        if matched == 0 {
                            first = index;
                        }
                        last = index;
                        matched += 1;
                        boundaries += usize::from(boundary);
                        wanted.next();
                        if wanted.peek().is_none() {
                            break;
                        }
                    }
                    boundary = !ch.is_alphanumeric();
                }
                wanted.peek().is_none().then(|| {
                    10_000
                        + first
                        + (last + 1 - first - matched) * 8
                        + matched.saturating_sub(boundaries)
                })
            })
            .min()?;
        total += best;
    }
    Some(total)
}

pub fn record_visit(run: &mut Run) {
    let Some(key) = run.selected.as_ref() else {
        return;
    };
    if !available_key(run, key) {
        return;
    }
    let key = key.clone();
    run.recent.retain(|old| old != &key);
    run.recent.insert(0, key);
    run.recent.truncate(20);
}

fn available_key(run: &Run, key: &str) -> bool {
    run.targets
        .iter()
        .any(|target| target.key == key && !target.dead)
        && !run
            .tiles
            .iter()
            .any(|tile| tile.target.key == key && tile.disconnected.is_some())
}

pub fn previous_key(run: &Run) -> Result<String, String> {
    run.recent
        .iter()
        .find(|key| Some(*key) != run.selected.as_ref() && available_key(run, key))
        .cloned()
        .ok_or_else(|| "No previous live session in this run; visit another terminal first".into())
}

pub struct Entry<'a> {
    pub target: &'a Target,
    pub tile: Option<usize>,
    pub disconnected: bool,
}

impl Entry<'_> {
    pub fn available(&self) -> bool {
        !self.disconnected && !self.target.dead
    }

    pub fn project(&self) -> Option<&str> {
        self.target.project.as_deref()
    }

    pub fn workspace(&self) -> (Option<&str>, Option<&str>) {
        (
            self.target.workspace.as_deref(),
            self.target.task.as_deref(),
        )
    }

    pub fn badges(&self, run: &Run) -> String {
        let focused = run.selected.as_deref() == Some(self.target.key.as_str());
        let mut labels = Vec::new();
        if focused {
            labels.push("Selected".into());
        }
        if let Some(tile) = self.tile {
            labels.push(format!("Slot {}", tile + 1));
        }
        labels.push(
            if self.disconnected {
                "DISCONNECTED"
            } else if self.target.dead {
                "EXITED"
            } else if self.tile.is_some() {
                if focused && run.control {
                    "Input enabled"
                } else {
                    "Read only"
                }
            } else {
                "LIVE"
            }
            .into(),
        );
        labels.join(" · ")
    }
}

pub fn entries<'a>(run: &'a Run, query: &str) -> Vec<Entry<'a>> {
    let mut entries: Vec<_> = run
        .targets
        .iter()
        .map(|target| {
            let tile = run
                .tiles
                .iter()
                .position(|tile| tile.target.key == target.key);
            Entry {
                target,
                tile,
                disconnected: tile.is_some_and(|index| run.tiles[index].disconnected.is_some()),
            }
        })
        .collect();
    // Keep stale pins as distinct old identities, even after a name/pane ID is reused.
    entries.extend(
        run.tiles
            .iter()
            .enumerate()
            .filter(|(_, tile)| {
                !run.targets
                    .iter()
                    .any(|target| target.key == tile.target.key)
            })
            .map(|(index, tile)| Entry {
                target: &tile.target,
                tile: Some(index),
                disconnected: true,
            }),
    );
    let score = |entry: &Entry<'_>| {
        let target = entry.target;
        let badges = entry.badges(run);
        match_score(
            query,
            &[
                &target.session_name,
                &target.pane_id,
                &target.cwd,
                target.project.as_deref().unwrap_or(""),
                target.workspace.as_deref().unwrap_or(""),
                target.task.as_deref().unwrap_or(""),
                &badges,
            ],
        )
    };
    let mut ranked: Vec<_> = entries
        .into_iter()
        .filter_map(|entry| score(&entry).map(|score| (score, entry)))
        .collect();
    ranked.sort_by(|(score_a, a), (score_b, b)| {
        (
            score_a,
            a.project().is_none(),
            a.project(),
            a.workspace(),
            &a.target.session_name,
            &a.target.pane_id,
            &a.target.key,
        )
            .cmp(&(
                score_b,
                b.project().is_none(),
                b.project(),
                b.workspace(),
                &b.target.session_name,
                &b.target.pane_id,
                &b.target.key,
            ))
    });
    ranked.into_iter().map(|(_, entry)| entry).collect()
}

pub fn name(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

#[cfg(test)]
pub(crate) fn fixture() -> Run {
    serde_json::from_value(serde_json::json!({
        "ui_socket":"/tmp/ui", "supervisor_pid":1,"supervisor_start":"1",
        "ui_server_pid":2,"ui_server_start":"2","original_socket":"/tmp/original",
        "executable":"fixture","leader":"C-]","projects":[],"targets":[],
        "selected":null,"terminal_pane":null,"sidebar_pane":null,"control":false,
        "message":"","tiles":[],"view":"focus","preferences":null
    }))
    .unwrap()
}

#[cfg(test)]
pub(crate) fn target(key: &str, project: Option<&str>, workspace: Option<&str>) -> Target {
    serde_json::from_value(serde_json::json!({
        "key":key,"socket":"/tmp/original","server_pid":1,"server_start":"1",
        "session_id":"$1","session_created":"1","session_name":"same-name",
        "window_id":"@1","pane_id":"%1","pane_pid":2,"pane_start":"2",
        "cwd":"/cwd","dead":false,"project":project,"workspace":workspace,
        "task":null,"claude_session_id":null,"association":"unknown"
    }))
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Tile;

    #[test]
    fn fuzzy_search_prefers_exact_then_substring_and_spans_unicode_fields() {
        assert!(match_score("cpt", &["cockpit"]).is_some());
        assert!(match_score("cpt", &["terminal"]).is_none());
        assert!(match_score("test", &["test"]) < match_score("test", &["my-test"]));
        assert!(match_score("test", &["my-test"]) < match_score("test", &["t-e-s-t"]));
        assert!(match_score("ÅP 日本", &["Ångström Project", "/作業/日本語"]).is_some());
        assert_eq!(match_score("", &["anything"]), Some(0));
        let mut run = fixture();
        let mut a = target("a", Some("/a/project"), None);
        a.session_name = "c-o-c-k-p-i-t".into();
        let mut b = target("b", Some("/b/project"), None);
        b.session_name = "cockpit".into();
        run.targets = vec![a, b];
        assert_eq!(entries(&run, "cockpit")[0].target.key, "b");
    }

    #[test]
    fn previous_visits_alternate_and_never_match_replacement_names() {
        let mut run = fixture();
        run.targets = vec![target("a", None, None), target("b", None, None)];
        run.selected = Some("a".into());
        record_visit(&mut run);
        assert!(previous_key(&run).is_err());
        run.selected = Some("b".into());
        record_visit(&mut run);
        assert_eq!(previous_key(&run).unwrap(), "a");
        run.selected = Some(previous_key(&run).unwrap());
        record_visit(&mut run);
        assert_eq!(previous_key(&run).unwrap(), "b");
        run.targets[1].key = "replacement-same-name".into();
        assert!(previous_key(&run).is_err());
        assert_eq!(run.selected.as_deref(), Some("a"));
        run.targets[1].key = "b".into();
        run.tiles.push(Tile {
            target: run.targets[1].clone(),
            pane: "%9".into(),
            disconnected: Some("lost".into()),
            visible: false,
        });
        assert!(previous_key(&run).is_err());
    }

    #[test]
    fn visits_are_bounded_unique_and_skip_dead_targets() {
        let mut run = fixture();
        for index in 0..24 {
            let key = index.to_string();
            run.targets.push(target(&key, None, None));
            run.selected = Some(key);
            record_visit(&mut run);
        }
        assert_eq!(run.recent.len(), 20);
        record_visit(&mut run);
        assert_eq!(run.recent.len(), 20);
        run.targets[22].dead = true;
        assert_eq!(previous_key(&run).unwrap(), "21");
    }

    #[test]
    fn grouping_keeps_same_named_projects_and_workspaces_distinct() {
        let mut run = fixture();
        run.targets = vec![
            target("other", None, None),
            target("b", Some("/b/project"), Some("/b/project/task")),
            target("a2", Some("/a/project"), Some("/a/project/task2")),
            target("a", Some("/a/project"), Some("/a/project/task")),
        ];
        let entries = entries(&run, "");
        assert_eq!(
            entries
                .iter()
                .map(|e| e.target.key.as_str())
                .collect::<Vec<_>>(),
            ["a", "a2", "b", "other"]
        );
        assert_ne!(entries[0].project(), entries[2].project());
    }

    #[test]
    fn missing_pin_never_becomes_a_same_named_live_replacement() {
        let mut run = fixture();
        let old = target("old-generation", Some("/project"), Some("/project/task"));
        run.tiles.push(Tile {
            target: old,
            pane: "%8".into(),
            disconnected: None,
            visible: true,
        });
        run.targets.push(target(
            "new-generation",
            Some("/project"),
            Some("/project/task"),
        ));
        let entries = entries(&run, "same-name");
        assert_eq!(entries.len(), 2);
        let old = entries
            .iter()
            .find(|e| e.target.key == "old-generation")
            .unwrap();
        assert!(!old.available());
        assert!(old.badges(&run).contains("DISCONNECTED"));
        assert!(
            entries
                .iter()
                .find(|e| e.target.key == "new-generation")
                .unwrap()
                .available()
        );
    }

    #[test]
    fn pin_and_focus_labels_follow_observed_input_mode_and_search() {
        let mut run = fixture();
        let target = target("selected", Some("/project"), Some("/project/task"));
        run.targets.push(target.clone());
        run.tiles.push(Tile {
            target,
            pane: "%8".into(),
            disconnected: None,
            visible: true,
        });
        run.selected = Some("selected".into());
        run.control = true;
        assert!(
            entries(&run, "input")[0]
                .badges(&run)
                .contains("Selected · Slot 1 · Input enabled")
        );
        run.control = false;
        assert!(
            entries(&run, "read only")[0]
                .badges(&run)
                .contains("Read only")
        );
        run.tiles[0].disconnected = Some("attachment lost".into());
        run.control = true;
        assert!(
            !entries(&run, "disconnected")[0]
                .badges(&run)
                .contains("Input enabled")
        );
    }
}
