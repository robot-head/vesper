//! Read-only validation of actual binary formats under explicit codec profiles.
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use vesper_grammar::{arrow::AlignmentProfile, der, ipc, protobuf};

const MAX_INPUT: usize = 8 * 1024 * 1024;
const MAX_SCHEMA: usize = 64 * 1024;
const MAX_OUTPUT: usize = 1024 * 1024;
const PROTOBUF_PROFILE: &str = "vesper.protobuf.explicit-presence/1";
const HELP: &str = r#"Usage: vesper grammar check FORMAT FILE [options]

Validate actual bytes; successful checks consume the entire input.
Formats: protobuf, der, arrow-ipc

  --schema FILE       Required strict JSON schema for protobuf
  --set-of            Interpret the DER root as SET OF supported values
  --simd-alignment    Require Arrow buffer offsets aligned to 64 bytes
  --json              One structured report, including failures
  -h, --help          Show help

Protobuf uses vesper.protobuf.explicit-presence/1, not a universal protobuf
canonicalization standard. Singular defaults preserve explicit presence.
DER supports a restricted universal-type profile; generic SET is unsupported.
Arrow IPC supports signed little-endian nullable i64, with ordinary eight-byte
relative alignment by default. IPC validation is not canonical serialization.
Summaries are bounded previews, not complete or signable decoded messages.
Input is limited to 8 MiB, schema to 64 KiB, output to 1 MiB; codec limits also
apply. Inputs are regular files without final symlinks. No file is modified.
No source macros, constant-time/SIMD execution or proof certificate is inferred.

Protobuf schema shape (unknown and duplicate decoded JSON keys are rejected):
{"profile":"vesper.protobuf.explicit-presence/1","root":0,"messages":[
  {"fields":[{"number":1,"kind":"u64","repeated":false}]}
]}
Kinds: u32, u64, sint32, sint64, bool, fixed32, fixed64, string, bytes, message.
The message kind requires an additional "message":N index; scalar kinds forbid it.
"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Format {
    Protobuf,
    Der,
    ArrowIpc,
}
impl Format {
    fn name(self) -> &'static str {
        match self {
            Self::Protobuf => "protobuf",
            Self::Der => "der",
            Self::ArrowIpc => "arrow-ipc",
        }
    }
}
struct Options {
    format: Format,
    input: PathBuf,
    schema: Option<PathBuf>,
    set_of: bool,
    simd_alignment: bool,
}
fn options(args: Vec<String>) -> Result<Option<Options>, String> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{HELP}");
        return Ok(None);
    }
    let mut args = args.into_iter();
    if args.next().as_deref() != Some("check") {
        return Err("grammar requires check; see grammar --help".into());
    }
    let format = match args.next().as_deref() {
        Some("protobuf") => Format::Protobuf,
        Some("der") => Format::Der,
        Some("arrow-ipc") => Format::ArrowIpc,
        _ => return Err("grammar check requires protobuf, der or arrow-ipc".into()),
    };
    let mut input = None;
    let mut schema = None;
    let mut set_of = false;
    let mut simd_alignment = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => {}
            "--schema" => {
                let path = args.next().ok_or("--schema requires a file")?;
                if schema.replace(PathBuf::from(path)).is_some() {
                    return Err("--schema may be supplied once".into());
                }
            }
            "--set-of" if !set_of => set_of = true,
            "--simd-alignment" if !simd_alignment => simd_alignment = true,
            _ if arg.starts_with('-') => return Err(format!("unknown grammar option '{arg}'")),
            _ => {
                if input.replace(PathBuf::from(arg)).is_some() {
                    return Err("grammar check accepts one input file".into());
                }
            }
        }
    }
    let input = input.ok_or("grammar check requires an input file")?;
    if (format == Format::Protobuf) != schema.is_some() {
        return Err("--schema is required for protobuf and unsupported for other formats".into());
    }
    if set_of && format != Format::Der {
        return Err("--set-of requires der".into());
    }
    if simd_alignment && format != Format::ArrowIpc {
        return Err("--simd-alignment requires arrow-ipc".into());
    }
    Ok(Some(Options {
        format,
        input,
        schema,
        set_of,
        simd_alignment,
    }))
}

