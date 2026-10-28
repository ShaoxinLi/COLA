use cola::{app::*, domain::*};
use serde_json::{Value, json};
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};

fn text(value: impl Into<String>) -> NonEmptyText {
    NonEmptyText::new(value).unwrap()
}
fn user(value: impl Into<String>) -> UserMessage {
    UserMessage::new(text(value))
}
fn limits(bytes: usize, tokens: u64) -> ContextLimits {
    ContextLimits {
        input_tokens: NonZeroU64::new(tokens).unwrap(),
        request_bytes: NonZeroUsize::new(bytes).unwrap(),
    }
}
fn settings() -> ModelSettings {
    ModelSettings {
        model_id: ModelId::new("audit-model-sentinel").unwrap(),
        effort: Effort::High,
        max_output_tokens: NonZeroU32::new(1024).unwrap(),
    }
}
fn tool() -> ToolDefinition {
    ToolDefinition::new(
        ToolName::new("lookup").unwrap(),
        text("tool description"),
        InputSchema::new(json!({"type":"object","properties":{"n":{"type":"integer"}}})).unwrap(),
    )
}
fn archive() -> SessionArchive {
    ArchiveChange::initial(SessionArchive::new(
        SessionId::new("context fixture").unwrap(),
    ))
    .unwrap()
    .candidate()
    .clone()
}
fn publish(archive: &mut SessionArchive, candidate: SessionArchive) {
    *archive = ArchiveChange::next(archive, candidate)
        .unwrap()
        .candidate()
        .clone();
}
fn start(archive: &mut SessionArchive, value: impl Into<String>) -> TurnId {
    let mut candidate = archive.clone();
    let turn = candidate.session_mut().start_turn(user(value)).unwrap();
    publish(archive, candidate);
    turn
}
fn request<O>(plan: &ContextPlan<O>) -> PreparedRequest<O> {
    PreparedRequest::new(ModelTarget::new("fixture","memory://audit-endpoint"),settings(),plan.fit(|selection|json!({"instructions":plan.instructions().as_str(),"history":selection.history.iter().map(|group|json!({"user":group.user(),"record":group.record()})).collect::<Vec<_>>()})).unwrap())
}
fn observed() -> ResponseReceipt {
    ResponseReceipt {
        served_model: Some("audit-served-model-sentinel".into()),
        request_id: Some("audit-request-sentinel".into()),
        response_id: Some("audit-response-sentinel".into()),
        finish: Some("audit-finish-sentinel".into()),
        usage: Some(Usage {
            input_tokens: Some(987654),
            output_tokens: Some(123456),
        }),
        capture: ResponseCapture::Complete(b"audit-native-sentinel".to_vec()),
        ..Default::default()
    }
}
fn accept(archive: &mut SessionArchive, output: AssistantMessage) -> GenerationId {
    let turn = archive.session().active_turn().unwrap().id();
    let plan = ContextPlan::assistant(
        archive,
        text("instructions"),
        vec![tool()],
        limits(1_000_000, 1_000_000),
        &[],
    )
    .unwrap();
    let request = request(&plan);
    let mut candidate = archive.clone();
    let source = candidate
        .begin_generation(InvocationRole::Assistant { turn }, &request)
        .unwrap();
    publish(archive, candidate);
    let mut candidate = archive.clone();
    candidate
        .record_observation(source, Timestamp::now(), observed(), Disposition::Accepted)
        .unwrap();
    candidate
        .session_mut()
        .record_assistant(turn, source, output)
        .unwrap();
    publish(archive, candidate);
    source
}
fn calls(names: &[&str]) -> AssistantMessage {
    AssistantMessage::tool_requests(
        Some("Tool preamble".into()),
        ToolCalls::new(
            names
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    ToolCall::new(
                        ToolCallId::new(*name).unwrap(),
                        ToolName::new("lookup").unwrap(),
                        json!({"n":i+1}),
                    )
                    .unwrap()
                })
                .collect(),
        )
        .unwrap(),
    )
}
fn review(archive: &mut SessionArchive, call: &CallRef, allow: bool) {
    let turn = archive.session().active_turn().unwrap().id();
    let plan = ContextPlan::reviewer(
        archive,
        call,
        text("review rules"),
        vec![tool()],
        limits(1_000_000, 1_000_000),
        &[],
    )
    .unwrap();
    let request = request(&plan);
    let mut candidate = archive.clone();
    let generation = candidate
        .begin_generation(InvocationRole::Reviewer { call: call.clone() }, &request)
        .unwrap();
    publish(archive, candidate);
    let mut candidate = archive.clone();
    candidate
        .record_observation(
            generation,
            Timestamp::now(),
            observed(),
            Disposition::Accepted,
        )
        .unwrap();
    candidate
        .session_mut()
        .review_tool(
            turn,
            call,
            generation,
            &Decision::new(allow, Some(text("review reason"))),
        )
        .unwrap();
    publish(archive, candidate);
}
fn dispatch(archive: &mut SessionArchive, call: &CallRef, outcome: Option<ToolOutcome>) {
    let turn = archive.session().active_turn().unwrap().id();
    let mut candidate = archive.clone();
    candidate
        .session_mut()
        .start_tool(turn, call, Timestamp::now())
        .unwrap();
    publish(archive, candidate);
    if let Some(outcome) = outcome {
        let mut candidate = archive.clone();
        candidate
            .session_mut()
            .record_tool_result(turn, call, outcome, Timestamp::now())
            .unwrap();
        publish(archive, candidate);
    }
}
fn close(archive: &mut SessionArchive) {
    let turn = archive.session().active_turn().unwrap().id();
    let mut candidate = archive.clone();
    candidate
        .session_mut()
        .interrupt(turn, StopReason::ProcessLost)
        .unwrap();
    publish(archive, candidate);
}
fn plan(
    archive: &SessionArchive,
    bytes: usize,
    tokens: u64,
    required: &[TurnId],
) -> ContextPlan<AssistantMessage> {
    ContextPlan::assistant(
        archive,
        text("entire instructions"),
        vec![tool()],
        limits(bytes, tokens),
        required,
    )
    .unwrap()
}
fn render(plan: &ContextPlan<AssistantMessage>, selection: &Selection<'_>) -> Value {
    let Mandatory::Assistant { user, steps } = plan.mandatory() else {
        panic!("assistant plan")
    };
    json!({"instructions":plan.instructions().as_str(),"catalog":plan.tools().iter().map(|tool|json!({"name":tool.name().as_str(),"description":tool.description().as_str(),"schema":tool.input_schema().document()})).collect::<Vec<_>>(),"history":selection.history.iter().map(|group|json!({"user":group.user(),"record":group.record()})).collect::<Vec<_>>(),"notices":selection.notice.iter().collect::<Vec<_>>(),"mandatory":{"user":user,"native":steps.iter().map(|step|format!("{:?}",step)).collect::<Vec<_>>()}})
}
fn fit(plan: &ContextPlan<AssistantMessage>) -> PreparedRequest<AssistantMessage> {
    PreparedRequest::new(
        ModelTarget::new("fixture", "memory://audit-endpoint"),
        settings(),
        plan.fit(|selection| render(plan, selection)).unwrap(),
    )
}

