//! Dynamic ABI decoding and bounded ABI discovery. No signing or transaction submission.
mod resolver;
pub use resolver::{AbiResolver, AbiResolverConfig, ResolvedAbi};

use alloy_dyn_abi::{DynSolValue, EventExt, JsonAbiExt, Specifier};
use alloy_json_abi::{JsonAbi, Param};
use alloy_primitives::B256;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const MAX_ABI_BYTES: usize = 2 * 1024 * 1024;
const MAX_DATA_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct AbiError {
    pub category: String,
    message: &'static str,
}
impl AbiError {
    pub(crate) fn new(category: &str, message: &'static str) -> Self {
        Self {
            category: category.into(),
            message,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Abi {
    parsed: JsonAbi,
    value: Value,
    hash: String,
}
impl Abi {
    pub fn new(value: Value) -> Result<Self, AbiError> {
        let value = value.get("abi").cloned().unwrap_or(value);
        let items = value.as_array().ok_or_else(|| {
            AbiError::new(
                "invalid_abi",
                "ABI must be an array or an artifact with an abi array",
            )
        })?;
        if items.len() > 4096
            || serde_json::to_vec(&value).map_or(true, |v| v.len() > MAX_ABI_BYTES)
        {
            return Err(AbiError::new("too_large", "ABI exceeds configured bounds"));
        }
        for item in items {
            for key in ["inputs", "outputs"] {
                if let Some(params) = item.get(key) {
                    validate_params(params, 0)?;
                }
            }
        }
        let parsed: JsonAbi = serde_json::from_value(value.clone())
            .map_err(|_| AbiError::new("invalid_abi", "ABI schema is invalid"))?;
        // Resolve types at registration rather than failing later inside a scan.
        for f in parsed.functions() {
            for p in f.inputs.iter().chain(&f.outputs) {
                let _: alloy_dyn_abi::DynSolType = p
                    .resolve()
                    .map_err(|_| AbiError::new("invalid_abi", "ABI parameter type is invalid"))?;
            }
        }
        for e in parsed.events() {
            let _: alloy_dyn_abi::DynSolEvent = e
                .resolve()
                .map_err(|_| AbiError::new("invalid_abi", "ABI event type is invalid"))?;
        }
        for e in parsed.errors() {
            for p in &e.inputs {
                let _: alloy_dyn_abi::DynSolType = p
                    .resolve()
                    .map_err(|_| AbiError::new("invalid_abi", "ABI error type is invalid"))?;
            }
        }
        let hash = format!(
            "sha256:{}",
            hex::encode(Sha256::digest(
                serde_json::to_vec(&value).expect("JSON value serializes")
            ))
        );
        Ok(Self {
            parsed,
            value,
            hash,
        })
    }
    pub fn value(&self) -> &Value {
        &self.value
    }
    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn decode_function(&self, input: &str) -> Value {
        let Some(bytes) = data_bytes(input) else {
            return invalid();
        };
        if bytes.len() < 4 {
            return unknown();
        }
        let mut candidates = Vec::new();
        for function in self.parsed.functions() {
            if function.selector().as_slice() != &bytes[..4] {
                continue;
            }
            if let Ok(values) = function.abi_decode_input(&bytes[4..]) {
                // Canonical re-encoding also rejects truncated, padded, or trailing payloads.
                if function.abi_encode_input_raw(&values).ok().as_deref() != Some(&bytes[4..]) {
                    continue;
                }
                candidates.push(json!({"signature":function.signature(),"arguments":arguments(&function.inputs, &values)}));
            }
        }
        choose(candidates)
    }
    pub fn decode_error(&self, input: &str) -> Value {
        let Some(bytes) = data_bytes(input) else {
            return invalid();
        };
        if bytes.len() < 4 {
            return unknown();
        }
        let mut candidates = Vec::new();
        let standard: JsonAbi = serde_json::from_value(json!([
            {"type":"error","name":"Error","inputs":[{"name":"message","type":"string"}]},
            {"type":"error","name":"Panic","inputs":[{"name":"code","type":"uint256"}]}
        ]))
        .expect("built-in ABI is valid");
        for error in self.parsed.errors().chain(standard.errors()) {
            if error.selector().as_slice() != &bytes[..4] {
                continue;
            }
            if let Ok(values) = error.abi_decode_input(&bytes[4..]) {
                if error.abi_encode_input_raw(&values).ok().as_deref() != Some(&bytes[4..]) {
                    continue;
                }
                let candidate = json!({"signature":error.signature(),"arguments":arguments(&error.inputs,&values)});
                if !candidates.contains(&candidate) {
                    candidates.push(candidate);
                }
            }
        }
        choose(candidates)
    }
    pub fn decode_event(&self, log: &Value) -> Value {
        let Some(data) = log.get("data").and_then(Value::as_str).and_then(data_bytes) else {
            return invalid();
        };
        let Some(raw_topics) = log.get("topics").and_then(Value::as_array) else {
            return invalid();
        };
        if raw_topics.len() > 4 {
            return invalid();
        }
        let topics: Option<Vec<B256>> = raw_topics
            .iter()
            .map(|v| v.as_str()?.parse().ok())
            .collect();
        let Some(topics) = topics else {
            return invalid();
        };
        let mut candidates = Vec::new();
        for event in self.parsed.events() {
            if !event.anonymous && topics.first() != Some(&event.selector()) {
                continue;
            }
            let Ok(decoded) = event.decode_log_parts(topics.iter().copied(), &data) else {
                continue;
            };
            let encoded = decoded.encode_log_data();
            if encoded.topics() != topics.as_slice() || encoded.data.as_ref() != data.as_slice() {
                continue;
            }
            let mut indexed = decoded.indexed.iter();
            let mut body = decoded.body.iter();
            let mut args = Vec::new();
            for (i, param) in event.inputs.iter().enumerate() {
                let value = if param.indexed {
                    indexed.next()
                } else {
                    body.next()
                };
                let Some(value) = value else {
                    continue;
                };
                let hashed = param.indexed
                    && (param.ty == "string"
                        || param.ty == "bytes"
                        || param.ty.contains('[')
                        || param.ty.starts_with("tuple"));
                args.push(json!({"name":param.name,"type":param.selector_type(),"path":format!("/{i}"),"value":json_value(value),"indexed":param.indexed,"indexed_hash":hashed}));
            }
            candidates.push(
                json!({"signature":event.signature(),"anonymous":event.anonymous,"arguments":args}),
            );
        }
        choose(candidates)
    }
}
fn validate_params(value: &Value, depth: usize) -> Result<(), AbiError> {
    if depth > 24 {
        return Err(AbiError::new("too_large", "ABI nesting exceeds limit"));
    }
    let params = value
        .as_array()
        .ok_or_else(|| AbiError::new("invalid_abi", "ABI inputs and outputs must be arrays"))?;
    if params.len() > 256 {
        return Err(AbiError::new(
            "too_large",
            "ABI parameter count exceeds limit",
        ));
    }
    for param in params {
        let ty = param
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| AbiError::new("invalid_abi", "ABI parameter type is missing"))?;
        if ty.len() > 512 || ty.matches('[').count() > 24 {
            return Err(AbiError::new("too_large", "ABI type exceeds limits"));
        }
        if let Some(parts) = param.get("components") {
            validate_params(parts, depth + 1)?;
        }
    }
    Ok(())
}
fn data_bytes(s: &str) -> Option<Vec<u8>> {
    let hex = s.strip_prefix("0x")?;
    if hex.len() > MAX_DATA_BYTES * 2 {
        return None;
    }
    hex::decode(hex).ok()
}
fn arguments(params: &[Param], values: &[DynSolValue]) -> Vec<Value> {
    params.iter().zip(values).enumerate().map(|(i,(param,value))| json!({"name":param.name,"type":param.selector_type(),"path":format!("/{i}"),"value":json_value(value)})).collect()
}
fn json_value(value: &DynSolValue) -> Value {
    match value {
        DynSolValue::Bool(v) => json!(v),
        DynSolValue::Int(v, _) => json!(v.to_string()),
        DynSolValue::Uint(v, _) => json!(v.to_string()),
        DynSolValue::FixedBytes(v, n) => json!(format!("0x{}", hex::encode(&v[..*n]))),
        DynSolValue::Address(v) => json!(format!("{v:#x}")),
        DynSolValue::Function(v) => json!(format!("0x{}", hex::encode(v.as_slice()))),
        DynSolValue::Bytes(v) => json!(format!("0x{}", hex::encode(v))),
        DynSolValue::String(v) => json!(v),
        DynSolValue::Array(v) | DynSolValue::FixedArray(v) | DynSolValue::Tuple(v) => {
            Value::Array(v.iter().map(json_value).collect())
        }
    }
}
fn unknown() -> Value {
    json!({"status":"unknown","candidates":[]})
}
fn invalid() -> Value {
    json!({"status":"invalid","candidates":[]})
}
fn choose(mut candidates: Vec<Value>) -> Value {
    match candidates.len() {
        0 => unknown(),
        1 => {
            let mut result = candidates.remove(0);
            result["status"] = json!("decoded");
            result
        }
        _ => json!({"status":"ambiguous","candidates":candidates}),
    }
}