fn read_bounded(path: &Path, max: usize) -> Result<Vec<u8>, String> {
    let fail = || {
        format!(
            "{}: expected a bounded regular file without final symlinks",
            path.display()
        )
    };
    if !fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .is_file()
    {
        return Err(fail());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > max as u64 {
        return Err(fail());
    }
    let mut bytes = Vec::new();
    file.take((max as u64).checked_add(1).ok_or("input limit overflow")?)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > max {
        return Err(fail());
    }
    Ok(bytes)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtoSchemaJson {
    profile: String,
    root: usize,
    messages: Vec<ProtoMessageJson>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtoMessageJson {
    fields: Vec<ProtoFieldJson>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtoFieldJson {
    number: u32,
    kind: ProtoKindJson,
    #[serde(default)]
    repeated: bool,
    #[serde(default, deserialize_with = "message_index")]
    message: Option<usize>,
}
fn message_index<'de, D: serde::Deserializer<'de>>(value: D) -> Result<Option<usize>, D::Error> {
    // A present null is not an absent member or a valid message index.
    usize::deserialize(value).map(Some)
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProtoKindJson {
    U32,
    U64,
    Sint32,
    Sint64,
    Bool,
    Fixed32,
    Fixed64,
    String,
    Bytes,
    Message,
}
fn schema_json(bytes: &[u8]) -> Result<ProtoSchemaJson, String> {
    let limits = vesper_serde::Limits {
        max_input_bytes: MAX_SCHEMA,
        max_depth: 32,
        max_tokens: 16_384,
        max_string_bytes: 1024,
        max_total_string_bytes: MAX_SCHEMA,
        max_work: 4 * 1024 * 1024,
        max_index_bytes: 1024 * 1024,
    };
    // Validate duplicates by decoded key equality before serde can overwrite
    // anything, and bound every unknown subtree before typed deserialization.
    vesper_serde::IndexedJson::parse(bytes, limits).map_err(|e| e.to_string())?;
    let schema: ProtoSchemaJson = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if schema.profile != PROTOBUF_PROFILE {
        return Err("unsupported protobuf schema profile".into());
    }
    if schema.messages.is_empty()
        || schema.messages.len() > 128
        || schema.root >= schema.messages.len()
    {
        return Err("protobuf schema message/root limit".into());
    }
    let mut count = 0usize;
    for message in &schema.messages {
        count = count
            .checked_add(message.fields.len())
            .ok_or("schema field overflow")?;
        if message.fields.len() > 128 || count > 1024 {
            return Err("protobuf schema field limit".into());
        }
        for field in &message.fields {
            if matches!(field.kind, ProtoKindJson::Message) != field.message.is_some() {
                return Err("only message fields require a message index".into());
            }
            if field
                .message
                .is_some_and(|index| index >= schema.messages.len())
            {
                return Err("protobuf schema message index out of range".into());
            }
        }
    }
    Ok(schema)
}

struct BoundedJson(Vec<u8>);
impl Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self
            .0
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("output size overflow"))?;
        if end > MAX_OUTPUT {
            return Err(io::Error::other("grammar output exceeds 1 MiB"));
        }
        self.0.try_reserve(bytes.len()).map_err(io::Error::other)?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encode_json(value: &impl Serialize) -> Result<Vec<u8>, String> {
    let mut output = BoundedJson(Vec::new());
    serde_json::to_writer(&mut output, value).map_err(|e| e.to_string())?;
    output.write_all(b"\n").map_err(|e| e.to_string())?;
    Ok(output.0)
}

fn output_buffer(size: usize) -> Result<Vec<u8>, String> {
    if size > MAX_INPUT {
        return Err("canonical re-encoding exceeds the CLI buffer limit".into());
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(size).map_err(|e| e.to_string())?;
    bytes.resize(size, 0);
    Ok(bytes)
}
fn bytes_preview(bytes: &[u8]) -> Json {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut prefix = String::with_capacity(64);
    for &byte in bytes.iter().take(32) {
        prefix.push(char::from(HEX[usize::from(byte >> 4)]));
        prefix.push(char::from(HEX[usize::from(byte & 15)]));
    }
    json!({"bytes":bytes.len(),"hex_prefix":prefix,"truncated":bytes.len()>32})
}
fn text_preview(text: &str) -> Json {
    let mut chars = text.chars();
    let prefix: String = chars.by_ref().take(128).collect();
    json!({"utf8_bytes":text.len(),"preview":prefix,"truncated":chars.next().is_some()})
}
struct Preview {
    remaining: usize,
    truncated: bool,
}
impl Preview {
    fn new() -> Self {
        Self {
            remaining: 256,
            truncated: false,
        }
    }
    fn take(&mut self) -> bool {
        if self.remaining == 0 {
            self.truncated = true;
            false
        } else {
            self.remaining -= 1;
            true
        }
    }
}
fn proto_preview(message: &protobuf::Message<'_>, preview: &mut Preview) -> Json {
    let mut fields = Vec::new();
    for field in message.fields.iter().take(16) {
        if !preview.take() {
            break;
        }
        use protobuf::Value as V;
        let value = match &field.value {
            V::U32(value) => json!({"kind":"u32","value":value.to_string()}),
            V::U64(value) => json!({"kind":"u64","value":value.to_string()}),
            V::Sint32(value) => json!({"kind":"sint32","value":value.to_string()}),
            V::Sint64(value) => json!({"kind":"sint64","value":value.to_string()}),
            V::Bool(value) => json!({"kind":"bool","value":value}),
            V::Fixed32(value) => json!({"kind":"fixed32","value":value.to_string()}),
            V::Fixed64(value) => json!({"kind":"fixed64","value":value.to_string()}),
            V::String(value) => {
                preview.truncated |= value.chars().nth(128).is_some();
                json!({"kind":"string","value":text_preview(value)})
            }
            V::Bytes(value) => {
                preview.truncated |= value.len() > 32;
                json!({"kind":"bytes","value":bytes_preview(value)})
            }
            V::Message(value) => json!({"kind":"message","value":proto_preview(value,preview)}),
        };
        fields.push(json!({"number":field.number,"value":value}));
    }
    let truncated = fields.len() != message.fields.len();
    preview.truncated |= truncated;
    json!({"field_occurrences":message.fields.len(),"fields":fields,"truncated":truncated})
}
fn protobuf_summary(input: &[u8], schema_bytes: &[u8]) -> Result<Json, String> {
    let raw = schema_json(schema_bytes)?;
    let mut messages = Vec::with_capacity(raw.messages.len());
    for message in raw.messages {
        let mut fields = Vec::with_capacity(message.fields.len());
        for field in message.fields {
            use protobuf::Kind as K;
            let kind = match field.kind {
                ProtoKindJson::U32 => K::U32,
                ProtoKindJson::U64 => K::U64,
                ProtoKindJson::Sint32 => K::Sint32,
                ProtoKindJson::Sint64 => K::Sint64,
                ProtoKindJson::Bool => K::Bool,
                ProtoKindJson::Fixed32 => K::Fixed32,
                ProtoKindJson::Fixed64 => K::Fixed64,
                ProtoKindJson::String => K::String,
                ProtoKindJson::Bytes => K::Bytes,
                ProtoKindJson::Message => K::Message(field.message.ok_or("missing message index")?),
            };
            fields.push(protobuf::FieldSchema {
                number: field.number,
                kind,
                repeated: field.repeated,
            });
        }
        messages.push(protobuf::MessageSchema { fields });
    }
    let schema = protobuf::Schema::validate(&messages, raw.root, protobuf::Limits::default())
        .map_err(|e| format!("protobuf schema: {e}"))?;
    let message = schema
        .parse_exact(input)
        .map_err(|e| format!("protobuf input: {e}"))?;
    let size = schema.encoded_size(&message).map_err(|e| e.to_string())?;
    let mut encoded = output_buffer(size)?;
    let written = schema
        .encode_into(&message, &mut encoded)
        .map_err(|e| e.to_string())?;
    if written != input.len() || encoded != input {
        return Err("protobuf accepted bytes did not roundtrip canonically".into());
    }
    let mut preview = Preview::new();
    let decoded = proto_preview(&message, &mut preview);
    Ok(json!({
        "profile":protobuf::PROFILE,"schema_blake3":blake3::hash(schema_bytes).to_hex().to_string(),
        "schema_messages":messages.len(),"root_message":raw.root,"roundtrip_bytes_equal":true,
        "projection":decoded,"projection_truncated":preview.truncated,
    }))
}
fn der_preview(node: der::Node<'_, '_>, preview: &mut Preview) -> Json {
    use der::Kind as K;
    preview.truncated |= match node.kind() {
        K::Integer | K::OctetString | K::ObjectIdentifier => node.content().len() > 32,
        K::BitString => node.content().len() > 33,
        _ => false,
    };
    let value = match node.kind() {
        K::Boolean => json!({"value":node.boolean()}),
        K::Integer => {
            json!({"value_i64":node.as_i64().ok().map(|v|v.to_string()),"twos_complement":bytes_preview(node.content())})
        }
        K::BitString => node
            .bit_string()
            .map(|(unused, bytes)| json!({"unused_bits":unused,"payload":bytes_preview(bytes)}))
            .unwrap_or(Json::Null),
        K::OctetString | K::ObjectIdentifier => bytes_preview(node.content()),
        K::Null => Json::Null,
        K::Sequence | K::SetOf => {
            let count = node.children().count();
            let mut children = Vec::new();
            for child in node.children().take(16) {
                if !preview.take() {
                    break;
                }
                children.push(der_preview(child, preview));
            }
            let truncated = children.len() != count;
            preview.truncated |= truncated;
            json!({"children":children,"child_count":count,"truncated":truncated})
        }
    };
    json!({"kind":format!("{:?}",node.kind()),"encoded_bytes":node.raw().len(),"value":value})
}
fn der_summary(input: &[u8], set_of: bool) -> Result<Json, String> {
    let any = der::Schema::Any;
    let schema = if set_of {
        der::Schema::SetOf(&any)
    } else {
        der::Schema::Any
    };
    let limits = der::Limits::default();
    let document =
        der::parse_exact(input, &schema, limits).map_err(|e| format!("DER input: {e}"))?;
    let mut encoded = output_buffer(input.len())?;
    let written = der::reencode_into(&document, &mut encoded, limits)
        .map_err(|e| format!("DER re-encoding: {e}"))?;
    if written != input.len() || encoded != input {
        return Err("DER accepted bytes did not roundtrip canonically".into());
    }
    let mut preview = Preview::new();
    let root = der_preview(document.root(), &mut preview);
    Ok(
        json!({"profile":"vesper.der.universal-subset/1","root_set_of":set_of,"nodes":document.node_count(),"roundtrip_bytes_equal":true,"projection":root,"projection_truncated":preview.truncated}),
    )
}
fn ipc_summary(input: &[u8], simd_alignment: bool) -> Result<Json, String> {
    let profile = if simd_alignment {
        AlignmentProfile::Relative64
    } else {
        AlignmentProfile::Relative8
    };
    let stream = ipc::parse_exact_with_alignment(input, ipc::Limits::default(), profile)
        .map_err(|e| format!("Arrow IPC input: {e}"))?;
    let version = match stream.schema().metadata_version() {
        3 => "V4",
        4 => "V5",
        _ => return Err("unsupported Arrow metadata version".into()),
    };
    let fields: Vec<_> = stream
        .schema()
        .fields()
        .iter()
        .take(16)
        .map(|f| json!({"name":text_preview(f.name()),"nullable":f.nullable()}))
        .collect();
    let mut batches = Vec::new();
    let mut total_rows = 0usize;
    for batch in stream.batches() {
        total_rows = total_rows
            .checked_add(batch.rows())
            .ok_or("IPC row count overflow")?;
    }
    for batch in stream.batches().iter().take(8) {
        let columns:Vec<_>=batch.columns().iter().take(8).map(|column|{
            let(sum,sum_error)=match column.checked_sum(1024*1024){Ok(value)=>(value.map(|n|n.to_string()),None),Err(e)=>(None,Some(e.to_string()))};
            let values:Vec<_>=(0..column.len().min(8)).map(|row|column.get(row).expect("validated row range").map(|v|v.to_string())).collect();
            json!({"rows":column.len(),"nulls":column.null_count(),"sum":sum,"sum_unavailable_reason":sum_error,"values":values,"values_truncated":column.len()>values.len()})
        }).collect();
        batches.push(json!({"rows":batch.rows(),"columns":columns,"columns_truncated":batch.columns().len()>8}));
    }
    Ok(
        json!({"profile":"arrow.ipc.signed-i64/1","canonical_serialization_claimed":false,"relative_buffer_alignment":if simd_alignment{64}else{8},"metadata_version":version,"metadata_version_discriminant":stream.schema().metadata_version(),"field_count":stream.schema().fields().len(),"fields":fields,"fields_truncated":stream.schema().fields().len()>16,"batch_count":stream.batches().len(),"total_rows":total_rows,"batches":batches,"batches_truncated":stream.batches().len()>8,"metadata_allocation_bytes":stream.metadata_allocation_bytes(),"work_used":stream.work_used()}),
    )
}
fn execute(args: Vec<String>) -> Result<Option<Json>, String> {
    let Some(options) = options(args)? else {
        return Ok(None);
    };
    let input = read_bounded(&options.input, MAX_INPUT)?;
    let result = match options.format {
        Format::Protobuf => {
            let path = options.schema.as_ref().ok_or("missing protobuf schema")?;
            protobuf_summary(&input, &read_bounded(path, MAX_SCHEMA)?)?
        }
        Format::Der => der_summary(&input, options.set_of)?,
        Format::ArrowIpc => ipc_summary(&input, options.simd_alignment)?,
    };
    Ok(Some(
        json!({"schema":"vesper.grammar/1","status":"valid","format":options.format.name(),"input":options.input,"input_bytes":input.len(),"input_blake3":blake3::hash(&input).to_hex().to_string(),"consumed_bytes":input.len(),"result":result}),
    ))
}
pub fn run(args: Vec<String>) -> Result<(), String> {
    let json_output = args.iter().any(|a| a == "--json");
    let result = execute(args);
    match result {
        Ok(None) => Ok(()),
        Ok(Some(data)) => {
            let bytes = encode_json(&data)?;
            if json_output {
                io::stdout()
                    .lock()
                    .write_all(&bytes)
                    .map_err(|e| e.to_string())?;
            } else {
                println!(
                    "Valid {}: {} bytes, exact consumption",
                    data["format"].as_str().unwrap_or("binary"),
                    data["input_bytes"]
                );
            }
            Ok(())
        }
        Err(error) => {
            if json_output {
                let bytes = encode_json(
                    &json!({"schema":"vesper.grammar/1","status":"invalid","error":error}),
                )?;
                io::stdout()
                    .lock()
                    .write_all(&bytes)
                    .map_err(|e| e.to_string())?;
            }
            Err(error)
        }
    }
}
