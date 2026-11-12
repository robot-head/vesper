//! Strict W3C TraceContext v00 inspection, without mutating tracing state.
use vesper_beacon::{W3cSpanContext, parse_traceparent_55};

const HELP: &str = "Usage: vesper traceparent HEADER [--json]\n\nValidate an exact W3C TraceContext version-00 header and emit its canonical form.\nOnly lowercase hexadecimal and nonzero trace/parent IDs are accepted.\nUnknown flag bits are preserved; sampled is bit zero. No tracing state is created.\n";

fn parse(args: Vec<String>) -> Result<Option<W3cSpanContext>, String> {
    let mut header = None;
    for arg in args {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(None);
            }
            "--json" => {}
            _ if arg.starts_with('-') => return Err(format!("unknown traceparent option '{arg}'")),
            _ => {
                if header.replace(arg).is_some() {
                    return Err("traceparent accepts one header".into());
                }
            }
        }
    }
    let header = header.ok_or("traceparent requires a header; see --help")?;
    parse_traceparent_55(header.as_bytes())
        .map(Some)
        .map_err(|error| format!("invalid traceparent: {error}"))
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let json = args.iter().any(|a| a == "--json");
    let context = match parse(args) {
        Ok(Some(context)) => context,
        Ok(None) => return Ok(()),
        Err(message) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version":1, "status":"invalid", "error":message
                    }))
                    .map_err(|e| e.to_string())?
                );
            }
            return Err(message);
        }
    };
    let canonical =
        String::from_utf8(context.to_traceparent().to_vec()).map_err(|e| e.to_string())?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version":1, "status":"valid", "version":"00",
                "trace_id":format!("{:032x}",context.trace_id()),
                "parent_span_id":format!("{:016x}",context.span_id()),
                "trace_flags":format!("{:02x}",context.trace_flags()),
                "sampled":context.sampled(), "canonical":canonical
            }))
            .map_err(|e| e.to_string())?
        );
    } else {
        println!("{canonical}");
    }
    Ok(())
}
