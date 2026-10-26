use super::support::*;
use cola::{adapters::models::*, app::*};
use serde_json::{Value, json};
use std::{error::Error, num::NonZeroUsize, time::Duration};

#[test]
fn openai_failed_response_is_a_safe_provider_failure() {
    let p = Provider::OpenAI;
    let server = Server::new(vec![
        json!({"status":"failed","error":{"code":"server_error","message":"BODY_SENTINEL"}}).into(),
    ]);
    let error = p
        .client(&server)
        .generate(&context(&session()), &p.settings(), None)
        .unwrap_err();
    assert_eq!(error.kind(), ModelErrorKind::ProviderFailure);
    assert_safe(&error);
    assert_eq!(server.requests().len(), 1);
}
#[test]
fn config_rejects_unsafe_urls_secrets_and_limits_with_redacted_diagnostics() {
    for url in [
        "file:///tmp/key",
        "http://@localhost/v1",
        "ftp://example.com",
        "http://user:pass@localhost/v1",
        "http://localhost/v1?secret=URL_SENTINEL",
        "http://localhost/v1#URL_SENTINEL",
        "http://[invalid",
    ] {
        let error = ClientConfig::new(
            url,
            "API_KEY_SENTINEL",
            Duration::from_secs(1),
            NonZeroUsize::new(1024).unwrap(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::InvalidConfig);
        assert_safe(&error);
    }
    for key in ["", "  ", "API_KEY_SENTINEL\r\nx-leak: true"] {
        assert_eq!(
            ClientConfig::new(
                "https://example.com/v1",
                key,
                Duration::from_secs(1),
                NonZeroUsize::new(1024).unwrap()
            )
            .unwrap_err()
            .kind(),
            ModelErrorKind::InvalidConfig
        );
    }
    assert!(
        ClientConfig::new(
            "https://example.com",
            "key",
            Duration::ZERO,
            NonZeroUsize::new(1024).unwrap()
        )
        .is_err()
    );
    assert!(
        ClientConfig::new(
            "https://example.com",
            "key",
            Duration::from_secs(1),
            NonZeroUsize::new(usize::MAX).unwrap()
        )
        .is_err()
    );
    let config = ClientConfig::new(
        "https://URL_SENTINEL.example/v1",
        "API_KEY_SENTINEL",
        Duration::from_secs(1),
        NonZeroUsize::new(1024).unwrap(),
    )
    .unwrap();
    let printed = format!(
        "{config:?} {:?}",
        OpenAIClient::new(config.clone()).unwrap()
    );
    assert!(!printed.contains("SENTINEL"));
    assert!(!format!("{:?}", ModelId::new("URL_SENTINEL").unwrap()).contains("SENTINEL"));
}
fn assert_safe(error: &ModelError) {
    let text = format!("{error:?} {error}");
    for secret in [
        "API_KEY_SENTINEL",
        "BODY_SENTINEL",
        "URL_SENTINEL",
        "secret-key",
        "http://",
    ] {
        assert!(!text.contains(secret), "leak {text}");
    }
    assert!(error.source().is_none());
}
#[test]
fn http_statuses_are_safe_typed_and_never_automatically_retried() {
    for p in PROVIDERS {
        for (status, expected) in [
            (401, ModelErrorKind::Authentication),
            (403, ModelErrorKind::Authentication),
            (429, ModelErrorKind::RateLimited),
            (500, ModelErrorKind::ProviderFailure),
            (400, ModelErrorKind::InvalidSettings),
        ] {
            let server = Server::new(vec![Reply {
                status,
                body: "BODY_SENTINEL API_KEY_SENTINEL URL_SENTINEL".into(),
                delay: Duration::ZERO,
                body_delay: std::time::Duration::ZERO,
                headers: vec![],
            }]);
            let error = p
                .client(&server)
                .generate(&context(&session()), &p.settings(), None)
                .unwrap_err();
            assert_eq!(error.kind(), expected);
            assert_eq!(error.http_status(), Some(status));
            assert_safe(&error);
            assert_eq!(server.requests().len(), 1);
        }
    }
}
#[test]
fn redirects_are_not_followed_or_allowed_to_leak_credentials() {
    let target = Server::new(vec![]);
    for p in PROVIDERS {
        let server = Server::new(vec![Reply {
            status: 307,
            body: "BODY_SENTINEL".into(),
            delay: Duration::ZERO,
            body_delay: std::time::Duration::ZERO,
            headers: vec![("Location".into(), target.url.clone())],
        }]);
        let error = p
            .client(&server)
            .generate(&context(&session()), &p.settings(), None)
            .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::Http);
        assert_safe(&error);
        assert!(target.requests().is_empty());
        assert_eq!(server.requests().len(), 1);
    }
}
#[test]
fn deadlines_and_bounded_response_reads_are_enforced() {
    for p in PROVIDERS {
        let mut reply: Reply = p.answer("late").into();
        reply.delay = Duration::from_millis(100);
        let server = Server::new(vec![reply]);
        let config = ClientConfig::new(
            &server.url,
            "key",
            Duration::from_millis(20),
            NonZeroUsize::new(1024).unwrap(),
        )
        .unwrap();
        let error = p
            .configured(config)
            .generate(&context(&session()), &p.settings(), None)
            .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::Timeout);
        assert_safe(&error);
        let server = Server::new(vec![p.answer("BODY_SENTINEL").into()]);
        let config = ClientConfig::new(
            &server.url,
            "key",
            Duration::from_secs(1),
            NonZeroUsize::new(10).unwrap(),
        )
        .unwrap();
        let error = p
            .configured(config)
            .generate(&context(&session()), &p.settings(), None)
            .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::ResponseTooLarge);
        assert_safe(&error);
    }
}
#[test]
fn malformed_http_json_is_redacted() {
    for p in PROVIDERS {
        let server = Server::new(vec![Reply {
            status: 200,
            body: "not json BODY_SENTINEL".into(),
            delay: Duration::ZERO,
            body_delay: std::time::Duration::ZERO,
            headers: vec![],
        }]);
        let error = p
            .client(&server)
            .generate(&context(&session()), &p.settings(), None)
            .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::MalformedResponse);
        assert_safe(&error);
    }
}
#[test]
fn truncated_and_refused_responses_never_expose_partial_executable_calls() {
    let cases = [
        (
            Provider::OpenAI,
            json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"function_call","call_id":"dangerous","name":"lookup","arguments":"{}"}]}),
            "incomplete",
        ),
        (
            Provider::Anthropic,
            json!({"stop_reason":"max_tokens","content":[{"type":"tool_use","id":"dangerous","name":"lookup","input":{}}]}),
            "incomplete",
        ),
        (
            Provider::OpenAI,
            json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"API_KEY_SENTINEL"}]}]}),
            "refused",
        ),
        (
            Provider::Anthropic,
            json!({"stop_reason":"refusal","content":[]}),
            "refused",
        ),
    ];
    for (p, value, kind) in cases {
        let server = Server::new(vec![value.into()]);
        let result = p
            .client(&server)
            .generate(&context(&session()), &p.settings(), None)
            .unwrap();
        match kind {
            "incomplete" => assert!(matches!(
                result,
                Generation::Incomplete {
                    reason: IncompleteReason::OutputLimit
                }
            )),
            "refused" => assert!(matches!(result, Generation::Refused { reason: None })),
            _ => unreachable!(),
        };
    }
}
#[test]
fn unknown_native_items_and_missing_completion_metadata_fail_closed() {
    let cases = [
        (
            Provider::OpenAI,
            json!({"status":"completed","output":[{"type":"web_search_call"}]}),
            ModelErrorKind::UnsupportedResponse,
        ),
        (
            Provider::OpenAI,
            json!({"output":[]}),
            ModelErrorKind::MalformedResponse,
        ),
        (
            Provider::OpenAI,
            json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"audio","data":"secret"}]}]}),
            ModelErrorKind::UnsupportedResponse,
        ),
        (
            Provider::Anthropic,
            json!({"type":"message","role":"assistant","stop_reason":"end_turn","content":[{"type":"server_tool_use","name":"unknown"}]}),
            ModelErrorKind::UnsupportedResponse,
        ),
        (
            Provider::Anthropic,
            json!({"type":"message","role":"assistant","content":[]}),
            ModelErrorKind::MalformedResponse,
        ),
    ];
    for (p, value, expected) in cases {
        let server = Server::new(vec![value.into()]);
        let error = p
            .client(&server)
            .generate(&context(&session()), &p.settings(), None)
            .unwrap_err();
        assert_eq!(error.kind(), expected);
        assert_safe(&error);
    }
}
fn mutate_call(p: Provider, value: &mut Value, field: &str, replacement: Value) {
    match p {
        Provider::OpenAI => {
            let key = match field {
                "input" => "arguments",
                other => other,
            };
            value["output"][2][key] = if field == "input" {
                json!(replacement.to_string())
            } else {
                replacement
            };
        }
        Provider::Anthropic => value["content"][3][field] = replacement,
    }
}
#[test]
fn invalid_arguments_tool_names_ambiguous_native_ids_and_empty_output_are_rejected() {
    for p in PROVIDERS {
        for (field, replacement) in [("name", json!("not_offered")), ("input", json!([]))] {
            let mut response = p.tool(1);
            mutate_call(p, &mut response, field, replacement);
            let server = Server::new(vec![response.into()]);
            assert_eq!(
                p.client(&server)
                    .generate(&context(&session()), &p.settings(), None)
                    .unwrap_err()
                    .kind(),
                ModelErrorKind::MalformedResponse
            );
        }
        let mut response = p.tool(1);
        match p {
            Provider::OpenAI => {
                let call = response["output"][2].clone();
                response["output"].as_array_mut().unwrap().push(call);
            }
            Provider::Anthropic => {
                let call = response["content"][3].clone();
                response["content"].as_array_mut().unwrap().push(call);
            }
        }
        let server = Server::new(vec![response.into()]);
        assert_eq!(
            p.client(&server)
                .generate(&context(&session()), &p.settings(), None)
                .unwrap_err()
                .kind(),
            ModelErrorKind::MalformedResponse
        );
        let server = Server::new(vec![p.answer("  ").into()]);
        assert_eq!(
            p.client(&server)
                .generate(&context(&session()), &p.settings(), None)
                .unwrap_err()
                .kind(),
            ModelErrorKind::MalformedResponse
        );
    }
}
#[test]
fn chunked_responses_cannot_bypass_the_body_limit() {
    for p in PROVIDERS {
        let body = "BODY_SENTINEL".repeat(20);
        let server = Server::new(vec![Reply {
            status: 200,
            body: format!("{:X}\r\n{}\r\n0\r\n\r\n", body.len(), body),
            delay: Duration::ZERO,
            body_delay: std::time::Duration::ZERO,
            headers: vec![("Transfer-Encoding".into(), "chunked".into())],
        }]);
        let config = ClientConfig::new(
            &server.url,
            "key",
            Duration::from_secs(1),
            NonZeroUsize::new(32).unwrap(),
        )
        .unwrap();
        let error = p
            .configured(config)
            .generate(&context(&session()), &p.settings(), None)
            .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::ResponseTooLarge);
        assert_safe(&error);
    }
}
#[test]
fn refused_connection_is_a_safe_transport_error() {
    let url = "http://127.0.0.1:0/URL_SENTINEL";
    for p in PROVIDERS {
        let config = ClientConfig::new(
            url,
            "API_KEY_SENTINEL",
            Duration::from_secs(1),
            NonZeroUsize::new(1024).unwrap(),
        )
        .unwrap();
        let error = p
            .configured(config)
            .generate(&context(&session()), &p.settings(), None)
            .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::Transport);
        assert_safe(&error);
    }
}

