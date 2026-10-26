use super::{OutputKind, Provider, error};
use crate::{
    app::{ModelError, ModelErrorKind, ModelSettings},
    domain::{
        AssistantMessage, Context, ContextExchange, ExecutionNotice, Message, NonEmptyText,
        ToolCall, ToolDefinition, TurnId, TurnOutcome,
    },
};
use serde_json::Value;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Snapshot {
    pub instructions: NonEmptyText,
    pub exchanges: Vec<ContextExchange>,
    pub tools: Vec<ToolDefinition>,
}
impl Snapshot {
    pub fn of<O>(context: &Context<O>) -> Self {
        Self {
            instructions: context.instructions().clone(),
            exchanges: context.exchanges().to_vec(),
            tools: context.tools().to_vec(),
        }
    }
}
#[derive(Clone)]
pub(super) struct NativeCall {
    pub call: ToolCall,
    pub id: Option<String>,
    pub position: usize,
}
#[derive(Clone)]
pub(super) struct Record {
    pub turn: TurnId,
    pub index: usize,
    pub raw: Vec<Value>,
    pub calls: Vec<NativeCall>,
}
#[derive(Clone)]
pub(super) struct Replay {
    pub(super) provider: Provider,
    pub(super) endpoint: String,
    pub(super) kind: OutputKind,
    pub(super) settings: ModelSettings,
    pub(super) snapshot: Snapshot,
    pub(super) receipt: Option<AssistantMessage>,
    pub(super) records: Vec<Record>,
}
impl Replay {
    pub(super) fn check(
        &self,
        provider: Provider,
        endpoint: &str,
        kind: OutputKind,
        settings: &ModelSettings,
        current: &Snapshot,
    ) -> Result<(), ModelError> {
        if self.provider != provider
            || self.endpoint != endpoint
            || self.kind != kind
            || &self.settings != settings
            || self.snapshot.instructions != current.instructions
            || self.snapshot.tools != current.tools
        {
            return Err(error(ModelErrorKind::ContinuationMismatch));
        }
        if !self.accepts_exact_output_and_next_input(current) {
            return Err(error(ModelErrorKind::ContinuationMismatch));
        }
        Ok(())
    }
    fn accepts_exact_output_and_next_input(&self, current: &Snapshot) -> bool {
        let Some(output) = &self.receipt else {
            return self.snapshot == *current;
        };
        let old = &self.snapshot.exchanges;
        let Some(last) = old.last() else {
            return false;
        };
        if last.outcome().is_some()
            || current.exchanges.len() < old.len()
            || old[..old.len() - 1] != current.exchanges[..old.len() - 1]
        {
            return false;
        }
        let accepted = &current.exchanges[old.len() - 1];
        if accepted.turn_id() != last.turn_id() || !accepted.messages().starts_with(last.messages())
        {
            return false;
        }
        let suffix = &accepted.messages()[last.messages().len()..];
        if suffix.first() != Some(&Message::Assistant(output.clone())) {
            return false;
        }
        match output {
            AssistantMessage::Answer(_) => {
                if suffix.len() != 1
                    || accepted.outcome() != Some(&TurnOutcome::Completed)
                    || accepted.notices() != last.notices()
                {
                    return false;
                }
            }
            AssistantMessage::ToolRequests { calls, .. } => {
                if suffix.len() != calls.as_slice().len() + 1 {
                    return false;
                }
                for (message, call) in suffix[1..].iter().zip(calls.as_slice()) {
                    if !matches!(message, Message::Tool(result) if result.call_id() == call.id()) {
                        return false;
                    }
                }
                if accepted.outcome().is_none() {
                    if accepted.notices() != last.notices() {
                        return false;
                    }
                } else if !has_exact_closure_notice(accepted) {
                    return false;
                }
            }
        }
        let newly_admitted_turns = &current.exchanges[old.len()..];
        if !newly_admitted_turns.is_empty() && accepted.outcome().is_none() {
            return false;
        }
        let mut previous = accepted.turn_id();
        for (index, exchange) in newly_admitted_turns.iter().enumerate() {
            if previous.get().checked_add(1) != Some(exchange.turn_id().get())
                || !matches!(exchange.messages(), [Message::User(_)])
            {
                return false;
            }
            if exchange.outcome().is_none() {
                if index + 1 != newly_admitted_turns.len() || !exchange.notices().is_empty() {
                    return false;
                }
            } else if !has_exact_closure_notice(exchange) {
                return false;
            }
            previous = exchange.turn_id();
        }
        true
    }
}

fn has_exact_closure_notice(exchange: &ContextExchange) -> bool {
    let Some(outcome @ (TurnOutcome::Interrupted(_) | TurnOutcome::Failed(_))) = exchange.outcome()
    else {
        return false;
    };
    matches!(exchange.notices(), [ExecutionNotice::TurnClosed { turn_id, outcome: notice_outcome }]
        if *turn_id == exchange.turn_id() && notice_outcome == outcome)
}

pub(super) fn require_history(
    snapshot: &Snapshot,
    replay: Option<&Replay>,
) -> Result<(), ModelError> {
    if replay.is_none()
        && snapshot.exchanges.iter().any(|exchange| {
            exchange
                .messages()
                .iter()
                .any(|message| matches!(message, Message::Assistant(a) if a.calls().is_some()))
        })
    {
        return Err(error(ModelErrorKind::MissingContinuation));
    }
    Ok(())
}
