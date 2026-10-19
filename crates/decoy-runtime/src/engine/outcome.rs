use crate::collections::Activation;
use crate::matching::ConditionFailure;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissDiagnostic {
    pub collection: String,
    pub reason: String,
    /// Every active route entry evaluated, bottom-to-top, with why it was rejected.
    pub candidates: Vec<Candidate>,
}

/// One evaluated active route entry and why it did not match.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Candidate {
    pub address: String,
    #[serde(flatten)]
    pub rejection: Rejection,
}

/// Why a Candidate was rejected. `method`/`path` hold the Route's value; the request's is known.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "reason", rename_all = "camelCase")]
pub enum Rejection {
    MissingRoute,
    MissingCase,
    MissingBehavior,
    /// The Route serves another transport (an HTTP request against a gRPC Route, or back).
    Transport,
    Method {
        expected: String,
    },
    Path {
        expected: String,
    },
    Condition(ConditionFailure),
    /// The request did not decode with the Route's Pipeline.
    Decode {
        error: String,
    },
}

/// Records why `activation` was rejected when explaining (`Some`); the fast path builds nothing.
pub(crate) fn reject(
    candidates: &mut Option<Vec<Candidate>>,
    activation: &Activation,
    rejection: impl FnOnce() -> Rejection,
) {
    if let Some(candidates) = candidates {
        candidates.push(Candidate {
            address: activation.address(),
            rejection: rejection(),
        });
    }
}
