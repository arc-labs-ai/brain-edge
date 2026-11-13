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
    InferenceKind, InferenceStep, ObservationInput, PlanBudget, PlanRequest, PlanResponseFrame,
    PlanState, PlanStatus, PlanStep, PlanStrategy, ReasonRequest, ReasonResponseFrame,
    ReasonStatus, TransitionKind,
};
use serde::{Deserialize, Serialize};

use super::parse_memory_id;
use super::reasoning_trace::{plan_trace_string, reason_trace_string};

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
    /// Ask for Brain's bidirectional-search trace alongside the steps: every
    /// node the BFS visited from each direction, with the edge it was reached
    /// through and its goal alignment, and every meeting point the two
    /// searches found — including the ones a `max_paths` cap dropped.
    ///
    /// Off by default and free when off — the engine builds the trace only
    /// when the request asks for one. Mirrors `/v1/recall`'s `include_trace`.
    #[serde(default)]
    pub include_trace: Option<bool>,
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
            trace: self.include_trace.unwrap_or(false),
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
    /// How the search ENDED, in the engine's own words: `goal_reached`,
    /// `budget_exhausted`, `no_path_found` or `cancelled`.
    ///
    /// An empty `steps` means three different things and a caller cannot tell
    /// them apart from the list alone — a goal that is genuinely unreachable,
    /// a budget that ran out mid-search, and a cancelled run all return no
    /// steps. The engine already distinguishes them on its terminal frame.
    pub status: Option<&'static str>,
    /// The bidirectional-search trace, as a JSON **string**, when
    /// `include_trace` asked for one; omitted otherwise, so a caller that did
    /// not ask sees the response it always saw.
    ///
    /// A string rather than a nested object for the reason every id this API
    /// returns is a decimal string: the trace carries 128-bit memory ids and
    /// a JSON number cannot hold one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
}

impl PlanResponseDto {
    /// Build from the streamed PLAN frames.
    ///
    /// The steps arrive spread across frames and are flattened in order; the
    /// terminal status and the trace ride the final frame alone, so both are
    /// taken from the last frame that carries one.
    #[must_use]
    pub fn from_frames(frames: Vec<PlanResponseFrame>) -> Self {
        let status = frames
            .iter()
            .rev()
            .find_map(|f| f.plan_status)
            .map(plan_status_str);
        let trace = frames
            .iter()
            .rev()
            .find_map(|f| f.trace.as_ref())
            .map(plan_trace_string);
        Self {
            steps: frames
                .into_iter()
                .flat_map(|f| f.steps)
                .map(PlanStepDto::from)
                .collect(),
            status,
            trace,
        }
    }
}

/// How a PLAN ended, as the response spells it.
fn plan_status_str(s: PlanStatus) -> &'static str {
    match s {
        PlanStatus::GoalReached => "goal_reached",
        PlanStatus::BudgetExhausted => "budget_exhausted",
        PlanStatus::NoPathFound => "no_path_found",
        PlanStatus::Cancelled => "cancelled",
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
    /// Ask for Brain's evidence-walk trace alongside the inferences: the base
    /// observation's whole candidate set, every edge the walk considered with
    /// the bucket that pruned it, the un-collapsed score components behind
    /// each confidence, and whether the topic centroid ran.
    ///
    /// Off by default and free when off. Mirrors `/v1/recall`'s
    /// `include_trace`.
    #[serde(default)]
    pub include_trace: Option<bool>,
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
            trace: self.include_trace.unwrap_or(false),
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
    /// How the walk ENDED: `complete`, `budget_exhausted`,
    /// `depth_limit_reached` or `cancelled`. A short chain that stopped at the
    /// depth limit is not the same answer as one that ran to completion, and
    /// the list of inferences cannot say which happened.
    pub status: Option<&'static str>,
    /// The evidence-walk trace, as a JSON **string**, when `include_trace`
    /// asked for one. See `PlanResponseDto::trace` for why it is a string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
}

impl ReasonResponseDto {
    /// Build from the streamed REASON frames.
    ///
    /// The inferences arrive spread across frames and are flattened in order;
    /// the terminal status and the trace ride the final frame alone.
    #[must_use]
    pub fn from_frames(frames: Vec<ReasonResponseFrame>) -> Self {
        let status = frames
            .iter()
            .rev()
            .find_map(|f| f.reason_status)
            .map(reason_status_str);
        let trace = frames
            .iter()
            .rev()
            .find_map(|f| f.trace.as_ref())
            .map(reason_trace_string);
        Self {
            inferences: frames
                .into_iter()
                .flat_map(|f| f.inferences)
                .map(InferenceStepDto::from)
                .collect(),
            status,
            trace,
        }
    }
}

