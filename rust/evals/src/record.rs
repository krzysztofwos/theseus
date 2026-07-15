//! Recording a live run: parse an `AGENT_TRACE` log into turn and tool-call
//! counts, and write a result row under `evals/runs/`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What one live run of a goal did and whether its artifact held.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub goal_id: u8,
    pub goal_title: String,
    /// Seconds since the Unix epoch when the run was recorded.
    pub unix_time: u64,
    /// The harness commit the run drove, for reproducing it.
    pub commit: String,
    /// The highest turn the trace reached.
    pub turns: usize,
    /// How many times each tool was called, by name.
    pub tool_calls: BTreeMap<String, usize>,
    /// The deterministic acceptance verdict after the run.
    pub acceptance: String,
    /// Where the full `AGENT_TRACE` log was written.
    pub trace_path: String,
}

/// The turn count and per-tool call counts an `AGENT_TRACE` log records. The
/// trace prints `[turn N] call <name>(...)` per call, so the highest `N` is the
/// turn count and each `call <name>(` is one tool call.
pub fn parse_trace(trace: &str) -> (usize, BTreeMap<String, usize>) {
    let mut turns = 0;
    let mut tool_calls = BTreeMap::new();
    for line in trace.lines() {
        if let Some(turn) = turn_number(line) {
            turns = turns.max(turn);
        }
        if let Some(tool) = tool_name(line) {
            *tool_calls.entry(tool.to_string()).or_insert(0) += 1;
        }
    }
    (turns, tool_calls)
}

/// The turn number a `[turn N] …` line names.
fn turn_number(line: &str) -> Option<usize> {
    let rest = line.strip_prefix("[turn ")?;
    let number = rest.split(']').next()?;
    number.trim().parse().ok()
}

/// The tool a `[turn N] call <name>(…)` line calls.
fn tool_name(line: &str) -> Option<&str> {
    let rest = line.split("] call ").nth(1)?;
    rest.split('(').next().map(str::trim)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trace_yields_its_turn_and_tool_counts() {
        let trace = "\
[turn 1] say: starting
[turn 1] call snapshot({\"label\":\"x\"})
[turn 1]   -> abc123
[turn 2] call patch({...})
[turn 2] call patch({...})
[turn 3] call verify({})
[turn 3] say: done";
        let (turns, calls) = parse_trace(trace);
        assert_eq!(turns, 3);
        assert_eq!(calls.get("snapshot"), Some(&1));
        assert_eq!(calls.get("patch"), Some(&2));
        assert_eq!(calls.get("verify"), Some(&1));
        // A `say` line is not a tool call.
        assert_eq!(calls.get("say"), None);
    }

    #[test]
    fn a_trace_with_no_calls_is_zero_turns() {
        let (turns, calls) = parse_trace("no trace markers here\n");
        assert_eq!(turns, 0);
        assert!(calls.is_empty());
    }
}