#[test]
fn response_body_reads_share_the_total_deadline() {
    for p in PROVIDERS {
        let mut reply: Reply = p.answer("late body").into();
        reply.body_delay = Duration::from_millis(100);
        let server = Server::new(vec![reply]);
        let config = ClientConfig::new(
            &server.url,
            "API_KEY_SENTINEL",
            Duration::from_millis(20),
            NonZeroUsize::new(4096).unwrap(),
        )
        .unwrap();
        let error = p
            .configured(config)
            .generate(&context(&session()), &p.settings(), None)
            .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::Timeout);
        assert_eq!(error.http_status(), None);
        assert_safe(&error);
    }
}

#[test]
fn assistant_requires_an_active_last_exchange_before_http() {
    for p in PROVIDERS {
        let server = Server::new(vec![]);
        let empty = cola::domain::Context::new(text("instructions"), vec![], vec![]).unwrap();
        assert_eq!(
            p.client(&server)
                .generate(&empty, &p.settings(), None)
                .unwrap_err()
                .kind(),
            ModelErrorKind::UnsupportedInput
        );
        let mut s = session();
        accept(
            &mut s,
            cola::domain::AssistantMessage::answer("historical answer").unwrap(),
        );
        assert_eq!(
            p.client(&server)
                .generate(&context(&s), &p.settings(), None)
                .unwrap_err()
                .kind(),
            ModelErrorKind::UnsupportedInput
        );
        assert!(server.requests().is_empty());
    }
}
