//! Flows: statecharts that lead an AI through a piece of work, step by step.
//!
//! A flow is a JSON file in the project, `<root>/.ontack/flows/<id>.json`. Its states say what
//! the agent is told in each step; its transitions say how it moves on, and what must be true
//! first: a command that passes, the person's approval, or the agent's evidence. States may
//! nest (a state with `states` and an `initial` one): a nested state is entered at its initial
//! child, and a transition of an outer state applies in every state inside it unless an inner
//! one names the same event. A top-level state may be `final`: reaching it completes the run.
//!
//! ```json
//! { "name": "CFD study", "initial": "geometry",
//!   "states": {
//!     "geometry": { "instructions": "Clean the CAD.", "on": { "ready": { "target": "solve", "check": { "approval": true } } } },
//!     "solve": { "initial": "setup", "states": {
//!         "setup": { "on": { "run": { "target": "running", "check": { "command": "python checks/case.py" } } } },
//!         "running": { "on": { "converged": { "target": "report", "evidence": true } } } },
//!       "on": { "diverged": { "target": "geometry" } } },
//!     "report": { "final": true, "instructions": "Write the report." } } }
//! ```
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

pub const PROJECT_DIR: &str = ".ontack/flows";
/// The longest a command check may run.
pub const MAX_CHECK_SECONDS: u64 = 3600;
pub const DEFAULT_CHECK_SECONDS: u64 = 600;

/// What must be true before a transition is taken.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// A shell command run in the chat's folder: the transition is taken when it exits 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Seconds the command may run (default 600, at most 3600).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    /// The person approves the transition in RiWork.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub approval: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    pub target: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<Check>,
    /// The agent must say what it did (`--evidence`), which is kept in the run's history.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub evidence: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub on: BTreeMap<String, Transition>,
    /// Nested states, entered at `initial`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub states: BTreeMap<String, State>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial: Option<String>,
    /// Reaching it completes the run (top-level states only).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub r#final: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flow {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Told to the agent in every state, before the state's own instructions.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions: String,
    pub initial: String,
    pub states: BTreeMap<String, State>,
}

/// A state's or event's name: letters, digits, `_` and `-`, starting with a letter.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A flow's id is its file's stem, with the same rules.
pub fn valid_id(id: &str) -> bool {
    valid_name(id)
}

/// Where a state sits: its parent (`None` at the top) and its own definition.
struct Entry<'a> {
    parent: Option<&'a str>,
    state: &'a State,
}

impl Flow {
    pub fn parse(text: &str) -> Result<Self, String> {
        let flow: Self = serde_json::from_str(text).map_err(|error| error.to_string())?;
        flow.validate()?;
        Ok(flow)
    }

