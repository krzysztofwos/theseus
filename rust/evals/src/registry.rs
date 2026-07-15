//! The goal corpus as data: each live goal, the prompt that drives it, and the
//! deterministic acceptance that proves its artifact survives — no model needed.
//!
//! The narrative record lives in `evals/README.md`; this registry is the
//! runnable projection of it, so `evals list` and the README never disagree
//! about which goals exist.

/// How a goal drives the agent: rooted in the harness itself, or in an isolated
/// foreign project the runner seeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The agent extends the Theseus harness in place.
    SelfMod,
    /// The agent develops a foreign project under `--project`.
    Foreign,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::SelfMod => "self-mod",
            Kind::Foreign => "foreign",
        }
    }
}

/// The deterministic acceptance for a goal: a check the runner performs with no
/// API key, proving the goal's artifact is still present and sound.
#[derive(Debug, Clone)]
pub enum Acceptance {
    /// The Theseus model still exposes an operation the goal grew.
    OperationPresent(&'static str),
    /// A named package's tests still pass; `test` narrows to one integration test.
    CargoTest {
        package: &'static str,
        test: Option<&'static str>,
    },
    /// The goal's proof is a live explanation with no deterministic artifact.
    LiveOnly,
}

/// One corpus goal.
#[derive(Debug, Clone)]
pub struct Goal {
    pub id: u8,
    pub title: &'static str,
    pub proves: &'static str,
    pub kind: Kind,
    /// The goal string handed to the agent for a live run.
    pub prompt: &'static str,
    pub acceptance: Acceptance,
}

/// The nine corpus goals, in the order `evals/README.md` records them.
pub fn goals() -> Vec<Goal> {
    vec![
        Goal {
            id: 1,
            title: "Add an operation with a handler; leave the workspace conformant",
            proves: "the model loop",
            kind: Kind::SelfMod,
            prompt: "Add an operation that reports how many operations your model holds. \
Follow your discipline and leave the workspace conformant.",
            acceptance: Acceptance::OperationPresent("diff"),
        },
        Goal {
            id: 2,
            title: "Grow a port method + adapter, restart, call it live",
            proves: "full self-modification",
            kind: Kind::SelfMod,
            prompt: "Give yourself a way to run the workspace tests through a new toolchain \
port method, then restart and prove it by calling your new tool.",
            acceptance: Acceptance::OperationPresent("test"),
        },
        Goal {
            id: 3,
            title: "Snapshot, break something, roll back",
            proves: "recovery",
            kind: Kind::SelfMod,
            prompt: "Run a recovery drill on yourself: snapshot, deliberately break an \
operation's handler, restart, prove the damage, roll back, prove the tree matches \
the snapshot with diff, restart, and prove the operation works again.",
            acceptance: Acceptance::OperationPresent("rollback"),
        },
        Goal {
            id: 4,
            title: "Scaffold an in-tree service, author it, verify",
            proves: "multi-service",
            kind: Kind::SelfMod,
            prompt: "Grow yourself a second small text-utility service — your choice of \
operations — scaffold its crate, author every handler, and leave the workspace conformant.",
            acceptance: Acceptance::CargoTest {
                package: "theseus-text-utils",
                test: None,
            },
        },
        Goal {
            id: 5,
            title: "Explain a subsystem end to end, citing files",
            proves: "investigation over read/search/list",
            kind: Kind::SelfMod,
            prompt: "Explain how your restart tool works end to end, citing the files and \
specific evidence you gather with search, read, and list. Do not change anything.",
            acceptance: Acceptance::LiveOnly,
        },
        Goal {
            id: 6,
            title: "Author a capability from search/read evidence, not only show",
            proves: "reading before writing",
            kind: Kind::SelfMod,
            prompt: "Give yourself a lint capability over a new toolchain port method, \
running clippy with warnings denied. Gather the local evidence with search and read \
before authoring, and let what you find shape what you write.",
            acceptance: Acceptance::OperationPresent("lint"),
        },
        Goal {
            id: 7,
            title: "Grow a capability in a freshly initialized foreign project",
            proves: "other software through the same catalog",
            kind: Kind::Foreign,
            prompt: "add a health operation, test it, and leave the project conformant",
            acceptance: Acceptance::CargoTest {
                package: "theseus",
                test: Some("initialized_project"),
            },
        },
        Goal {
            id: 8,
            title: "Rebuild and call a new capability from the foreign agent process",
            proves: "foreign process replacement through drive",
            kind: Kind::Foreign,
            prompt: "Give the project a clear operation that empties it, and prove the whole \
capability live from your own session: drive an add, a count, a clear, and a count \
again — the final count must be zero.",
            acceptance: Acceptance::CargoTest {
                package: "theseus",
                test: Some("foreign_project"),
            },
        },
        Goal {
            id: 9,
            title: "Initialize a foreign project from a goal string",
            proves: "agent-visible bootstrap",
            kind: Kind::Foreign,
            prompt: "Stand up a small service for keeping shopping lists: initialize the \
project with a fitting identity, give it an operation that adds an item and one that \
lists them, and prove both live through their own command line.",
            acceptance: Acceptance::CargoTest {
                package: "theseus",
                test: Some("initialized_project"),
            },
        },
    ]
}

/// The goal with `id`, if the registry defines it.
pub fn goal(id: u8) -> Option<Goal> {
    goals().into_iter().find(|goal| goal.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_matches_the_recorded_corpus() {
        let goals = goals();
        assert_eq!(goals.len(), 9, "the corpus records nine goals");
        // Ids are dense and unique from 1, so `evals show <id>` never collides.
        for (index, goal) in goals.iter().enumerate() {
            assert_eq!(goal.id as usize, index + 1, "goal ids are 1..=9 in order");
            assert!(!goal.prompt.is_empty(), "goal {} has a prompt", goal.id);
        }
    }

    #[test]
    fn every_foreign_goal_accepts_through_an_integration_test() {
        // A foreign goal's artifact lives in an isolated root the runner discards,
        // so its deterministic proof is a checked-in integration test, not a
        // model-presence check against the harness.
        for goal in goals().iter().filter(|g| g.kind == Kind::Foreign) {
            assert!(
                matches!(goal.acceptance, Acceptance::CargoTest { .. }),
                "foreign goal {} needs a CargoTest acceptance",
                goal.id
            );
        }
    }
}