#[test]
fn six_turn_selection_keeps_newest_whole_groups_in_chronological_order() {
    let mut archive = archive();
    for n in 1..=6 {
        start(&mut archive, format!("user {n}"));
        accept(
            &mut archive,
            AssistantMessage::answer(format!("answer {n}")).unwrap(),
        );
    }
    start(&mut archive, "active authorization");
    let full = plan(&archive, 1_000_000, 1_000_000, &[]);
    let expected = render(
        &full,
        &Selection {
            notice: Some(
                "Context note: 3 earlier turn(s) of this session are not included in this request."
                    .into(),
            ),
            history: full.history()[3..].iter().collect(),
        },
    );
    let cap = serde_json::to_vec(&expected).unwrap().len();
    let limited = plan(&archive, cap, 1_000_000, &[]);
    let fitted = fit(&limited);
    assert_eq!(fitted.body(), &expected);
    assert_eq!(
        fitted
            .selection()
            .included()
            .iter()
            .map(|id| id.get())
            .collect::<Vec<_>>(),
        vec![4, 5, 6]
    );
    assert_eq!(
        fitted
            .selection()
            .omitted()
            .iter()
            .map(|id| id.get())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(fitted.selection().required().is_empty());
    assert_eq!(fitted.selection().revision(), archive.revision());
    assert_eq!(fitted.selection().policy(), SELECTION_POLICY);
    assert_eq!(fitted.body()["notices"].as_array().unwrap().len(), 1);
    assert_eq!(fitted.budget().request_bytes(), cap);
    assert_eq!(fitted.budget().max_request_bytes(), cap);
    assert_eq!(
        fitted.budget().estimated_input_tokens(),
        (cap as u64).div_ceil(3)
    );
    assert_eq!(fitted.budget().method(), TOKEN_ESTIMATE_METHOD);
    assert_eq!(fit(&full).body()["notices"], json!([]));
}

#[test]
fn required_older_groups_survive_and_missing_required_turn_is_typed() {
    let mut archive = archive();
    for n in 1..=6 {
        start(&mut archive, format!("user {n}"));
        accept(
            &mut archive,
            AssistantMessage::answer(format!("answer {n}")).unwrap(),
        );
    }
    start(&mut archive, "active");
    let required = TurnId::new(1).unwrap();
    let full = plan(&archive, 1_000_000, 1_000_000, &[required]);
    let expected = render(
        &full,
        &Selection {
            notice: Some(
                "Context note: 3 earlier turn(s) of this session are not included in this request."
                    .into(),
            ),
            history: vec![&full.history()[0], &full.history()[4], &full.history()[5]],
        },
    );
    let cap = serde_json::to_vec(&expected).unwrap().len();
    let limited = plan(&archive, cap, 1_000_000, &[required]);
    let fitted = fit(&limited);
    assert_eq!(fitted.body(), &expected);
    assert_eq!(fitted.selection().required(), &[required]);
    assert_eq!(
        fitted
            .selection()
            .included()
            .iter()
            .map(|id| id.get())
            .collect::<Vec<_>>(),
        vec![1, 5, 6]
    );
    assert_eq!(
        fitted
            .selection()
            .omitted()
            .iter()
            .map(|id| id.get())
            .collect::<Vec<_>>(),
        vec![2, 3, 4]
    );
    assert_eq!(
        ContextPlan::assistant(
            &archive,
            text("rules"),
            vec![],
            limits(100, 100),
            &[TurnId::new(999).unwrap()]
        ),
        Err(SessionError::TurnNotFound)
    );
    let tiny = plan(&archive, 1, 1_000_000, &[required]);
    assert_eq!(
        tiny.fit(|selection| render(&tiny, selection))
            .unwrap_err()
            .kind(),
        ModelErrorKind::ContextCapacity
    );
}

#[test]
fn capacity_counts_serialized_instructions_catalog_framing_and_utf8_exactly() {
    let mut archive = archive();
    start(&mut archive, "中文字 🦀");
    let full = plan(&archive, 1_000_000, 1_000_000, &[]);
    let bytes = serde_json::to_vec(fit(&full).body()).unwrap().len();
    assert!(bytes > "中文字 🦀".len());
    let exact = plan(&archive, bytes, (bytes as u64).div_ceil(3), &[]);
    assert_eq!(fit(&exact).budget().request_bytes(), bytes);
    let byte_short = plan(&archive, bytes - 1, 1_000_000, &[]);
    assert_eq!(
        byte_short
            .fit(|selection| render(&byte_short, selection))
            .unwrap_err()
            .kind(),
        ModelErrorKind::ContextCapacity
    );
    let token_short = plan(&archive, 1_000_000, (bytes as u64).div_ceil(3) - 1, &[]);
    assert_eq!(
        token_short
            .fit(|selection| render(&token_short, selection))
            .unwrap_err()
            .kind(),
        ModelErrorKind::ContextCapacity
    );
    let huge = ContextPlan::assistant(
        &archive,
        text("rules".repeat(1000)),
        vec![tool()],
        limits(bytes, 1_000_000),
        &[],
    )
    .unwrap();
    assert_eq!(
        huge.fit(|selection| render(&huge, selection))
            .unwrap_err()
            .kind(),
        ModelErrorKind::ContextCapacity
    );
}

#[test]
fn portable_closed_facts_include_results_denial_unknowns_resolutions_and_nondispatch() {
    let mut archive = archive();
    let turn = start(&mut archive, "Read all files");
    let source = accept(
        &mut archive,
        calls(&[
            "audit-call-a",
            "audit-call-b",
            "audit-call-c",
            "audit-call-d",
            "audit-call-e",
        ]),
    );
    let call = |id| CallRef::new(source, ToolCallId::new(id).unwrap());
    review(&mut archive, &call("audit-call-a"), true);
    dispatch(
        &mut archive,
        &call("audit-call-a"),
        Some(ToolOutcome::Success(String::new())),
    );
    review(&mut archive, &call("audit-call-b"), false);
    review(&mut archive, &call("audit-call-c"), true);
    dispatch(
        &mut archive,
        &call("audit-call-c"),
        Some(ToolOutcome::Failure(ToolError::new(text(
            "genuine tool failure",
        )))),
    );
    review(&mut archive, &call("audit-call-d"), true);
    dispatch(&mut archive, &call("audit-call-d"), None);
    close(&mut archive);
    let mut candidate = archive.clone();
    candidate
        .session_mut()
        .resolve_effect(
            turn,
            &call("audit-call-d"),
            EffectResolution::new(
                EffectDisposition::Completed,
                ResolutionSource::Observation,
                text("file log confirms effect"),
            ),
        )
        .unwrap();
    publish(&mut archive, candidate);
    start(&mut archive, "approve but do not dispatch");
    let approved = accept(&mut archive, calls(&["audit-approved"]));
    let approved_call = CallRef::new(approved, ToolCallId::new("audit-approved").unwrap());
    review(&mut archive, &approved_call, true);
    close(&mut archive);
    start(&mut archive, "new request");
    let before = archive.clone();
    let context = plan(&archive, 1_000_000, 1_000_000, &[]);
    assert_eq!(archive, before);
    assert_eq!(context.history().len(), 2);
    let record = context.history()[0].record();
    let activity = record
        .strip_prefix("Tool preamble\n\nRecorded tool activity (history, not new tool results): ")
        .unwrap()
        .split_once("\n\nTurn ended: ")
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(activity.0).unwrap(),
        json!([
            {"tool":"lookup","arguments":{"n":1},"result":{"status":"returned","outcome":{"success":true,"output":""}}},
            {"tool":"lookup","arguments":{"n":2},"result":{"status":"not_executed","reason":"denied by review","review_reason":"review reason"}},
            {"tool":"lookup","arguments":{"n":3},"result":{"status":"returned","outcome":{"success":false,"error":"genuine tool failure"}}},
            {"tool":"lookup","arguments":{"n":4},"result":{"status":"unknown_effect","reason":"dispatched without a recorded result","resolution":{"disposition":"completed","source":"observation","evidence":"file log confirms effect"}}},
            {"tool":"lookup","arguments":{"n":5},"result":{"status":"not_executed","reason":"not reviewed or dispatched before the turn ended"}}
        ])
    );
    assert_eq!(
        serde_json::from_str::<Value>(activity.1).unwrap(),
        json!({"status":"interrupted","reason":"process_lost"})
    );
    assert!(
        context.history()[1]
            .record()
            .contains("approved but not dispatched before the turn ended")
    );
    for sentinel in [
        "audit-model-sentinel",
        "audit-served-model-sentinel",
        "audit-request-sentinel",
        "audit-response-sentinel",
        "audit-finish-sentinel",
        "audit-native-sentinel",
        "audit-call-",
        "audit-approved",
        "987654",
        "123456",
    ] {
        assert!(
            !context
                .history()
                .iter()
                .any(|group| group.record().contains(sentinel)),
            "metadata leaked {sentinel}"
        );
    }
    let Mandatory::Assistant { user, steps } = context.mandatory() else {
        panic!("assistant plan")
    };
    assert_eq!(user, "new request");
    assert!(steps.is_empty());
}