    /// Every state by name, with its parent. Names are unique (`validate`).
    fn index(&self) -> BTreeMap<&str, Entry<'_>> {
        fn walk<'a>(states: &'a BTreeMap<String, State>, parent: Option<&'a str>, index: &mut BTreeMap<&'a str, Entry<'a>>) {
            for (id, state) in states {
                index.insert(id.as_str(), Entry { parent, state });
                walk(&state.states, Some(id.as_str()), index);
            }
        }
        let mut index = BTreeMap::new();
        walk(&self.states, None, &mut index);
        index
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("name is empty".into());
        }
        if self.states.is_empty() {
            return Err("states is empty".into());
        }
        let mut seen = BTreeSet::new();
        let mut duplicate = None;
        fn names<'a>(states: &'a BTreeMap<String, State>, seen: &mut BTreeSet<&'a str>, duplicate: &mut Option<String>) {
            for (id, state) in states {
                if !seen.insert(id.as_str()) {
                    duplicate.get_or_insert_with(|| id.clone());
                }
                names(&state.states, seen, duplicate);
            }
        }
        names(&self.states, &mut seen, &mut duplicate);
        if let Some(id) = duplicate {
            return Err(format!("state {id:?} is named twice: every state needs its own name"));
        }
        if !self.states.contains_key(&self.initial) {
            return Err(format!("initial {:?} is not a top-level state", self.initial));
        }
        for (id, entry) in self.index() {
            let state = entry.state;
            if !valid_name(id) {
                return Err(format!("state {id:?}: names are letters, digits, _ and -, starting with a letter"));
            }
            if state.states.is_empty() {
                if state.initial.is_some() {
                    return Err(format!("state {id:?} has an initial state but no states"));
                }
            } else {
                match &state.initial {
                    Some(initial) if state.states.contains_key(initial) => {}
                    _ => return Err(format!("state {id:?} has states, so it needs an initial one of them")),
                }
                if state.r#final {
                    return Err(format!("state {id:?} is final, so it cannot have states"));
                }
            }
            if state.r#final && entry.parent.is_some() {
                return Err(format!("state {id:?} is final: only top-level states can be"));
            }
            if state.r#final && !state.on.is_empty() {
                return Err(format!("state {id:?} is final, so it has no transitions"));
            }
            for (event, transition) in &state.on {
                if !valid_name(event) {
                    return Err(format!("state {id:?}: event {event:?} must be letters, digits, _ and -"));
                }
                if !seen.contains(transition.target.as_str()) {
                    return Err(format!("state {id:?}: event {event:?} goes to {:?}, which is no state", transition.target));
                }
                if let Some(check) = &transition.check {
                    let command = check.command.as_deref().map(str::trim).filter(|command| !command.is_empty());
                    if command.is_some() == check.approval {
                        return Err(format!("state {id:?}: event {event:?}: a check is a command or an approval, one of them"));
                    }
                    if check.timeout.is_some_and(|seconds| seconds == 0 || seconds > MAX_CHECK_SECONDS) {
                        return Err(format!("state {id:?}: event {event:?}: a check's timeout is 1 to {MAX_CHECK_SECONDS} seconds"));
                    }
                }
            }
        }
        Ok(())
    }

    /// The leaf a state enters at: itself, or its initial descendant.
    pub fn enter(&self, id: &str) -> Option<String> {
        let index = self.index();
        let mut current = id;
        loop {
            let state = index.get(current)?.state;
            match &state.initial {
                Some(initial) if !state.states.is_empty() => current = initial,
                _ => return Some(current.to_owned()),
            }
        }
    }

    /// The state and its outer states, innermost first.
    pub fn path(&self, id: &str) -> Vec<String> {
        let index = self.index();
        let mut path = Vec::new();
        let mut current = Some(id);
        while let Some(id) = current {
            let Some(entry) = index.get(id) else { break };
            path.push(id.to_owned());
            current = entry.parent;
        }
        path
    }

    pub fn state(&self, id: &str) -> Option<&State> {
        self.index().get(id).map(|entry| entry.state)
    }

    pub fn title(&self, id: &str) -> String {
        self.state(id)
            .map(|state| if state.title.is_empty() { id.to_owned() } else { state.title.clone() })
            .unwrap_or_else(|| id.to_owned())
    }

    /// "Solve › Setup": where the state sits, outermost first.
    pub fn location(&self, id: &str) -> String {
        self.path(id).iter().rev().map(|id| self.title(id)).collect::<Vec<_>>().join(" › ")
    }

    /// What the agent is told in a state: the flow's, its outer states', then its own.
    pub fn instructions(&self, id: &str) -> Vec<String> {
        let mut parts = vec![self.instructions.trim().to_owned()];
        parts.extend(self.path(id).iter().rev().filter_map(|id| self.state(id)).map(|state| state.instructions.trim().to_owned()));
        parts.retain(|part| !part.is_empty());
        parts
    }

    /// The events a state can take: its own, then its outer states' that it does not override.
    pub fn events(&self, id: &str) -> Vec<(String, Transition)> {
        let mut events: Vec<(String, Transition)> = Vec::new();
        for state in self.path(id).iter().filter_map(|id| self.state(id)) {
            for (event, transition) in &state.on {
                if !events.iter().any(|(seen, _)| seen == event) {
                    events.push((event.clone(), transition.clone()));
                }
            }
        }
        events
    }

    pub fn is_final(&self, id: &str) -> bool {
        self.state(id).is_some_and(|state| state.r#final)
    }

    /// The flow as a Mermaid state diagram, `active` (a leaf) and its outer states marked.
    pub fn mermaid(&self, active: Option<&str>) -> String {
        fn node(id: &str) -> String {
            format!("s_{}", id.replace('-', "__"))
        }
        fn label(text: &str) -> String {
            text.chars().map(|c| if matches!(c, ':' | ';' | '"' | '{' | '}' | '\n' | '#') { ' ' } else { c }).collect()
        }
        fn write(flow: &Flow, states: &BTreeMap<String, State>, initial: &str, depth: usize, out: &mut Vec<String>) {
            let pad = "  ".repeat(depth);
            out.push(format!("{pad}[*] --> {}", node(initial)));
            for (id, state) in states {
                out.push(format!("{pad}state \"{}\" as {}", label(&flow.title(id)), node(id)));
                if !state.states.is_empty() {
                    out.push(format!("{pad}state {} {{", node(id)));
                    write(flow, &state.states, state.initial.as_deref().unwrap_or_default(), depth + 1, out);
                    out.push(format!("{pad}}}"));
                }
                if state.r#final {
                    out.push(format!("{pad}{} --> [*]", node(id)));
                }
            }
        }
        let mut out = vec!["stateDiagram-v2".to_owned()];
        write(self, &self.states, &self.initial, 1, &mut out);
        for (id, entry) in self.index() {
            for (event, transition) in &entry.state.on {
                let gate = match &transition.check {
                    Some(Check { approval: true, .. }) => " (approval)",
                    Some(_) => " (check)",
                    None if transition.evidence => " (evidence)",
                    None => "",
                };
                out.push(format!("  {} --> {}: {}{gate}", node(id), node(&transition.target), label(event)));
            }
        }
        if let Some(active) = active.filter(|active| self.state(active).is_some()) {
            out.push("  classDef active fill:#f5c46b,stroke:#9a6b00,stroke-width:2px,color:#111".into());
            out.push(format!("  class {} active", node(active)));
        }
        out.join("\n")
    }
}