/// How a REASON ended, as the response spells it.
fn reason_status_str(s: ReasonStatus) -> &'static str {
    match s {
        ReasonStatus::Complete => "complete",
        ReasonStatus::BudgetExhausted => "budget_exhausted",
        ReasonStatus::DepthLimitReached => "depth_limit_reached",
        ReasonStatus::Cancelled => "cancelled",
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
            include_trace: Some(true),
        };
        let req = body.to_request().unwrap();
        assert!(matches!(req.start, PlanState::ByText(ref t) if t == "at home"));
        assert!(matches!(req.goal, PlanState::ByMemoryId(42)));
        assert!(matches!(req.strategy_hint, Some(PlanStrategy::AStar)));
        assert_eq!(req.budget.max_steps, 12);
        assert_eq!(req.budget.max_wall_time_ms, 2_000);
        assert_eq!(req.budget.max_branches_explored, 16);
        assert!(req.trace, "include_trace must reach the wire request");
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
            include_trace: None,
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
            include_trace: None,
        };
        assert!(blank.to_request().is_err());

        let bad_strategy = PlanBody {
            start: text_endpoint("a"),
            goal: text_endpoint("b"),
            max_steps: 8,
            max_wall_time_ms: 5_000,
            max_branches: 32,
            strategy: Some("teleport".into()),
            include_trace: None,
        };
        assert!(bad_strategy.to_request().is_err());
    }

    fn plan_step(i: u32, id: u128, kind: TransitionKind) -> PlanStep {
        PlanStep {
            step_index: i,
            memory_id: id,
            text: "step".into(),
            transition_kind: kind,
            confidence: 1.0,
            estimated_distance_to_goal: 1.0,
        }
    }

    #[test]
    fn plan_response_flattens_frames_and_maps_transition_kinds() {
        // Two frames, as the engine streams them: the steps concatenate in
        // order and only the final frame carries the status.
        let frames = vec![
            PlanResponseFrame {
                steps: vec![plan_step(0, 7, TransitionKind::Initial)],
                is_final: false,
                plan_status: None,
                trace: None,
            },
            PlanResponseFrame {
                steps: vec![plan_step(1, 8, TransitionKind::Other("teleport".into()))],
                is_final: true,
                plan_status: Some(PlanStatus::GoalReached),
                trace: None,
            },
        ];
        let dto = PlanResponseDto::from_frames(frames);
        assert_eq!(dto.steps.len(), 2);
        assert_eq!(dto.steps[0].memory_id, "7");
        assert_eq!(dto.steps[0].transition_kind, "initial");
        assert_eq!(dto.steps[1].transition_kind, "teleport"); // Other passthrough
        assert_eq!(dto.status, Some("goal_reached"));
        assert!(dto.trace.is_none(), "a plan that did not ask carries none");
    }

    /// An empty path is three different outcomes, and the status is the only
    /// thing that tells them apart.
    #[test]
    fn an_empty_plan_still_reports_why_it_ended() {
        let dto = PlanResponseDto::from_frames(vec![PlanResponseFrame {
            steps: vec![],
            is_final: true,
            plan_status: Some(PlanStatus::BudgetExhausted),
            trace: None,
        }]);
        assert!(dto.steps.is_empty());
        assert_eq!(dto.status, Some("budget_exhausted"));
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
            include_trace: None,
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
                include_trace: None,
            }
            .to_request()
            .is_err()
        );
    }

    #[test]
    fn reason_response_maps_inferences_kinds_and_terminal_status() {
        let frames = vec![ReasonResponseFrame {
            inferences: vec![InferenceStep {
                step_index: 0,
                claim: "it rained".into(),
                supporting_memories: vec![1, 2],
                contradicting_memories: vec![3],
                confidence: 0.8,
                inference_kind: InferenceKind::CausalExplanation,
            }],
            is_final: true,
            reason_status: Some(ReasonStatus::DepthLimitReached),
            trace: None,
        }];
        let dto = ReasonResponseDto::from_frames(frames);
        assert_eq!(dto.inferences.len(), 1);
        assert_eq!(dto.inferences[0].claim, "it rained");
        assert_eq!(dto.inferences[0].supporting_memories, vec!["1", "2"]);
        assert_eq!(dto.inferences[0].contradicting_memories, vec!["3"]);
        assert_eq!(dto.inferences[0].inference_kind, "causal_explanation");
        // A chain that stopped at the depth limit is not a completed one.
        assert_eq!(dto.status, Some("depth_limit_reached"));
    }
}
