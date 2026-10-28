#[test]
fn public_custom_client_can_prepare_commit_invoke_and_archive_without_opaque_state() {
    use cola::{app::*, domain::*};
    use serde_json::json;
    use std::{
        cell::Cell,
        num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    };

    struct CustomClient(Cell<u32>);
    impl ModelClient<AssistantMessage> for CustomClient {
        fn prepare(
            &self,
            plan: &ContextPlan<AssistantMessage>,
            settings: &ModelSettings,
        ) -> Result<PreparedRequest<AssistantMessage>, ModelError> {
            let fitted = plan.fit(|selected| json!({
                "instructions": plan.instructions().as_str(),
                "selected": selected.history.iter().map(|group| group.user()).collect::<Vec<_>>(),
                "current": match plan.mandatory() { Mandatory::Assistant { user, .. } => user.as_str(), _ => unreachable!() },
            }))?;
            Ok(PreparedRequest::new(
                ModelTarget::new("custom", "memory"),
                settings.clone(),
                fitted,
            ))
        }
        fn invoke(
            &self,
            request: &PreparedRequest<AssistantMessage>,
        ) -> ObservedReply<AssistantMessage> {
            assert_eq!(request.body()["current"], "hello");
            self.0.set(self.0.get() + 1);
            ObservedReply::new(
                ResponseReceipt {
                    capture: ResponseCapture::Complete(b"{}".to_vec()),
                    ..ResponseReceipt::default()
                },
                Ok(Generation::Complete(
                    AssistantMessage::answer("custom reply").unwrap(),
                )),
            )
        }
    }
    let concrete = CustomClient(Cell::new(0));
    let client: &dyn ModelClient<AssistantMessage> = &concrete;
    let mut archive = SessionArchive::new(SessionId::new("custom-client").unwrap());
    let turn = archive
        .session_mut()
        .start_turn(UserMessage::new(NonEmptyText::new("hello").unwrap()))
        .unwrap();
    let limits = ContextLimits {
        input_tokens: NonZeroU64::new(10_000).unwrap(),
        request_bytes: NonZeroUsize::new(30_000).unwrap(),
    };
    let plan = ContextPlan::assistant(
        &archive,
        NonEmptyText::new("be helpful").unwrap(),
        vec![],
        limits,
        &[],
    )
    .unwrap();
    let settings = ModelSettings {
        model_id: ModelId::new("custom-model").unwrap(),
        effort: Effort::Default,
        max_output_tokens: NonZeroU32::new(100).unwrap(),
    };
    let prepared = client.prepare(&plan, &settings).unwrap();
    assert_eq!(concrete.0.get(), 0);
    let generation = archive
        .begin_generation(InvocationRole::Assistant { turn }, &prepared)
        .unwrap();
    assert_eq!(
        archive.generation(generation).unwrap().intent().request(),
        prepared.body()
    );
    let observed = client.invoke(&prepared);
    assert_eq!(concrete.0.get(), 1);
    let Generation::Complete(output) = observed.result.unwrap() else {
        panic!()
    };
    archive
        .session_mut()
        .record_assistant(turn, generation, output)
        .unwrap();
    archive
        .record_observation(
            generation,
            observed.at,
            observed.receipt,
            Disposition::Accepted,
        )
        .unwrap();
    let initial = ArchiveChange::initial(archive).unwrap();
    let decoded = SessionArchive::restore(initial.candidate().snapshot()).unwrap();
    assert_eq!(
        decoded.session().turns()[0].steps()[0].source_generation(),
        generation
    );
    assert_eq!(
        decoded
            .generation(generation)
            .unwrap()
            .intent()
            .target()
            .provider(),
        "custom"
    );
}