/// A flow file of a project.
#[derive(Clone, Debug)]
pub struct FoundFlow {
    pub id: String,
    pub path: PathBuf,
    pub flow: Result<Flow, String>,
}

pub fn project_dir(root: &Path) -> PathBuf {
    root.join(PROJECT_DIR)
}

/// The flows of a project, by id; a file that does not parse says why.
pub fn discover(root: &Path) -> Vec<FoundFlow> {
    let Ok(entries) = fs::read_dir(project_dir(root)) else { return Vec::new() };
    let mut found: Vec<FoundFlow> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "json"))
        .filter_map(|path| {
            let id = path.file_stem()?.to_string_lossy().into_owned();
            let flow = if valid_id(&id) {
                fs::read_to_string(&path).map_err(|error| error.to_string()).and_then(|text| Flow::parse(&text))
            } else {
                Err(format!("{id}.json: a flow's file name is letters, digits, _ and -, starting with a letter"))
            };
            Some(FoundFlow { id, path, flow })
        })
        .collect();
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}

pub fn find(root: &Path, id: &str) -> Result<Flow, String> {
    if !valid_id(id) {
        return Err(format!("{id:?} is not a flow id"));
    }
    let path = project_dir(root).join(format!("{id}.json"));
    let text = fs::read_to_string(&path).map_err(|_| format!("The project has no flow {id} ({}).", path.display()))?;
    Flow::parse(&text).map_err(|error| format!("{id}.json: {error}"))
}

