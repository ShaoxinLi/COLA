use super::{
    Decoded, OutputKind, Provider, anthropic, context, error, google, malformed, openai, replay,
};
use super::{
    output::Output,
    replay::{NativeCall, Record, Replay, Snapshot},
    transport::{ClientConfig, Transport},
};
use crate::{
    app::{Continuation, Effort, Generation, ModelError, ModelErrorKind, ModelSettings},
    domain::{Context, ToolCall, ToolCallId, ToolName},
};
use serde_json::Value;

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
        let effort = profile(self.provider, settings)?;
        let endpoint = self
            .transport
            .endpoint(self.provider, settings.model_id.as_str());
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
                settings,
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
            for tool in &snapshot.tools {
                context::validate_portable_schema(&Value::Object(
                    tool.input_schema().document().clone(),
                ))?;
            }
        }
        let records = old.map_or(&[][..], |r| r.records.as_slice());
        let body = match self.provider {
            Provider::OpenAI => openai::request(&snapshot, settings, effort, O::KIND, records)?,
            Provider::Anthropic => {
                anthropic::request(&snapshot, settings, effort, O::KIND, records)?
            }
            Provider::Google => google::request(&snapshot, settings, effort, O::KIND, records)?,
        };
        let value = self
            .transport
            .send(self.provider, endpoint.clone(), &body)?;
        let decoded = match self.provider {
            Provider::OpenAI => openai::decode(value)?,
            Provider::Anthropic => anthropic::decode(value)?,
            Provider::Google => google::decode(value)?,
        };
        let (text, wire, raw) = match decoded {
            Decoded::Refused => return Ok(Generation::Refused { reason: None }),
            Decoded::Blocked => return Ok(Generation::Blocked { reason: None }),
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
            .filter_map(|call| call.id.clone())
            .collect();
        for (index, call) in wire.into_iter().enumerate() {
            if let Some(id) = &call.id
                && (id.trim().is_empty() || !ids.insert(id.clone()))
            {
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
                position: call.position,
            });
        }
        let output = O::decode(text, native.iter().map(|c| c.call.clone()).collect())?;
        let receipt = output.receipt();
        if O::KIND == OutputKind::Decision {
            return Ok(Generation::Complete {
                output,
                continuation: None,
            });
        }
        let mut records = records.to_vec();
        if receipt.is_some() {
            let last = snapshot.exchanges.last().ok_or_else(malformed)?;
            records.push(Record {
                turn: last.turn_id(),
                index: last.messages().len(),
                raw,
                calls: native,
            });
        }
        let state = Replay {
            provider: self.provider,
            endpoint: endpoint.to_string(),
            kind: O::KIND,
            settings: settings.clone(),
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

struct Profile {
    provider: Provider,
    model: &'static str,
    efforts: &'static [Effort],
}
const PROFILES: &[Profile] = &[
    Profile {
        provider: Provider::OpenAI,
        model: "gpt-6.1-sol",
        efforts: &[
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Xhigh,
            Effort::Max,
        ],
    },
    Profile {
        provider: Provider::Anthropic,
        model: "claude-opus-5-5",
        efforts: &[
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Xhigh,
            Effort::Max,
        ],
    },
    Profile {
        provider: Provider::Google,
        model: "gemini-3.8-flash",
        efforts: &[Effort::Low, Effort::Medium, Effort::High],
    },
];
fn profile(
    provider: Provider,
    settings: &ModelSettings,
) -> Result<Option<&'static str>, ModelError> {
    let profile = PROFILES
        .iter()
        .find(|profile| profile.provider == provider && profile.model == settings.model_id.as_str())
        .ok_or_else(|| error(ModelErrorKind::UnsupportedModel))?;
    if settings.effort == Effort::Default {
        return Ok(None);
    }
    if !profile.efforts.contains(&settings.effort) {
        return Err(error(ModelErrorKind::UnsupportedEffort));
    }
    Ok(Some(match settings.effort {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::Xhigh => "xhigh",
        Effort::Max => "max",
        Effort::Default | Effort::Off => return Err(error(ModelErrorKind::UnsupportedEffort)),
    }))
}