#[test]
fn active_bundle_is_indivisible_and_preserves_original_evidence_and_results() {
    let mut archive = archive();
    start(&mut archive, "active authorization");
    let source = accept(&mut archive, calls(&["original / Unicode 调用"]));
    let call = CallRef::new(source, ToolCallId::new("original / Unicode 调用").unwrap());
    review(&mut archive, &call, false);
    let bytes = archive.encode().unwrap();
    let cold = SessionArchive::decode(&bytes).unwrap();
    let context = plan(&cold, 1_000_000, 1_000_000, &[]);
    let Mandatory::Assistant { user, steps } = context.mandatory() else {
        panic!("assistant plan")
    };
    assert_eq!(user, "active authorization");
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].artifact().source(), source);
    assert_eq!(
        steps[0].artifact().capture(),
        &ResponseCapture::Complete(b"audit-native-sentinel".to_vec())
    );
    assert_eq!(
        steps[0].artifact().target(),
        &ModelTarget::new("fixture", "memory://audit-endpoint")
    );
    assert_eq!(steps[0].artifact().settings(), &settings());
    assert_eq!(steps[0].results().len(), 1);
    assert_eq!(steps[0].results()[0].call_id(), call.call_id());
    assert_eq!(
        steps[0].results()[0].outcome(),
        &ToolOutcome::Failure(ToolError::new(text(
            "Not executed: tool call denied by review. Reason: review reason"
        )))
    );
    let tiny = plan(&cold, 1, 1_000_000, &[]);
    assert_eq!(
        tiny.fit(|selection| render(&tiny, selection))
            .unwrap_err()
            .kind(),
        ModelErrorKind::ContextCapacity
    );
    assert_eq!(cold.encode().unwrap(), bytes);
}

