use super::{Decoded, OutputKind, Provider, anthropic, error, malformed, openai, replay};
use super::{
    output::Output,
    replay::{NativeCall, Record, Replay, Snapshot},
    transport::{ClientConfig, Transport},
};
use crate::{
    app::{Continuation, Effort, Generation, ModelError, ModelErrorKind, ModelSettings},
    domain::{Context, ToolCall, ToolCallId, ToolName},
};

#[derive(Clone)]
pub(super) struct Engine {
    provider: Provider,
    transport: Transport,
}
impl Engine {
    pub(super) fn new(provider: Provider, config: ClientConfig) -> Result<Self, ModelError> {
        Ok(Self {
            provider,
            transport: Transport::new(config)?,
        })
    }
    pub(super) fn generate<O: Output>(
        &self,
        context: &Context<O>,
        settings: &ModelSettings,
        continuation: Option<&Continuation<O>>,
    ) -> Result<Generation<O>, ModelError> {
        let effort = match settings.effort {
            Effort::Default => None,
            Effort::Off => Some("none"),
            Effort::Low => Some("low"),
            Effort::Medium => Some("medium"),
            Effort::High => Some("high"),
            Effort::Xhigh => Some("xhigh"),
            Effort::Max => Some("max"),
        };
        let endpoint = self.transport.endpoint(self.provider);
        let snapshot = Snapshot::of(context);
        let old = continuation
            .map(|c| {
                c.downcast::<Replay>()
                    .ok_or_else(|| error(ModelErrorKind::ContinuationMismatch))
            })
            .transpose()?;
        if let Some(old) = old {
            old.check(
                self.provider,
                endpoint.as_str(),
                O::KIND,
                &settings.model_id,
                &snapshot,
            )?;
        }
        if O::KIND == OutputKind::Assistant {
            replay::require_history(&snapshot, old)?;
            if snapshot
                .exchanges
                .last()
                .is_none_or(|e| e.outcome().is_some())
            {
                return Err(error(ModelErrorKind::UnsupportedInput));
            }
        }
        let records = old.map_or(&[][..], |r| r.records.as_slice());
        let body = match self.provider {
            Provider::OpenAI => openai::request(&snapshot, settings, effort, O::KIND, records)?,
            Provider::Anthropic => {
                anthropic::request(&snapshot, settings, effort, O::KIND, records)?
            }
        };
        let value = self
            .transport
            .send(self.provider, endpoint.clone(), &body)?;
        let decoded = match self.provider {
            Provider::OpenAI => openai::decode(value)?,
            Provider::Anthropic => anthropic::decode(value)?,
        };
        let (text, wire, raw) = match decoded {
            Decoded::Refused => return Ok(Generation::Refused { reason: None }),
            Decoded::Incomplete(reason) => return Ok(Generation::Incomplete { reason }),
            Decoded::Complete { text, calls, raw } => (text, calls, raw),
        };
        if O::KIND == OutputKind::Decision && !wire.is_empty() {
            return Err(malformed());
        }
        let nonce = uuid::Uuid::new_v4();
        let mut native = Vec::new();
        let mut ids: std::collections::BTreeSet<String> = records
            .iter()
            .flat_map(|record| &record.calls)
            .map(|call| call.id.clone())
            .collect();
        for (index, call) in wire.into_iter().enumerate() {
            if call.id.trim().is_empty() || !ids.insert(call.id.clone()) {
                return Err(malformed());
            }
            let name = ToolName::new(call.name).map_err(|_| malformed())?;
            if !snapshot.tools.iter().any(|tool| tool.name() == &name) {
                return Err(malformed());
            }
            let domain = ToolCall::new(
                ToolCallId::new(format!("{nonce}:{index}")).map_err(|_| malformed())?,
                name,
                call.arguments,
            )
            .map_err(|_| malformed())?;
            native.push(NativeCall {
                call: domain,
                id: call.id,
            });
        }
        let output = O::decode(text, native.iter().map(|c| c.call.clone()).collect())?;
        if O::KIND == OutputKind::Decision {
            return Ok(Generation::Complete {
                output,
                continuation: None,
            });
        }
        let receipt = output.receipt().ok_or_else(malformed)?;
        let mut records = records.to_vec();
        let last = snapshot.exchanges.last().ok_or_else(malformed)?;
        records.push(Record {
            turn: last.turn_id(),
            index: last.messages().len(),
            raw,
            calls: native,
        });
        let state = Replay {
            provider: self.provider,
            endpoint: endpoint.to_string(),
            kind: O::KIND,
            model_id: settings.model_id.clone(),
            snapshot,
            receipt,
            records,
        };
        Ok(Generation::Complete {
            output,
            continuation: Some(Continuation::new(state)),
        })
    }
}
