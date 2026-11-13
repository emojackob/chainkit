use serde_json::{json, Value};

pub fn document() -> Value {
    let base = "/v1/projects/{project}/chains/{chain_id}/contracts/{address}";
    let mut paths = serde_json::Map::new();
    let entries = [
        (
            "/v1/projects/{project}/contracts".to_owned(),
            "get",
            "List project contract registrations",
            None,
        ),
        (
            "/v1/projects/{project}/contracts".to_owned(),
            "post",
            "Register a contract with manual ABI or automatic lookup",
            Some(
                json!({"type":"object","required":["chain_id","address","start_block"],"properties":{"chain_id":{"type":"integer","minimum":1},"address":{"$ref":"#/components/schemas/Address"},"start_block":{"type":"integer","minimum":0},"mode":{"enum":["basic","full"],"default":"full"},"abi":{"description":"ABI array or artifact. Omit for automatic discovery; [] explicitly collects raw evidence."}}}),
            ),
        ),
        (
            format!("{base}/coverage"),
            "get",
            "Read coverage before interpreting empty results",
            None,
        ),
        (
            format!("{base}/transactions"),
            "get",
            "Search related transactions",
            None,
        ),
        (
            format!("{base}/events"),
            "get",
            "Search contract-emitted events",
            None,
        ),
        (
            format!("{base}/context/{{tx_hash}}"),
            "get",
            "Read transaction evidence and call traces",
            None,
        ),
        (format!("{base}/abis"), "get", "List ABI history", None),
        (
            format!("{base}/abis"),
            "post",
            "Bind ABI at an effective historical block",
            Some(
                json!({"type":"object","required":["abi","effective_from"],"properties":{"abi":{},"effective_from":{"type":"integer","minimum":0}}}),
            ),
        ),
        (
            format!("{base}/pause"),
            "post",
            "Pause or resume a contract registration",
            Some(
                json!({"type":"object","required":["paused"],"properties":{"paused":{"type":"boolean"}}}),
            ),
        ),
        (
            format!("{base}/backfill"),
            "post",
            "Invalidate the indexed tail and queue a rescan",
            Some(
                json!({"type":"object","required":["from_block"],"properties":{"from_block":{"type":"integer","minimum":0}}}),
            ),
        ),
    ];
    for (path, method, summary, body) in entries {
        let mut parameters = Vec::new();
        for segment in path.split('/') {
            if segment.starts_with('{') && segment.ends_with('}') {
                let name = &segment[1..segment.len() - 1];
                let schema = match name {
                    "chain_id" => json!({"type":"integer","minimum":1}),
                    "address" => json!({"$ref":"#/components/schemas/Address"}),
                    "tx_hash" => json!({"type":"string","pattern":"^0x[0-9a-fA-F]{64}$"}),
                    _ => json!({"type":"string"}),
                };
                parameters.push(json!({"name":name,"in":"path","required":true,"schema":schema}));
            }
        }
        if method == "get"
            && (path.ends_with("/transactions")
                || path.ends_with("/events")
                || path.contains("/context/"))
        {
            let names: &[&str] = if path.contains("/context/") {
                &["limit", "cursor"]
            } else if path.ends_with("/transactions") {
                &[
                    "limit",
                    "cursor",
                    "from_block",
                    "to_block",
                    "tx_hash",
                    "sender",
                    "status",
                    "signature",
                ]
            } else {
                &[
                    "limit",
                    "cursor",
                    "from_block",
                    "to_block",
                    "tx_hash",
                    "sender",
                    "status",
                    "signature",
                    "topic0",
                ]
            };
            for &name in names {
                let schema = match name {
                    "limit" => json!({"type":"integer","minimum":1,"maximum":200,"default":50}),
                    "from_block" | "to_block" => {
                        json!({"type":"integer","minimum":0,"description":"Block ranges are [from_block,to_block)."})
                    }
                    "status" => json!({"type":"integer","enum":[0,1]}),
                    _ => json!({"type":"string"}),
                };
                parameters.push(json!({"name":name,"in":"query","schema":schema}));
            }
        }
        let output = if path.ends_with("/transactions") {
            "TransactionPage"
        } else if path.ends_with("/events") {
            "EventPage"
        } else if path.contains("/context/") {
            "Context"
        } else if path.ends_with("/coverage") || path.ends_with("/backfill") {
            "CoverageEnvelope"
        } else if path.ends_with("/abis") && method == "get" {
            "AbiHistoryEnvelope"
        } else if path.ends_with("/abis") {
            "BindingEnvelope"
        } else if path.ends_with("/contracts") && method == "get" {
            "ContractListEnvelope"
        } else if path.ends_with("/contracts") {
            "RegistrationEnvelope"
        } else {
            "CoverageEnvelope"
        };
        let mut op = json!({"summary":summary,"parameters":parameters,
            "operationId":format!("{}_{}",method,path.replace(['/', '{', '}'],"_")),
            "security":if method=="post"{json!([{"ProjectToken":[]}])}else{json!([])},
            "responses":{"200":{"description":"Success. Inspect coverage and warnings.","content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{output}")}}}},
                "400":{"description":"Invalid request"},"401":{"description":"Manage token required"},"403":{"description":"Invalid project token"},"404":{"description":"Project or contract not found"},"409":{"description":"Conflict or invalidated snapshot"},"413":{"description":"Message exceeds limits"},"500":{"description":"Storage unavailable"}}});
        for code in ["400", "401", "403", "404", "409", "413", "500"] {
            op["responses"][code]["content"] =
                json!({"application/json":{"schema":reference("Error")}});
        }
        if let Some(body) = body {
            op["requestBody"] =
                json!({"required":true,"content":{"application/json":{"schema":body}}});
        }
        paths.entry(path).or_insert_with(|| json!({}))[method] = op;
    }
    json!({"openapi":"3.1.0","info":{"title":"Chainkit indexer","version":env!("CARGO_PKG_VERSION"),
    "description":"Project-scoped EVM evidence. GET queries are public; POST requires a project manage token. Missing indexed data never proves absence onchain. Full mode requires block-level call traces. Integers in ABI values are decimal strings. MCP is provided separately over stdio."},
    "paths":paths,
    "components":{"securitySchemes":{"ProjectToken":{"type":"http","scheme":"bearer"}},"schemas":schemas()}})
}