#[test]
fn reviewer_keeps_exact_candidate_authorization_and_current_execution_facts() {
    let mut archive = archive();
    start(&mut archive, "Read only approved files  \n");
    let source = accept(&mut archive, calls(&["first", "second"]));
    let first = CallRef::new(source, ToolCallId::new("first").unwrap());
    review(&mut archive, &first, true);
    dispatch(
        &mut archive,
        &first,
        Some(ToolOutcome::Success("actual result".into())),
    );
    let second = CallRef::new(source, ToolCallId::new("second").unwrap());
    let before = archive.clone();
    let context = ContextPlan::reviewer(
        &archive,
        &second,
        text("review rules"),
        vec![tool()],
        limits(1_000_000, 1_000_000),
        &[],
    )
    .unwrap();
    assert_eq!(archive, before);
    let Mandatory::Reviewer { facts } = context.mandatory() else {
        panic!("review plan")
    };
    let facts: Value = serde_json::from_str(facts.split_once('\n').unwrap().1).unwrap();
    assert_eq!(
        facts["candidate"],
        json!({"id":"second","name":"lookup","arguments":{"n":2}})
    );
    assert_eq!(facts["active_turn"]["user"], "Read only approved files  \n");
    assert_eq!(
        facts["active_turn"]["steps"][0]["executions"],
        json!([{"call_id":"first","state":{"kind":"returned","outcome":{"kind":"success","text":"actual result"}}},{"call_id":"second","state":{"kind":"requested"}}])
    );
    assert_eq!(context.tools(), &[tool()]);
    let wrong = CallRef::new(source, ToolCallId::new("absent").unwrap());
    assert_eq!(
        ContextPlan::reviewer(
            &archive,
            &wrong,
            text("rules"),
            vec![],
            limits(100, 100),
            &[]
        ),
        Err(SessionError::UnknownCall)
    );
    assert_eq!(
        ContextPlan::assistant(&archive, text("rules"), vec![], limits(100, 100), &[]),
        Err(SessionError::NotAwaitingModel)
    );
}

