use super::{
    AssistantMessage, EffectResolution, ExecutionState, Message, NonEmptyText, Session, ToolCall,
    ToolCallId, ToolDefinition, ToolName, ToolResult, TurnId, TurnOutcome,
};
use std::{collections::BTreeSet, error::Error, fmt, marker::PhantomData};

/// Domain-derived facts, separate from user, assistant, and tool messages.
/// An unknown effect stays unknown in the original history after reconciliation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionNotice {
    TurnClosed {
        turn_id: TurnId,
        outcome: TurnOutcome,
    },
    Undispatched {
        turn_id: TurnId,
        call: ToolCall,
    },
    UnknownEffect {
        turn_id: TurnId,
        call: ToolCall,
        resolution: Option<EffectResolution>,
    },
    ConfirmedResult {
        turn_id: TurnId,
        call: ToolCall,
        result: ToolResult,
    },
}
impl ExecutionNotice {
    fn call(&self) -> Option<&ToolCall> {
        match self {
            Self::TurnClosed { .. } => None,
            Self::Undispatched { call, .. }
            | Self::UnknownEffect { call, .. }
            | Self::ConfirmedResult { call, .. } => Some(call),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextError {
    MissingUser,
    InvalidMessageOrder,
    UnresolvedToolBatch,
    MismatchedToolResult,
    DuplicateCallId(ToolCallId),
    DuplicateToolName(ToolName),
    InvalidOutcome,
    InvalidTurnOrder,
    ActiveExchangeNotLast,
}
impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingUser => f.write_str("an exchange must start with one user message"),
            Self::InvalidMessageOrder => {
                f.write_str("messages do not follow user, assistant, and ordered tool returns")
            }
            Self::UnresolvedToolBatch => {
                f.write_str("model input cannot contain an unresolved tool batch")
            }
            Self::MismatchedToolResult => {
                f.write_str("tool result does not match the next requested call")
            }
            Self::DuplicateCallId(id) => write!(f, "duplicate tool call ID {id} in context"),
            Self::DuplicateToolName(name) => {
                write!(f, "duplicate tool name {name} in context catalog")
            }
            Self::InvalidOutcome => {
                f.write_str("exchange outcome disagrees with its final message")
            }
            Self::InvalidTurnOrder => f.write_str("context turn IDs must be strictly increasing"),
            Self::ActiveExchangeNotLast => {
                f.write_str("only the last context exchange can be active")
            }
        }
    }
}
impl Error for ContextError {}

/// A checked per-turn message exchange. Interrupted boundaries remain explicit,
/// even when the last confirmed batch has no subsequent assistant answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextExchange {
    turn_id: TurnId,
    messages: Vec<Message>,
    outcome: Option<TurnOutcome>,
    notices: Vec<ExecutionNotice>,
}
impl ContextExchange {
    /// Accepts complete tool batches only. An active exchange must await the model.
    /// A completed exchange must end in an answer. Interrupted and failed exchanges
    /// must not contain an answer and receive a separate closure notice.
    pub fn new(
        turn_id: TurnId,
        messages: Vec<Message>,
        outcome: Option<TurnOutcome>,
    ) -> Result<Self, ContextError> {
        validate_messages(&messages, outcome.as_ref())?;
        let notices = match &outcome {
            Some(outcome @ (TurnOutcome::Interrupted(_) | TurnOutcome::Failed(_))) => {
                vec![ExecutionNotice::TurnClosed {
                    turn_id,
                    outcome: outcome.clone(),
                }]
            }
            _ => vec![],
        };
        Ok(Self {
            turn_id,
            messages,
            outcome,
            notices,
        })
    }
    pub fn turn_id(&self) -> TurnId {
        self.turn_id
    }
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
    pub fn outcome(&self) -> Option<&TurnOutcome> {
        self.outcome.as_ref()
    }
    pub fn notices(&self) -> &[ExecutionNotice] {
        &self.notices
    }
}

/// Immutable model input data. This value performs no history selection, token
/// measurement, persistence, prompt rendering, execution, or fitting policy.
/// The output type associates input with the value an external model should produce.
///
/// ```compile_fail
/// use cola::domain::{AssistantMessage, Context, Decision};
/// fn assistant_input(_: Context<AssistantMessage>) {}
/// fn review_input(context: Context<Decision>) {
///     assistant_input(context);
/// }
/// ```
pub struct Context<O> {
    instructions: NonEmptyText,
    exchanges: Vec<ContextExchange>,
    tools: Vec<ToolDefinition>,
    output: PhantomData<fn() -> O>,
}
impl<O> Context<O> {
    pub fn new(
        instructions: NonEmptyText,
        exchanges: Vec<ContextExchange>,
        tools: Vec<ToolDefinition>,
    ) -> Result<Self, ContextError> {
        let mut names = BTreeSet::new();
        for tool in &tools {
            if !names.insert(tool.name()) {
                return Err(ContextError::DuplicateToolName(tool.name().clone()));
            }
        }
        let mut ids = BTreeSet::new();
        let mut previous_turn = None;
        for (index, exchange) in exchanges.iter().enumerate() {
            if previous_turn.is_some_and(|previous| previous >= exchange.turn_id) {
                return Err(ContextError::InvalidTurnOrder);
            }
            previous_turn = Some(exchange.turn_id);
            if exchange.outcome.is_none() && index + 1 != exchanges.len() {
                return Err(ContextError::ActiveExchangeNotLast);
            }
            for id in exchange
                .messages
                .iter()
                .filter_map(|message| match message {
                    Message::Assistant(assistant) => assistant.calls(),
                    _ => None,
                })
                .flat_map(|calls| calls.as_slice())
                .map(ToolCall::id)
                .chain(
                    exchange
                        .notices
                        .iter()
                        .filter_map(ExecutionNotice::call)
                        .map(ToolCall::id),
                )
            {
                if !ids.insert(id) {
                    return Err(ContextError::DuplicateCallId(id.clone()));
                }
            }
        }
        Ok(Self {
            instructions,
            exchanges,
            tools,
            output: PhantomData,
        })
    }
    pub fn instructions(&self) -> &NonEmptyText {
        &self.instructions
    }
    pub fn exchanges(&self) -> &[ContextExchange] {
        &self.exchanges
    }
    pub fn tools(&self) -> &[ToolDefinition] {
        &self.tools
    }
    pub fn messages(&self) -> impl Iterator<Item = &Message> {
        self.exchanges
            .iter()
            .flat_map(|exchange| &exchange.messages)
    }
    pub fn execution_notices(&self) -> impl Iterator<Item = &ExecutionNotice> {
        self.exchanges.iter().flat_map(|exchange| &exchange.notices)
    }
}

