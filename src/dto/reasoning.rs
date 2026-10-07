//! Reasoning verbs: plan / reason.

// The DTOs below mirror the HTTP contract one-for-one: the JSON field names are
// the API, and `tools/http_manifest.py` emits every one of them — with its type
// and serde attributes — into `contract/http-routes.json`, which the three SDK
// clients are checked against. A doc comment on each of ~312 fields would
// restate the field name; the ones that carry meaning beyond their name have
// one written below.
#![allow(missing_docs)]

use brain_db_sdk::new_id;
use brain_db_sdk::wire::types::{
    InferenceKind, InferenceStep, ObservationInput, PlanBudget, PlanRequest, PlanState, PlanStep,
    PlanStrategy, ReasonRequest, TransitionKind,
};
use serde::{Deserialize, Serialize};

use super::parse_memory_id;

/// A plan/reason endpoint: either free `text` or an existing `memory_id`.
#[derive(Debug, Deserialize)]
pub struct EndpointSpec {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub memory_id: Option<String>,
}

impl EndpointSpec {
    fn resolve(&self, field: &str) -> Result<Resolved, String> {
        match (&self.memory_id, &self.text) {
            (Some(id), _) => Ok(Resolved::Memory(parse_memory_id(id)?)),
            (None, Some(t)) if !t.trim().is_empty() => Ok(Resolved::Text(t.clone())),
            _ => Err(format!("{field} needs `text` or `memory_id`")),
        }
    }

    fn plan_state(&self, field: &str) -> Result<PlanState, String> {
        Ok(match self.resolve(field)? {
            Resolved::Memory(id) => PlanState::ByMemoryId(id),
            Resolved::Text(t) => PlanState::ByText(t),
        })
    }

    fn observation(&self, field: &str) -> Result<ObservationInput, String> {
        Ok(match self.resolve(field)? {
            Resolved::Memory(id) => ObservationInput::ByMemoryId(id),
            Resolved::Text(t) => ObservationInput::ByText(t),
        })
    }
}

enum Resolved {
    Memory(u128),
    Text(String),
}

fn parse_strategy(s: &str) -> Result<PlanStrategy, String> {
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "auto" => PlanStrategy::Auto,
        "astar" | "a_star" | "a*" => PlanStrategy::AStar,
        "mcts" => PlanStrategy::Mcts,
        "attractor_rollout" | "attractor" => PlanStrategy::AttractorRollout,
        other => return Err(format!("unknown plan strategy `{other}`")),
    })
}

/// `POST /v1/plan` body.
#[derive(Debug, Deserialize)]
pub struct PlanBody {
    pub start: EndpointSpec,
    pub goal: EndpointSpec,
    #[serde(default = "default_max_steps")]
    pub max_steps: u32,
    #[serde(default = "default_wall_ms")]
    pub max_wall_time_ms: u32,
    #[serde(default = "default_branches")]
    pub max_branches: u32,
    #[serde(default)]
    pub strategy: Option<String>,
}

fn default_max_steps() -> u32 {
    8
}
fn default_wall_ms() -> u32 {
    5_000
}
fn default_branches() -> u32 {
    32
}

impl PlanBody {
    /// Build the wire request (mints a fresh `request_id`).
    pub fn to_request(&self) -> Result<PlanRequest, String> {
        let strategy_hint = match &self.strategy {
            Some(s) => Some(parse_strategy(s)?),
            None => None,
        };
        Ok(PlanRequest {
            start: self.start.plan_state("start")?,
            goal: self.goal.plan_state("goal")?,
            budget: PlanBudget {
                max_steps: self.max_steps,
                max_wall_time_ms: self.max_wall_time_ms,
                max_branches_explored: self.max_branches,
            },
            strategy_hint,
            session_filter: None,
            request_id: Some(new_id()),
            txn_id: None,
            trace: false,
            act_as: None,
        })
    }
}

/// One planned step.
#[derive(Debug, Serialize)]
pub struct PlanStepDto {
    pub step_index: u32,
    pub memory_id: String,
    pub text: String,
    pub transition_kind: String,
    pub confidence: f32,
    pub estimated_distance_to_goal: f32,
}

impl From<PlanStep> for PlanStepDto {
    fn from(s: PlanStep) -> Self {
        Self {
            step_index: s.step_index,
            memory_id: s.memory_id.to_string(),
            text: s.text,
            transition_kind: transition_kind_str(&s.transition_kind),
            confidence: s.confidence,
            estimated_distance_to_goal: s.estimated_distance_to_goal,
        }
    }
}

