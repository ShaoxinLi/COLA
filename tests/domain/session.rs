use cola::domain::*;
use serde_json::json;
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
fn text(value: &str) -> NonEmptyText {
    NonEmptyText::new(value).unwrap()
}
fn request(archive: &SessionArchive) -> PreparedRequest<AssistantMessage> {
    let limits = ContextLimits {
        input_tokens: NonZeroU64::new(1000).unwrap(),
        request_bytes: NonZeroUsize::new(3000).unwrap(),
    };
    let plan = ContextPlan::assistant(archive, text("instructions"), vec![], limits, &[]).unwrap();
    PreparedRequest::new(
        ModelTarget::new("custom", "memory"),
        ModelSettings {
            model_id: ModelId::new("custom").unwrap(),
            effort: Effort::Off,
            max_output_tokens: NonZeroU32::new(1).unwrap(),
        },
        plan.fit(|_| json!({"exact":"body"})).unwrap(),
    )
}
fn answered() -> SessionArchive {
    let mut draft = SessionArchive::new(SessionId::new("external").unwrap());
    let turn = draft
        .session_mut()
        .start_turn(UserMessage::new(text("first")))
        .unwrap();
    let request = request(&draft);
    let generation = draft
        .begin_generation(InvocationRole::Assistant { turn }, &request)
        .unwrap();
    draft
        .record_observation(
            generation,
            Timestamp::now(),
            ResponseReceipt {
                capture: ResponseCapture::Complete(b"{}".to_vec()),
                ..ResponseReceipt::default()
            },
            Disposition::Accepted,
        )
        .unwrap();
    draft
        .session_mut()
        .record_assistant(
            turn,
            generation,
            AssistantMessage::answer("first answer").unwrap(),
        )
        .unwrap();
    ArchiveChange::initial(draft).unwrap().candidate().clone()
}

#[test]
fn candidate_cannot_rewrite_committed_outputs_requests_or_native_evidence() {
    let base = answered();
    let mut variants = Vec::new();
    let mut snapshot = base.snapshot();
    snapshot.session.turns[0].steps[0].assistant =
        AssistantSnapshot::Answer("rewritten answer".into());
    variants.push(snapshot);
    let mut snapshot = base.snapshot();
    snapshot.session.turns[0].user = "other".into();
    variants.push(snapshot);
    let mut snapshot = base.snapshot();
    let Observation::Observed { receipt, .. } = &mut snapshot.generations[0].observation else {
        panic!()
    };
    receipt.capture = ResponseCapture::Complete(b"[]".to_vec());
    variants.push(snapshot);
    let mut snapshot = base.snapshot();
    snapshot.generations[0].request = json!({"exact":"else"});
    variants.push(snapshot);
    for snapshot in variants {
        assert_ne!(snapshot, base.snapshot());
        let candidate = SessionArchive::restore(snapshot).unwrap();
        assert!(ArchiveChange::next(&base, candidate).is_err());
    }
}
#[test]
fn corrupt_revision_and_source_links_are_rejected_before_persistence() {
    let base = answered();
    let mut variants = Vec::new();
    let mut snapshot = base.snapshot();
    snapshot.revision = 0;
    variants.push(snapshot);
    let mut snapshot = base.snapshot();
    snapshot.session.turns[0].steps[0].source_generation = 2;
    variants.push(snapshot);
    let mut snapshot = base.snapshot();
    snapshot.generations[0].role = InvocationRole::Assistant {
        turn: TurnId::new(2).unwrap(),
    };
    variants.push(snapshot);
    for snapshot in variants {
        assert_ne!(snapshot, base.snapshot());
        assert!(SessionArchive::restore(snapshot).is_err());
    }
    assert!(ArchiveChange::initial(base.clone()).is_err());
    assert!(ArchiveChange::next(&base, SessionArchive::new(base.session().id().clone())).is_err());
}

#[test]
fn committed_admission_distinguishes_drafts_and_rejects_invalid_bases_and_overflow() {
    let base = answered();
    let mut draft = base.clone();
    let turn = draft
        .session_mut()
        .start_turn(UserMessage::new(text("next")))
        .unwrap();
    let prepared = request(&draft);
    draft
        .begin_generation(InvocationRole::Assistant { turn }, &prepared)
        .unwrap();
    assert!(draft.validate_committed().is_err());
    assert!(SessionArchive::restore(draft.snapshot()).is_err());
    let change = ArchiveChange::next(&base, draft).unwrap();
    change.candidate().validate_committed().unwrap();
    assert_eq!(change.base(), Some(&base));
    assert_eq!(
        SessionArchive::restore(change.candidate().snapshot()).unwrap(),
        *change.candidate()
    );

    let mut invalid_base = base.clone();
    let turn = invalid_base
        .session_mut()
        .start_turn(UserMessage::new(text("missing evidence")))
        .unwrap();
    invalid_base
        .session_mut()
        .record_assistant(
            turn,
            GenerationId::new(2).unwrap(),
            AssistantMessage::answer("unlinked").unwrap(),
        )
        .unwrap();
    assert!(ArchiveChange::next(&invalid_base, invalid_base.clone()).is_err());

    let mut snapshot = base.snapshot();
    snapshot.revision = u64::MAX;
    let exhausted = SessionArchive::restore(snapshot).unwrap();
    assert!(ArchiveChange::next(&exhausted, exhausted.clone()).is_err());
}