impl<O> Clone for Context<O> {
    fn clone(&self) -> Self {
        Self {
            instructions: self.instructions.clone(),
            exchanges: self.exchanges.clone(),
            tools: self.tools.clone(),
            output: PhantomData,
        }
    }
}
impl<O> fmt::Debug for Context<O> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("instructions", &self.instructions)
            .field("exchanges", &self.exchanges)
            .field("tools", &self.tools)
            .finish()
    }
}
impl<O> PartialEq for Context<O> {
    fn eq(&self, other: &Self) -> bool {
        self.instructions == other.instructions
            && self.exchanges == other.exchanges
            && self.tools == other.tools
    }
}
impl<O> Eq for Context<O> {}

fn validate_messages(
    messages: &[Message],
    outcome: Option<&TurnOutcome>,
) -> Result<(), ContextError> {
    if !matches!(messages.first(), Some(Message::User(_))) {
        return Err(ContextError::MissingUser);
    }
    let mut pending = Vec::new();
    let mut next_result = 0;
    let mut answered = false;
    let mut ids = BTreeSet::new();
    for message in &messages[1..] {
        if answered {
            return Err(ContextError::InvalidMessageOrder);
        }
        if next_result < pending.len() {
            match message {
                Message::Tool(result) if result.call_id() == pending[next_result] => {
                    next_result += 1
                }
                Message::Tool(_) => return Err(ContextError::MismatchedToolResult),
                _ => return Err(ContextError::InvalidMessageOrder),
            }
        } else {
            match message {
                Message::Assistant(AssistantMessage::Answer(_)) => answered = true,
                Message::Assistant(AssistantMessage::ToolRequests { calls, .. }) => {
                    pending = calls.as_slice().iter().map(ToolCall::id).collect();
                    next_result = 0;
                    for id in &pending {
                        if !ids.insert(*id) {
                            return Err(ContextError::DuplicateCallId((*id).clone()));
                        }
                    }
                }
                _ => return Err(ContextError::InvalidMessageOrder),
            }
        }
    }
    if next_result < pending.len() {
        return Err(ContextError::UnresolvedToolBatch);
    }
    if answered != matches!(outcome, Some(TurnOutcome::Completed)) {
        return Err(ContextError::InvalidOutcome);
    }
    Ok(())
}

impl Session {
    /// Projects exchanges with complete tool batches and deterministic factual notices.
    /// For a closed partial batch, confirmed returns, undispatched calls, unknown
    /// effects, and reconciliations become notices instead of incomplete messages.
    /// Active work must already await the model. Every turn is included.
    pub fn context_exchanges(&self) -> Result<Vec<ContextExchange>, ContextError> {
        let mut exchanges = self.closed_context_exchanges()?;
        if let Some(turn) = self.active_turn() {
            exchanges.push(ContextExchange::new(turn.id(), turn.messages(), None)?);
        }
        Ok(exchanges)
    }

    /// Projects closed turns with their recovery notices, excluding all active-turn messages.
    /// This projection is available while an active tool batch is unresolved.
    pub fn closed_context_exchanges(&self) -> Result<Vec<ContextExchange>, ContextError> {
        let mut exchanges = vec![];
        for turn in self.turns() {
            let mut messages = vec![Message::User(turn.user().clone())];
            let mut partial_notices = vec![];
            for step in turn.steps() {
                if step.all_returned() {
                    messages.extend(step.messages());
                } else {
                    for execution in step.executions() {
                        let call = execution.call().clone();
                        partial_notices.push(match execution.state() {
                            ExecutionState::NotStarted => ExecutionNotice::Undispatched {
                                turn_id: turn.id(),
                                call,
                            },
                            ExecutionState::Started => ExecutionNotice::UnknownEffect {
                                turn_id: turn.id(),
                                call,
                                resolution: execution.resolution().cloned(),
                            },
                            ExecutionState::Returned(outcome) => ExecutionNotice::ConfirmedResult {
                                turn_id: turn.id(),
                                result: ToolResult::new(call.id().clone(), outcome.clone()),
                                call,
                            },
                        });
                    }
                }
            }
            let mut exchange =
                ContextExchange::new(turn.id(), messages, Some(turn.outcome().clone()))?;
            exchange.notices.extend(partial_notices);
            exchanges.push(exchange);
        }
        Ok(exchanges)
    }
}
