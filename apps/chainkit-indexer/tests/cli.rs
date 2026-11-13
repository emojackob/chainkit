use serde_json::{json, Value};
use std::process::Command;

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_chainkit-indexer"))
}

#[test]
fn help_and_openapi_do_not_create_a_database() {
    let temp = tempfile::tempdir().unwrap();
    let help = binary()
        .current_dir(temp.path())
        .arg("--help")
        .output()
        .unwrap();
    assert!(
        help.status.success(),
        "{}",
        String::from_utf8_lossy(&help.stderr)
    );
    assert!(String::from_utf8_lossy(&help.stdout).contains("rpc-health"));
    let schema = binary()
        .current_dir(temp.path())
        .arg("openapi")
        .output()
        .unwrap();
    assert!(schema.status.success());
    let schema: Value = serde_json::from_slice(&schema.stdout).unwrap();
    assert_eq!(schema["openapi"], "3.1.0");
    assert!(!temp.path().join("chainkit.sqlite").exists());
}

#[test]
fn local_project_registration_and_query_need_no_python_or_rpc_calls() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config.toml");
    let database = temp.path().join("test.sqlite");
    let text = format!(
        "database = {}\n[chains]\n31337 = [\"http://127.0.0.1:1\"]\n[rpc]\nmax_attempts = 1\ntimeout_ms = 100\ntotal_timeout_ms = 200\n",
        serde_json::to_string(&database.to_string_lossy()).unwrap()
    );
    std::fs::write(&config, text).unwrap();
    let run = |args: &[&str]| {
        let output = binary()
            .arg("--config")
            .arg(&config)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let created = run(&["project-create", "demo", "--name", "Demo"]);
    assert!(created["data"]["tokens"]["manage"].as_str().is_some());
    let registered = run(&[
        "contract-add",
        "--project",
        "demo",
        "--chain-id",
        "31337",
        "--address",
        "0x1111111111111111111111111111111111111111",
        "--start-block",
        "0",
        "--mode",
        "basic",
        "--abi-json",
        "[]",
    ]);
    assert_eq!(registered["data"]["abi_source"], "manual");
    let contracts = run(&["contracts", "--project", "demo"]);
    assert_eq!(contracts["data"].as_array().unwrap().len(), 1);
    let events = run(&[
        "events",
        "--project",
        "demo",
        "--chain-id",
        "31337",
        "--address",
        "0x1111111111111111111111111111111111111111",
    ]);
    assert_eq!(events["data"], json!([]));
    assert!(events.get("coverage").is_some());
    let scan = binary()
        .arg("--config")
        .arg(&config)
        .arg("scan")
        .output()
        .unwrap();
    assert!(!scan.status.success());
}

#[test]
fn checked_in_configuration_example_loads() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/rust-indexer/config.example.toml");
    let config = chainkit_indexer_cli::Config::load(Some(&path)).unwrap();
    assert_eq!(config.chains.get(&31337).unwrap().len(), 2);
    assert_eq!(config.rpc.max_attempts, 6);
    assert_eq!(config.pools().unwrap().len(), 1);
}
