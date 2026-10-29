#[path = "adapters/stores/jsonl.rs"]
mod jsonl;
#[path = "adapters/models"]
mod models {
    mod anthropic;
    mod client;
    mod openai;
    #[allow(dead_code)]
    mod support;
}
#[path = "adapters/tools"]
mod tools {
    mod bash;
    mod files;

    use cola::adapters::tools::{BashTool, ReadFileTool, WriteFileTool};
    use cola::domain::{Tool, ToolDefinition, ToolOutcome};
    use serde_json::{Value, json};
    use std::fs;
    use tempfile::tempdir;

    fn execute(tool: &dyn Tool, arguments: Value) -> ToolOutcome {
        tool.execute(arguments.as_object().unwrap())
    }

    fn success(outcome: ToolOutcome) -> String {
        match outcome {
            ToolOutcome::Success(output) => output,
            other => panic!("expected success, got {other:?}"),
        }
    }

    fn failure(outcome: ToolOutcome) -> String {
        match outcome {
            ToolOutcome::Failure(error) => {
                let text = error.description().as_str();
                assert!(!text.trim().is_empty());
                text.to_owned()
            }
            other => panic!("expected failure, got {other:?}"),
        }
    }

    #[test]
    fn definitions_describe_strict_argument_shapes() {
        let definitions: [(ToolDefinition, &str, &[&str]); 3] = [
            (BashTool::definition(), "bash", &["command"]),
            (ReadFileTool::definition(), "read_file", &["path"]),
            (
                WriteFileTool::definition(),
                "write_file",
                &["path", "content"],
            ),
        ];
        for (definition, name, fields) in definitions {
            assert_eq!(definition.name().as_str(), name);
            assert!(!definition.description().as_str().is_empty());
            let schema = definition.input_schema().document();
            assert_eq!(schema["type"], "object");
            assert_eq!(schema["additionalProperties"], false);
            assert_eq!(schema["required"], json!(fields));
            let properties = schema["properties"].as_object().unwrap();
            assert_eq!(properties.len(), fields.len());
            for field in fields {
                assert_eq!(properties[*field]["type"], "string");
                if *field == "content" {
                    assert!(properties[*field].get("minLength").is_none());
                } else {
                    assert_eq!(properties[*field]["minLength"], 1);
                }
            }
        }
        let bash_description = BashTool::definition().description().as_str().to_owned();
        assert!(bash_description.contains("no wall-clock limit"));
        assert!(bash_description.contains("descendants"));
    }