fn reference(name: &str) -> Value {
    json!({"$ref":format!("#/components/schemas/{name}")})
}
fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required})
}
fn array(items: Value) -> Value {
    json!({"type":"array","items":items})
}
fn wrapped(data: Value) -> Value {
    object(
        json!({"schema_version":{"const":"1"},"data":data,"warnings":array(json!({"type":"string"}))}),
        &["schema_version", "data", "warnings"],
    )
}
fn page(item: &str) -> Value {
    object(
        json!({"schema_version":{"const":"1"},"data":array(reference(item)),"coverage":reference("Coverage"),"warnings":array(json!({"type":"string"})),"next_cursor":{"type":["string","null"]},"snapshot_to_block":{"type":"integer","minimum":0}}),
        &[
            "schema_version",
            "data",
            "coverage",
            "warnings",
            "next_cursor",
            "snapshot_to_block",
        ],
    )
}

pub fn schemas() -> Value {
    let text = json!({"type":"string"});
    let integer = json!({"type":"integer","minimum":0});
    let boolean = json!({"type":"boolean"});
    let raw = json!({"type":"object","additionalProperties":true,"description":"Untrusted raw RPC evidence. Onchain quantities remain hex strings."});
    let strings = array(text.clone());
    let mut result = json!({
        "Address":{"type":"string","pattern":"^0x[0-9a-fA-F]{40}$"},
        "Hash":{"type":"string","pattern":"^0x[0-9a-fA-F]{64}$"},
        "Abi":{"oneOf":[{"type":"array","items":{"type":"object"}},{"type":"object","required":["abi"],"properties":{"abi":{"type":"array","items":{"type":"object"}}}}]},
        "Argument":object(json!({"name":text,"type":text,"path":text,"value":{"description":"Integers are exact decimal strings, bytes and addresses are hex, tuple/array values are positional arrays."},"indexed":boolean,"indexed_hash":boolean}), &["name","type","path","value"]),
        "Decoding":object(json!({"status":{"enum":["decoded","unknown","invalid","ambiguous"]},"signature":text,"arguments":array(reference("Argument")),"reason":text,"candidates":array(object(json!({"signature":text,"arguments":array(reference("Argument"))}), &["signature","arguments"]))}), &["status"]),
        "Coverage":object(json!({
            "project":text,"chain_id":integer,"address":reference("Address"),"mode":{"enum":["basic","full"]},
            "start_block":integer,"next_block":integer,
            "indexed_range":object(json!({"from_block":integer,"to_block":integer}), &["from_block","to_block"]),
            "paused":boolean,"head":{"type":["integer","null"],"minimum":0},"last_error":{"type":["string","null"]},
            "revision":integer,"genesis_hash":{"type":["string","null"]},"updated_at":text,
            "capabilities":object(json!({"emitted_events":boolean,"direct_transactions":boolean,"contract_creation":boolean,"internal_calls":boolean,"external_token_transfers":boolean}), &["emitted_events","direct_transactions","contract_creation","internal_calls","external_token_transfers"]),
            "limitations":strings
        }), &["project","chain_id","address","mode","start_block","next_block","indexed_range","paused","head","last_error","revision","genesis_hash","updated_at","capabilities","limitations"]),
        "Contract":object(json!({"id":integer,"project":text,"chain_id":integer,"address":reference("Address"),"start_block":integer,"next_block":integer,"mode":{"enum":["basic","full"]},"paused":boolean,"revision":integer,"head":{"type":["integer","null"]},"last_error":{"type":["string","null"]},"genesis_hash":{"type":["string","null"]},"updated_at":text}), &["project","chain_id","address","start_block","next_block","mode","paused","revision"]),
        "TraceFrame":object(json!({"trace_path":array(integer.clone()),"type":text,"context_address":{"type":["string","null"]},"code_address":{"type":["string","null"]},"reverted":boolean,"error":{"type":["string","null"]},"decoded_input":{"oneOf":[reference("Decoding"),{"type":"null"}]},"decoded_error":{"oneOf":[reference("Decoding"),{"type":"null"}]}}), &["trace_path","type","reverted"]),
        "Trace":object(json!({"raw":raw,"related_frames":array(reference("TraceFrame")),"abi_binding":{"const":"operator_supplied_block_level"}}), &["raw","related_frames","abi_binding"]),
        "Transaction":object(json!({"hash":reference("Hash"),"block_hash":reference("Hash"),"block_number":integer,"transaction_index":integer,"status":{"type":"integer","enum":[0,1]},"relations":array(json!({"type":"string","description":"Evidence relationship, such as direct_call, creation, emitted_event, or call_trace."})),"raw":raw,"receipt":raw,"decoded":reference("Decoding"),"trace":{"oneOf":[reference("Trace"),{"type":"null"}]}}), &["hash","block_hash","block_number","transaction_index","status","relations","raw","receipt","decoded","trace"]),
        "Event":object(json!({"transaction_hash":reference("Hash"),"block_hash":reference("Hash"),"block_number":integer,"transaction_index":integer,"log_index":integer,"raw":raw,"decoded":reference("Decoding")}), &["transaction_hash","block_hash","block_number","transaction_index","log_index","raw","decoded"]),
        "AbiRevision":object(json!({"revision":integer,"effective_from":integer,"abi_hash":text,"abi":reference("Abi"),"source":{"enum":["manual","sourcify","etherscan"]},"created_at":text}), &["revision","effective_from","abi_hash","abi","source","created_at"]),
        "Binding":object(json!({"abi_hash":text,"effective_from":integer,"revision":integer,"redecoded":boolean}), &["abi_hash","effective_from","revision","redecoded"]),
        "Error":object(json!({"schema_version":{"const":"1"},"error":object(json!({"code":text,"message":text}), &["code","message"])}), &["schema_version","error"])
    });
    result["Envelope"] = wrapped(json!({}));
    result["CoverageEnvelope"] = wrapped(reference("Coverage"));
    result["ContractListEnvelope"] = wrapped(array(reference("Contract")));
    result["RegistrationEnvelope"] = wrapped(object(
        json!({"registration":reference("Contract"),"abi_source":{"enum":["manual","sourcify","etherscan"]}}),
        &["registration", "abi_source"],
    ));
    result["AbiHistoryEnvelope"] = wrapped(array(reference("AbiRevision")));
    result["BindingEnvelope"] = wrapped(reference("Binding"));
    result["TransactionPage"] = page("Transaction");
    result["EventPage"] = page("Event");
    // Generic Page is retained as a discovery alias; actual operations use typed pages.
    result["Page"] = page("Transaction");
    result["Context"] = object(
        json!({"schema_version":{"const":"1"},"data":{"oneOf":[{"type":"null"},object(json!({"transaction":reference("Transaction"),"logs":array(reference("Event")),"next_cursor":{"type":["string","null"]}}), &["transaction","logs","next_cursor"])]},"coverage":reference("Coverage"),"warnings":strings,"next_cursor":{"type":["string","null"]}}),
        &[
            "schema_version",
            "data",
            "coverage",
            "warnings",
            "next_cursor",
        ],
    );
    result
}
