//! Reasoning verbs: plan / reason.

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