/// A new flow to start from: plan, build with a check, review with the person, done.
pub fn template(name: &str) -> Flow {
    let text = r#"{
  "name": "NAME",
  "description": "Plan, build and review a change, step by step.",
  "instructions": "Work in small steps. Say what you did when you move on.",
  "initial": "plan",
  "states": {
    "plan": {
      "title": "Plan",
      "instructions": "Read the code involved and write a short plan. Do not change files yet.",
      "on": { "planned": { "target": "build", "description": "The plan is agreed", "check": { "approval": true } } }
    },
    "build": {
      "title": "Build",
      "instructions": "Implement the plan and keep the tests passing.",
      "on": { "built": { "target": "review", "description": "It is implemented and tested", "evidence": true } }
    },
    "review": {
      "title": "Review",
      "instructions": "Summarize what changed and what to look at.",
      "on": {
        "accepted": { "target": "done", "check": { "approval": true } },
        "rework": { "target": "build", "description": "Something needs another pass" }
      }
    },
    "done": { "title": "Done", "final": true, "instructions": "The work is accepted." }
  }
}"#;
    let mut flow: Flow = serde_json::from_str(text).expect("the template parses");
    flow.name = name.to_owned();
    flow
}

/// The file name a flow called `name` gets: `Code review` is `code-review`.
pub fn id_from_name(name: &str) -> String {
    let mut id = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            id.push(c);
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
    }
    let id: String = id.trim_matches('-').chars().take(48).collect();
    if id.bytes().next().is_some_and(|b| b.is_ascii_alphabetic()) { id } else { format!("flow-{id}").trim_end_matches('-').to_owned() }
}