#[test]
fn dangling_active_generation_is_rejected_as_a_typed_context_error() {
    let mut archive = archive();
    let turn = start(&mut archive, "active");
    let source = GenerationId::new(99).unwrap();
    let call = CallRef::new(source, ToolCallId::new("missing source").unwrap());
    archive
        .session_mut()
        .record_assistant(turn, source, calls(&["missing source"]))
        .unwrap();
    archive
        .session_mut()
        .review_tool(
            turn,
            &call,
            GenerationId::new(100).unwrap(),
            &Decision::new(false, None),
        )
        .unwrap();
    assert_eq!(
        ContextPlan::assistant(&archive, text("rules"), vec![], limits(1000, 1000), &[]),
        Err(SessionError::InvalidGeneration(source))
    );
}

#[test]
fn full_history_can_fit_when_an_omission_notice_would_exceed_capacity() {
    let mut archive = archive();
    start(&mut archive, "a");
    accept(&mut archive, AssistantMessage::answer("b").unwrap());
    start(&mut archive, "a");
    let plan = ContextPlan::assistant(&archive, text("role"), vec![], limits(20, 20), &[]).unwrap();
    let fitted = plan
        .fit(|selection| {
            if selection.notice.is_some() {
                json!({"notice":"a notice that exceeds the entire byte budget"})
            } else {
                json!({"turns":selection.history.len()})
            }
        })
        .unwrap();
    let request: PreparedRequest<AssistantMessage> =
        PreparedRequest::new(ModelTarget::new("fixture", "memory"), settings(), fitted);
    assert_eq!(request.body(), &json!({"turns":1}));
    assert_eq!(request.selection().included().len(), 1);
    assert!(request.selection().omitted().is_empty());
}
