//! Model roles: the named jobs a session hands to a model.
//!
//! A profile runs one model for the conversation. Everything else it asks a
//! model to do — a two-token command check, a subagent that runs for ten
//! minutes, the rolling summary of a long session — is a role, and each role
//! can be given a model of its own, on any provider: `llama/qwen-3-4b` for the
//! cheap background calls beside `openrouter/anthropic/claude-opus` for the
//! conversation.
//!
//! A role left unassigned falls back along [`Role::fallback`] rather than
//! erroring, so a profile that assigns nothing runs everything on its own
//! model, and assigning one is always additive.

use crate::config::Profile;

/// One named job, and what it falls back to when unassigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Role {
    pub id: &'static str,
    pub label: &'static str,
    /// One line, shown beside the role. Says what the role *does*, so the
    /// choice can be made without reading the source.
    pub help: &'static str,
    /// The role consulted when this one is unassigned. `None` on `default`,
    /// which is the root of every chain.
    pub fallback: Option<&'static str>,
}

/// The known roles, in the order they are listed.
///
/// Ordered by how much the choice costs you: the conversation model first,
/// then the jobs that spend tokens in the background, then the one that only
/// matters on a long session.
pub const ROLES: &[Role] = &[
    Role {
        id: "default",
        label: "Model",
        help: "Runs the conversation and every tool call. The model the session is.",
        fallback: None,
    },
    Role {
        id: "aux",
        label: "Auxiliary model",
        help: "Background calls — reading fetched pages and large outputs, drafts.",
        fallback: Some("default"),
    },
    Role {
        id: "review",
        label: "Safety reviewer",
        help: "Judges, in AUTO mode, whether an action is what you asked for. Follows the auxiliary model.",
        fallback: Some("aux"),
    },
    Role {
        id: "subagent",
        label: "Subagent model",
        help: "Delegated agents, when the delegating call does not name a model itself.",
        fallback: Some("default"),
    },
    Role {
        id: "compaction",
        label: "Compaction model",
        help: "The rolling summary. Load-bearing for a long session — worth keeping on a capable model.",
        fallback: Some("default"),
    },
];

/// Look up a role by id.
pub fn role(id: &str) -> Option<&'static Role> {
    ROLES.iter().find(|role| role.id == id)
}

/// How a role resolved: which model, and whether that came from an assignment
/// or from following the fallback chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// `provider/model`.
    pub model: String,
    /// True when no assignment was found and a fallback supplied the model.
    /// The interface dims these, so "inherited" and "chosen" never look alike.
    pub inherited: bool,
}

impl Profile {
    /// The `provider/model` assigned to `id` on this profile, if any.
    pub fn role_model(&self, id: &str) -> Option<&str> {
        let value = match id {
            "default" => Some(self.model.as_str()),
            _ => self.roles.get(id).map(String::as_str),
        };
        value.map(str::trim).filter(|model| !model.is_empty())
    }

    /// Assign a model to `id`, or clear it back to its fallback with `None`.
    ///
    /// Clearing `default` is meaningless — it is the root of every fallback
    /// chain — so it is ignored rather than emptying the profile's model.
    pub fn set_role_model(&mut self, id: &str, model: Option<String>) {
        let model = model.map(|model| model.trim().to_owned()).filter(|model| !model.is_empty());
        match (id, model) {
            ("default", Some(model)) => self.model = model,
            ("default", None) => {}
            (id, Some(model)) => {
                self.roles.insert(id.to_owned(), model);
            }
            (id, None) => {
                self.roles.remove(id);
            }
        }
    }

    /// Resolve a role to the model that will actually serve it, following the
    /// fallback chain. `None` only when the profile has no model at all.
    pub fn resolve_role(&self, id: &str) -> Option<Resolved> {
        let mut current = role(id)?;
        let mut inherited = false;
        loop {
            if let Some(model) = self.role_model(current.id) {
                return Some(Resolved { model: model.to_owned(), inherited });
            }
            current = role(current.fallback?)?;
            inherited = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> Profile {
        Profile::for_model("kojo/kojo")
    }

    #[test]
    fn an_unassigned_role_inherits_down_the_chain() {
        let profile = profile();
        let resolved = profile.resolve_role("subagent").expect("subagent resolves");
        assert_eq!(resolved.model, "kojo/kojo");
        assert!(resolved.inherited, "and says it was inherited");
        // The root is never inherited.
        assert!(!profile.resolve_role("default").unwrap().inherited);
    }

    #[test]
    fn the_reviewer_follows_the_auxiliary_model() {
        let mut profile = profile();
        assert_eq!(profile.resolve_role("review").unwrap().model, "kojo/kojo");
        profile.set_role_model("aux", Some("llama/qwen-3-4b".to_owned()));
        let review = profile.resolve_role("review").unwrap();
        assert_eq!(review.model, "llama/qwen-3-4b", "a cheap aux model judges by default");
        assert!(review.inherited);
        profile.set_role_model("review", Some("kojo/kojo".to_owned()));
        assert!(!profile.resolve_role("review").unwrap().inherited);
    }

    #[test]
    fn clearing_a_role_falls_back_but_never_empties_the_default() {
        let mut profile = profile();
        profile.set_role_model("aux", Some("llama/qwen-3-4b".to_owned()));
        profile.set_role_model("aux", None);
        assert!(profile.roles.is_empty());
        assert!(profile.resolve_role("aux").unwrap().inherited);

        profile.set_role_model("default", None);
        assert_eq!(profile.model, "kojo/kojo", "the root of the chain cannot be cleared out from under the session");
        // Whitespace is not an assignment.
        profile.set_role_model("subagent", Some("   ".to_owned()));
        assert!(profile.resolve_role("subagent").unwrap().inherited);
    }
}
