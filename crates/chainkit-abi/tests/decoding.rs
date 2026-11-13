use alloy_dyn_abi::{DynSolValue, JsonAbiExt};
use alloy_json_abi::JsonAbi;
use alloy_primitives::{keccak256, U256};
use chainkit_abi::Abi;
use serde_json::json;

#[test]
fn exact_integer_tuple_array_and_artifact_decoding() {
    let raw = json!([{"type":"function","name":"set","inputs":[{"name":"items","type":"tuple[]","components":[{"name":"count","type":"uint256"},{"name":"label","type":"string"}]}],"outputs":[],"stateMutability":"nonpayable"}]);
    let parsed: JsonAbi = serde_json::from_value(raw.clone()).unwrap();
    let value = U256::MAX;
    let data = parsed
        .functions()
        .next()
        .unwrap()
        .abi_encode_input(&[DynSolValue::Array(vec![DynSolValue::Tuple(vec![
            DynSolValue::Uint(value, 256),
            DynSolValue::String("sample".into()),
        ])])])
        .unwrap();
    let abi = Abi::new(json!({"abi":raw})).unwrap();
    let result = abi.decode_function(&format!("0x{}", hex::encode(data)));
    assert_eq!(result["signature"], "set((uint256,string)[])");
    assert_eq!(result["arguments"][0]["value"][0][0], value.to_string());
    assert_eq!(result["arguments"][0]["value"][0][1], "sample");
    assert!(abi.hash().starts_with("sha256:"));
}
#[test]
fn indexed_dynamic_values_remain_hashes() {
    let abi=Abi::new(json!([{"type":"event","name":"Tagged","anonymous":false,"inputs":[{"name":"tag","type":"string","indexed":true},{"name":"value","type":"uint256","indexed":false}]}])).unwrap();
    let result=abi.decode_event(&json!({"topics":[format!("{:#x}",keccak256("Tagged(string,uint256)")),format!("0x{}","aa".repeat(32))],"data":format!("0x{:064x}",42)}));
    assert_eq!(result["status"], "decoded");
    assert_eq!(result["arguments"][0]["indexed_hash"], true);
    assert_eq!(result["arguments"][1]["value"], "42");
}
#[test]
fn unknown_malformed_and_builtin_error() {
    let abi = Abi::new(json!([])).unwrap();
    assert_eq!(abi.decode_function("0xdeadbeef")["status"], "unknown");
    assert_eq!(
        abi.decode_event(&json!({"topics":["bad"],"data":"0x"}))["status"],
        "invalid"
    );
    let result = abi.decode_error(&format!("0x4e487b71{:064x}", 17));
    assert_eq!(result["signature"], "Panic(uint256)");
    assert_eq!(result["arguments"][0]["value"], "17");
    assert!(Abi::new(
        json!([{"type":"function","name":"bad","inputs":[{"name":"x","type":"uint7"}]}])
    )
    .is_err());
}
#[test]
fn anonymous_event_and_selector_collision_preserve_candidates() {
    let abi=Abi::new(json!([
        {"type":"event","name":"First","anonymous":true,"inputs":[{"name":"x","type":"uint256","indexed":false}]},
        {"type":"event","name":"Second","anonymous":true,"inputs":[{"name":"x","type":"uint256","indexed":false}]}
    ])).unwrap();
    let result = abi.decode_event(&json!({"topics":[],"data":format!("0x{:064x}",1)}));
    assert_eq!(result["status"], "ambiguous");
    assert_eq!(result["candidates"].as_array().unwrap().len(), 2);
}

#[test]
fn indexed_address_requires_canonical_padding() {
    let abi=Abi::new(json!([{"type":"event","name":"Seen","anonymous":false,"inputs":[{"name":"account","type":"address","indexed":true}]}])).unwrap();
    let signature = format!("{:#x}", keccak256("Seen(address)"));
    let valid = format!("0x{}{}", "00".repeat(12), "11".repeat(20));
    let invalid = format!("0x{}{}", "ff".repeat(12), "11".repeat(20));
    assert_eq!(
        abi.decode_event(&json!({"topics":[signature,valid],"data":"0x"}))["status"],
        "decoded"
    );
    assert_ne!(
        abi.decode_event(&json!({"topics":[signature,invalid],"data":"0x"}))["status"],
        "decoded"
    );
}