/// `POST /v1/plan` response.
#[derive(Debug, Serialize)]
pub struct PlanResponseDto {
    pub steps: Vec<PlanStepDto>,
}

impl From<Vec<PlanStep>> for PlanResponseDto {
    fn from(v: Vec<PlanStep>) -> Self {
        Self {
            steps: v.into_iter().map(PlanStepDto::from).collect(),
        }
    }
}

fn transition_kind_str(k: &TransitionKind) -> String {
    match k {
        TransitionKind::Initial => "initial".into(),
        TransitionKind::Causal => "causal".into(),
        TransitionKind::Temporal => "temporal".into(),
        TransitionKind::Similarity => "similarity".into(),
        TransitionKind::Other(s) => s.clone(),
    }
}

/// `POST /v1/reason` body.
#[derive(Debug, Deserialize)]
pub struct ReasonBody {
    pub observation: EndpointSpec,
    #[serde(default = "default_depth")]
    pub depth: u32,
    #[serde(default = "default_threshold")]
    pub confidence_threshold: f32,
    #[serde(default = "default_max_inferences")]
    pub max_inferences: u32,
    #[serde(default = "default_wall_ms")]
    pub budget_wall_time_ms: u32,
}

fn default_depth() -> u32 {
    3
}
fn default_threshold() -> f32 {
    0.5
}
fn default_max_inferences() -> u32 {
    10
}

impl ReasonBody {
    /// Build the wire request (mints a fresh `request_id`).
    pub fn to_request(&self) -> Result<ReasonRequest, String> {
        Ok(ReasonRequest {
            observation: self.observation.observation("observation")?,
            depth: self.depth,
            confidence_threshold: self.confidence_threshold,
            session_filter: None,
            max_inferences: self.max_inferences,
            budget_wall_time_ms: self.budget_wall_time_ms,
            request_id: Some(new_id()),
            txn_id: None,
            trace: false,
            act_as: None,
        })
    }
}

/// One inference step.
#[derive(Debug, Serialize)]
pub struct InferenceStepDto {
    pub step_index: u32,
    pub claim: String,
    pub supporting_memories: Vec<String>,
    pub contradicting_memories: Vec<String>,
    pub confidence: f32,
    pub inference_kind: String,
}

impl From<InferenceStep> for InferenceStepDto {
    fn from(s: InferenceStep) -> Self {
        Self {
            step_index: s.step_index,
            claim: s.claim,
            supporting_memories: s
                .supporting_memories
                .iter()
                .map(ToString::to_string)
                .collect(),
            contradicting_memories: s
                .contradicting_memories
                .iter()
                .map(ToString::to_string)
                .collect(),
            confidence: s.confidence,
            inference_kind: inference_kind_str(&s.inference_kind),
        }
    }
}

/// `POST /v1/reason` response.
#[derive(Debug, Serialize)]
pub struct ReasonResponseDto {
    pub inferences: Vec<InferenceStepDto>,
}

impl From<Vec<InferenceStep>> for ReasonResponseDto {
    fn from(v: Vec<InferenceStep>) -> Self {
        Self {
            inferences: v.into_iter().map(InferenceStepDto::from).collect(),
        }
    }
}

fn inference_kind_str(k: &InferenceKind) -> String {
    match k {
        InferenceKind::CausalExplanation => "causal_explanation".into(),
        InferenceKind::EvidenceAccumulation => "evidence_accumulation".into(),
        InferenceKind::AnalogicalInference => "analogical_inference".into(),
        InferenceKind::Other(s) => s.clone(),
    }
}

#[cfg(test)]
mod reasoning_tests {
    use super::*;

    fn text_endpoint(t: &str) -> EndpointSpec {
        EndpointSpec {
            text: Some(t.into()),
            memory_id: None,
        }
    }

