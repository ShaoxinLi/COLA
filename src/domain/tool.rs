use super::{NonEmptyText, ToolCallId, ToolName, ValueError};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

/// Synchronous execution implemented outside the domain library.
///
/// Implementations validate tool-specific argument fields and return ordinary
/// failures as [`ToolOutcome::Failure`]. Execution can block the caller.
/// The caller selects the implementation and records its outcome through
/// `Session::record_tool_result` with the complete `CallAddress`.
pub trait Tool {
    fn execute(&self, arguments: &Map<String, Value>) -> ToolOutcome;
}

/// A JSON Schema descriptor with an object root and `"type": "object"`.
///
/// Only those two structural conditions are checked. Other keywords, references,
/// schema dialects, and tool argument conformance are not compiled or validated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputSchema(Map<String, Value>);
impl InputSchema {
    pub fn new(document: Value) -> Result<Self, ValueError> {
        let Value::Object(document) = document else {
            return Err(ValueError::InvalidInputSchema);
        };
        if document.get("type").and_then(Value::as_str) != Some("object") {
            return Err(ValueError::InvalidInputSchema);
        }
        Ok(Self(document))
    }
    pub fn document(&self) -> &Map<String, Value> {
        &self.0
    }
}

/// Model-visible data. This value contains no executable behavior.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolDefinition {
    name: ToolName,
    description: NonEmptyText,
    input_schema: InputSchema,
}
impl ToolDefinition {
    pub fn new(name: ToolName, description: NonEmptyText, input_schema: InputSchema) -> Self {
        Self {
            name,
            description,
            input_schema,
        }
    }
    pub fn name(&self) -> &ToolName {
        &self.name
    }
    pub fn description(&self) -> &NonEmptyText {
        &self.description
    }
    pub fn input_schema(&self) -> &InputSchema {
        &self.input_schema
    }
}

/// A requested invocation. The ID correlates facts; it does not ensure exactly-once effects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    id: ToolCallId,
    name: ToolName,
    arguments: Map<String, Value>,
}
impl ToolCall {
    pub fn new(id: ToolCallId, name: ToolName, arguments: Value) -> Result<Self, ValueError> {
        let Value::Object(arguments) = arguments else {
            return Err(ValueError::ExpectedObject);
        };
        Ok(Self {
            id,
            name,
            arguments,
        })
    }
    pub fn id(&self) -> &ToolCallId {
        &self.id
    }
    pub fn name(&self) -> &ToolName {
        &self.name
    }
    pub fn arguments(&self) -> &Map<String, Value> {
        &self.arguments
    }
}

/// A nonempty batch whose call IDs are unique within the batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCalls(Vec<ToolCall>);
impl ToolCalls {
    pub fn new(calls: Vec<ToolCall>) -> Result<Self, ValueError> {
        if calls.is_empty() {
            return Err(ValueError::EmptyToolCalls);
        }
        let mut ids = BTreeSet::new();
        for call in &calls {
            if !ids.insert(call.id()) {
                return Err(ValueError::DuplicateToolCallId(call.id().clone()));
            }
        }
        Ok(Self(calls))
    }
    pub fn as_slice(&self) -> &[ToolCall] {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolError(NonEmptyText);
impl ToolError {
    pub fn new(description: NonEmptyText) -> Self {
        Self(description)
    }
    pub fn description(&self) -> &NonEmptyText {
        &self.0
    }
}

/// Empty success output is valid. A tool failure is a normal recorded return.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolOutcome {
    Success(String),
    Failure(ToolError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolResult {
    call_id: ToolCallId,
    outcome: ToolOutcome,
}
impl ToolResult {
    pub fn new(call_id: ToolCallId, outcome: ToolOutcome) -> Self {
        Self { call_id, outcome }
    }
    pub fn call_id(&self) -> &ToolCallId {
        &self.call_id
    }
    pub fn outcome(&self) -> &ToolOutcome {
        &self.outcome
    }
}
