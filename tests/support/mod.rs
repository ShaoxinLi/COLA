use cola::app::*;

pub fn prepared<O>(input: &ModelInput<'_, O>, route: &str, assistant: bool) -> PreparedInvocation {
    let artifact = ModelArtifact {
        format: "fixture".into(),
        version: 1,
        bytes: b"{}".to_vec(),
    };
    let selection = input
        .selection
        .closed_turns
        .iter()
        .map(|id| id.get())
        .collect::<Vec<_>>();
    let view = if assistant && input.view.is_none() {
        Some(TurnView {
            turn: input.context.exchanges().last().unwrap().turn_id().get(),
            selection: selection.clone(),
            policy_version: 1,
            frozen_prefix: artifact.clone(),
            requested: RequestedModel {
                route: route.into(),
                settings: input.settings.clone(),
            },
        })
    } else {
        None
    };
    PreparedInvocation {
        request: RequestEvidence {
            selection,
            policy_version: 1,
            renderer_version: 1,
            body: artifact,
            started_at: now_millis(),
        },
        view,
    }
}
pub fn observed<O>(generation: Result<Generation<O>, ModelError>) -> Observation<O> {
    let mut evidence = ResponseEvidence::not_returned();
    evidence.response = Some(ModelArtifact {
        format: "fixture".into(),
        version: 1,
        bytes: b"{}".to_vec(),
    });
    evidence.capture = CaptureCompleteness::Complete;
    Observation {
        generation: generation.unwrap_or_else(Generation::Failed),
        evidence,
    }
}