/// Writes a new flow from the template into the project; refuses one that exists.
pub fn create(root: &Path, name: &str) -> Result<(String, PathBuf), String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 80 {
        return Err("A flow's name is 1 to 80 characters.".into());
    }
    let id = id_from_name(name);
    let dir = project_dir(root);
    let path = dir.join(format!("{id}.json"));
    if path.exists() {
        return Err(format!("{} exists already", path.display()));
    }
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let text = serde_json::to_string_pretty(&template(name)).map_err(|error| error.to_string())?;
    fs::write(&path, text + "\n").map_err(|error| error.to_string())?;
    Ok((id, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn study() -> Flow {
        Flow::parse(r#"{ "name": "CFD study", "instructions": "Be careful.", "initial": "geometry",
          "states": {
            "geometry": { "title": "Geometry", "instructions": "Clean the CAD.",
              "on": { "ready": { "target": "solve", "check": { "approval": true } } } },
            "solve": { "title": "Solve", "instructions": "Run the solver.", "initial": "setup",
              "states": {
                "setup": { "title": "Setup", "instructions": "Set the case up.", "on": { "run": { "target": "running", "check": { "command": "true" } } } },
                "running": { "on": { "converged": { "target": "report", "evidence": true }, "diverged": { "target": "setup" } } } },
              "on": { "diverged": { "target": "geometry" }, "abandon": { "target": "report" } } },
            "report": { "final": true, "instructions": "Write it up." } } }"#)
            .unwrap()
    }

    #[test]
    fn nested_states_enter_at_their_initial_child_and_inherit_events() {
        let flow = study();
        assert_eq!(flow.enter("geometry").as_deref(), Some("geometry"));
        assert_eq!(flow.enter("solve").as_deref(), Some("setup"));
        assert_eq!(flow.path("running"), ["running", "solve"]);
        assert_eq!(flow.location("setup"), "Solve › Setup");
        assert_eq!(flow.instructions("setup"), ["Be careful.", "Run the solver.", "Set the case up."]);
        let events = |id: &str| flow.events(id).into_iter().map(|(event, t)| format!("{event}>{}", t.target)).collect::<Vec<_>>();
        assert_eq!(events("setup"), ["run>running", "abandon>report", "diverged>geometry"]);
        // An inner state's event shadows its outer state's of the same name.
        assert_eq!(events("running"), ["converged>report", "diverged>setup", "abandon>report"]);
        assert!(flow.is_final("report") && !flow.is_final("solve"));
        assert_eq!(flow.events("report"), []);
    }

    #[test]
    fn invalid_flows_say_what_is_wrong() {
        for (text, why) in [
            (r#"{"name": "x", "initial": "a", "states": {}}"#, "states is empty"),
            (r#"{"name": "x", "initial": "b", "states": {"a": {}}}"#, "initial"),
            (r#"{"name": "x", "initial": "a", "states": {"a": {"on": {"go": {"target": "nowhere"}}}}}"#, "no state"),
            (r#"{"name": "x", "initial": "a", "states": {"a": {"states": {"b": {}}}}}"#, "needs an initial"),
            (r#"{"name": "x", "initial": "a", "states": {"a": {"initial": "a1", "states": {"a1": {"final": true}}}}}"#, "only top-level"),
            (r#"{"name": "x", "initial": "a", "states": {"a": {"on": {"go": {"target": "a", "check": {}}}}}}"#, "one of them"),
            (r#"{"name": "x", "initial": "a", "states": {"a": {"on": {"go": {"target": "a", "check": {"command": "x", "approval": true}}}}}}"#, "one of them"),
            (r#"{"name": "x", "initial": "a", "states": {"a": {"on": {"go": {"target": "a", "check": {"command": "x", "timeout": 99999}}}}}}"#, "timeout"),
            (r#"{"name": "x", "initial": "a", "states": {"a": {"initial": "b", "states": {"b": {}}}, "c": {"initial": "b", "states": {"b": {}}}}}"#, "named twice"),
            (r#"{"name": "x", "initial": "a", "states": {"a": {"wat": 1}}}"#, "unknown field"),
            (r#"{"name": "x", "initial": "1a", "states": {"1a": {}}}"#, "names are"),
        ] {
            let error = Flow::parse(text).unwrap_err();
            assert!(error.contains(why), "{text}: {error}");
        }
    }

    #[test]
    fn the_diagram_marks_the_active_state_and_gates() {
        let diagram = study().mermaid(Some("setup"));
        assert!(diagram.starts_with("stateDiagram-v2"));
        assert!(diagram.contains("state s_solve {"));
        assert!(diagram.contains("s_geometry --> s_solve: ready (approval)"));
        assert!(diagram.contains("s_setup --> s_running: run (check)"));
        assert!(diagram.contains("s_running --> s_report: converged (evidence)"));
        assert!(diagram.contains("s_report --> [*]"));
        assert!(diagram.ends_with("class s_setup active"));
        assert!(!study().mermaid(None).contains("classDef"));
    }

    #[test]
    fn projects_keep_flows_by_file_and_new_ones_come_from_the_template() {
        let root = std::env::temp_dir().join(format!("riwork-flows-{}", uuid::Uuid::new_v4()));
        assert!(discover(&root).is_empty());
        let (id, path) = create(&root, "Code review").unwrap();
        assert_eq!(id, "code-review");
        assert!(create(&root, "Code review").is_err());
        fs::write(project_dir(&root).join("broken.json"), "{").unwrap();
        let found = discover(&root);
        assert_eq!(found.iter().map(|flow| flow.id.as_str()).collect::<Vec<_>>(), ["broken", "code-review"]);
        assert!(found[0].flow.is_err());
        let flow = find(&root, "code-review").unwrap();
        assert_eq!(flow.name, "Code review");
        assert_eq!(flow.enter(&flow.initial).as_deref(), Some("plan"));
        assert!(path.is_file());
        assert_eq!(id_from_name("42 checks"), "flow-42-checks");
        fs::remove_dir_all(root).unwrap();
    }
}