    #[test]
    fn invalid_arguments_fail_before_effects() {
        let directory = tempdir().unwrap();
        let bash = BashTool::new(directory.path()).unwrap();
        let read = ReadFileTool::new(directory.path()).unwrap();
        let write = WriteFileTool::new(directory.path()).unwrap();
        for arguments in [
            json!({}),
            json!({"command": null}),
            json!({"command": ["touch", "effect"]}),
            json!({"command": ""}),
            json!({"command": "touch effect", "extra": true}),
        ] {
            failure(execute(&bash, arguments));
        }
        for arguments in [
            json!({}),
            json!({"path": null}),
            json!({"path": 1}),
            json!({"path": ""}),
            json!({"path": "effect", "extra": true}),
        ] {
            failure(execute(&read, arguments));
        }
        for arguments in [
            json!({}),
            json!({"path": "effect"}),
            json!({"content": "text"}),
            json!({"path": "effect", "content": null}),
            json!({"path": false, "content": "text"}),
            json!({"path": "", "content": "text"}),
            json!({"path": "effect", "content": "text", "extra": true}),
        ] {
            failure(execute(&write, arguments));
        }
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
        fs::write(directory.path().join("existing"), "keep").unwrap();
        failure(execute(
            &write,
            json!({"path": "existing", "content": "replace", "extra": true}),
        ));
        assert_eq!(
            fs::read_to_string(directory.path().join("existing")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn constructors_require_existing_directories() {
        let directory = tempdir().unwrap();
        let missing = directory.path().join("missing");
        let file = directory.path().join("file");
        fs::write(&file, "text").unwrap();
        for path in [&missing, &file] {
            assert!(BashTool::new(path).is_err());
            assert!(ReadFileTool::new(path).is_err());
            assert!(WriteFileTool::new(path).is_err());
        }
    }

    #[test]
    fn instances_anchor_paths_without_changing_process_cwd() {
        let original = std::env::current_dir().unwrap();
        let parent = tempfile::tempdir_in(&original).unwrap();
        let first = parent.path().join("first");
        let second = parent.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let relative = first.strip_prefix(&original).unwrap();
        let first_write = WriteFileTool::new(relative).unwrap();
        let second_write = WriteFileTool::new(&second).unwrap();
        success(execute(
            &first_write,
            json!({"path": "same", "content": "first"}),
        ));
        success(execute(
            &second_write,
            json!({"path": "same", "content": "second"}),
        ));
        let first_read = ReadFileTool::new(&first).unwrap();
        assert_eq!(
            success(execute(&first_read, json!({"path": "same"}))),
            "first"
        );
        assert_eq!(
            success(execute(&first_read, json!({"path": second.join("same")}))),
            "second"
        );
        success(execute(
            &first_write,
            json!({"path": second.join("absolute"), "content": "outside"}),
        ));
        assert_eq!(
            fs::read_to_string(second.join("absolute")).unwrap(),
            "outside"
        );
        success(execute(
            &first_write,
            json!({"path": "../parent", "content": "parent"}),
        ));
        assert_eq!(
            fs::read_to_string(parent.path().join("parent")).unwrap(),
            "parent"
        );
        for path in [&first, &second] {
            let bash = BashTool::new(path).unwrap();
            let output = success(execute(&bash, json!({"command": "printf '%s' \"$PWD\""})));
            assert_eq!(output, path.canonicalize().unwrap().to_str().unwrap());
        }
        assert_eq!(std::env::current_dir().unwrap(), original);
    }

    #[cfg(unix)]
    #[test]
    fn constructors_anchor_symlinked_directories_once() {
        use std::os::unix::fs::symlink;
        let directory = tempdir().unwrap();
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        let alias = directory.path().join("alias");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        fs::write(first.join("source"), "first").unwrap();
        fs::write(second.join("source"), "second").unwrap();
        symlink(&first, &alias).unwrap();
        let read = ReadFileTool::new(&alias).unwrap();
        let write = WriteFileTool::new(&alias).unwrap();
        let bash = BashTool::new(&alias).unwrap();
        fs::remove_file(&alias).unwrap();
        symlink(&second, &alias).unwrap();
        assert_eq!(success(execute(&read, json!({"path": "source"}))), "first");
        success(execute(
            &write,
            json!({"path": "created", "content": "anchored"}),
        ));
        assert_eq!(
            fs::read_to_string(first.join("created")).unwrap(),
            "anchored"
        );
        assert!(!second.join("created").exists());
        assert_eq!(
            success(execute(&bash, json!({"command": "cat source"}))),
            "first"
        );
    }

    #[test]
    fn removed_working_directory_returns_contextual_failures() {
        let directory = tempdir().unwrap();
        let cwd = directory.path().join("removed");
        fs::create_dir(&cwd).unwrap();
        let bash = BashTool::new(&cwd).unwrap();
        let read = ReadFileTool::new(&cwd).unwrap();
        let write = WriteFileTool::new(&cwd).unwrap();
        fs::remove_dir(&cwd).unwrap();
        let error = failure(execute(&bash, json!({"command": "printf unreachable"})));
        assert!(error.to_lowercase().contains("bash"));
        for outcome in [
            execute(&read, json!({"path": "file"})),
            execute(&write, json!({"path": "file", "content": "text"})),
        ] {
            assert!(failure(outcome).contains("removed"));
        }
    }
}

#[path = "adapters/api"]
mod api {
    mod cli;
    mod web;
}