    #[test]
    fn plan_body_resolves_endpoints_parses_strategy_and_keeps_budget() {
        let body = PlanBody {
            start: text_endpoint("at home"),
            goal: EndpointSpec {
                text: None,
                memory_id: Some("42".into()),
            },
            max_steps: 12,
            max_wall_time_ms: 2_000,
            max_branches: 16,
            strategy: Some(" A* ".into()),
        };
        let req = body.to_request().unwrap();
        assert!(matches!(req.start, PlanState::ByText(ref t) if t == "at home"));
        assert!(matches!(req.goal, PlanState::ByMemoryId(42)));
        assert!(matches!(req.strategy_hint, Some(PlanStrategy::AStar)));
        assert_eq!(req.budget.max_steps, 12);
        assert_eq!(req.budget.max_wall_time_ms, 2_000);
        assert_eq!(req.budget.max_branches_explored, 16);
        assert!(req.request_id.is_some());
    }

    #[test]
    fn plan_body_rejects_empty_endpoint_and_unknown_strategy() {
        // An endpoint with neither text nor memory_id is an error.
        let missing = PlanBody {
            start: EndpointSpec {
                text: None,
                memory_id: None,
            },
            goal: text_endpoint("b"),
            max_steps: 8,
            max_wall_time_ms: 5_000,
            max_branches: 32,
            strategy: None,
        };
        assert!(missing.to_request().is_err());

        // A blank text does not count as a supplied endpoint.
        let blank = PlanBody {
            start: text_endpoint("   "),
            goal: text_endpoint("b"),
            max_steps: 8,
            max_wall_time_ms: 5_000,
            max_branches: 32,
            strategy: None,
        };
        assert!(blank.to_request().is_err());

        let bad_strategy = PlanBody {
            start: text_endpoint("a"),
            goal: text_endpoint("b"),
            max_steps: 8,
            max_wall_time_ms: 5_000,
            max_branches: 32,
            strategy: Some("teleport".into()),
        };
        assert!(bad_strategy.to_request().is_err());
    }

    #[test]
    fn plan_response_maps_steps_and_transition_kinds() {
        let steps = vec![
            PlanStep {
                step_index: 0,
                memory_id: 7,
                text: "start".into(),
                transition_kind: TransitionKind::Initial,
                confidence: 1.0,
                estimated_distance_to_goal: 3.0,
            },
            PlanStep {
                step_index: 1,
                memory_id: 8,
                text: "next".into(),
                transition_kind: TransitionKind::Other("teleport".into()),
                confidence: 0.7,
                estimated_distance_to_goal: 1.0,
            },
        ];
        let dto = PlanResponseDto::from(steps);
        assert_eq!(dto.steps.len(), 2);
        assert_eq!(dto.steps[0].memory_id, "7");
        assert_eq!(dto.steps[0].transition_kind, "initial");
        assert_eq!(dto.steps[1].transition_kind, "teleport"); // Other passthrough
    }

    #[test]
    fn reason_body_resolves_observation_and_carries_budget() {
        let body = ReasonBody {
            observation: EndpointSpec {
                text: None,
                memory_id: Some("9".into()),
            },
            depth: 4,
            confidence_threshold: 0.6,
            max_inferences: 7,
            budget_wall_time_ms: 3_000,
        };
        let req = body.to_request().unwrap();
        assert!(matches!(req.observation, ObservationInput::ByMemoryId(9)));
        assert_eq!(req.depth, 4);
        assert_eq!(req.confidence_threshold.to_bits(), 0.6f32.to_bits());
        assert_eq!(req.max_inferences, 7);
        assert_eq!(req.budget_wall_time_ms, 3_000);
        assert!(req.request_id.is_some());

        assert!(
            ReasonBody {
                observation: EndpointSpec {
                    text: None,
                    memory_id: None,
                },
                depth: 3,
                confidence_threshold: 0.5,
                max_inferences: 10,
                budget_wall_time_ms: 5_000,
            }
            .to_request()
            .is_err()
        );
    }

    #[test]
    fn reason_response_maps_inferences_and_kinds() {
        let steps = vec![InferenceStep {
            step_index: 0,
            claim: "it rained".into(),
            supporting_memories: vec![1, 2],
            contradicting_memories: vec![3],
            confidence: 0.8,
            inference_kind: InferenceKind::CausalExplanation,
        }];
        let dto = ReasonResponseDto::from(steps);
        assert_eq!(dto.inferences.len(), 1);
        assert_eq!(dto.inferences[0].claim, "it rained");
        assert_eq!(dto.inferences[0].supporting_memories, vec!["1", "2"]);
        assert_eq!(dto.inferences[0].contradicting_memories, vec!["3"]);
        assert_eq!(dto.inferences[0].inference_kind, "causal_explanation");
    }
}
