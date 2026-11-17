//! OTLP (protobuf) export of the same Beacon data the OTAP encoders carry,
//! so a stock collector can receive Vesper telemetry without the Arrow
//! receiver.
//!
//! [`traces_request`], [`metrics_request`] and [`logs_request`] build the
//! collector service requests from the grouped model. They use the same
//! exported attributes as OTAP (the PII boundary is unchanged: only stored
//! [`AttributeValue`](crate::otap::AttributeValue)s exist). Consecutive
//! points of one scope with the same name, unit and kind become one
//! `Metric` with several data points ([`metric_points_request`] covers
//! gauges, sums, histograms and exponential histograms with exemplars). The gRPC client and receiver live in
//! [`crate::grpc`] (feature `grpc`).

pub mod proto;

use crate::otap::{
    buckets, exported_attributes, to_metric_groups, AttributeValue, Attributes, EncodeError,
    Exemplar, ExpBuckets, ExpHistogramPoint, HistogramPoint, LogRecord, MetricKey, MetricPoint,
    NumberPoint, NumberValue, OtapSpan, Resource, ResourceGroup, Scope,
};
use crate::otap::{error_message, schema, span_name};
use proto::*;
use vesper_beacon::SpanStatus;

fn any(value: &AttributeValue) -> AnyValue {
    let v = match value {
        AttributeValue::Bool(b) => any_value::Value::BoolValue(*b),
        AttributeValue::Int(i) => any_value::Value::IntValue(*i),
        AttributeValue::Double(d) => any_value::Value::DoubleValue(*d),
        AttributeValue::Bytes(b) => any_value::Value::BytesValue(b.to_vec()),
        AttributeValue::Slice(items) => any_value::Value::ArrayValue(ArrayValue {
            values: items.iter().map(any).collect(),
        }),
        AttributeValue::Map(entries) => any_value::Value::KvlistValue(KeyValueList {
            values: entries
                .iter()
                .map(|(k, v)| KeyValue {
                    key: (*k).to_owned(),
                    value: Some(any(v)),
                })
                .collect(),
        }),
        other => any_value::Value::StringValue(other.export_str().expect("string forms")),
    };
    AnyValue { value: Some(v) }
}
fn key_values(attrs: &Attributes) -> Vec<KeyValue> {
    attrs
        .iter()
        .map(|(k, v)| KeyValue {
            key: k.to_owned(),
            value: Some(any(&v)),
        })
        .collect()
}
fn resource(r: &Resource) -> proto::Resource {
    proto::Resource {
        attributes: key_values(&r.attributes),
        dropped_attributes_count: r.dropped_attributes_count,
    }
}
fn scope(s: &Scope) -> InstrumentationScope {
    InstrumentationScope {
        name: s.name.unwrap_or("").to_owned(),
        version: s.version.unwrap_or("").to_owned(),
        attributes: key_values(&s.attributes),
        dropped_attributes_count: s.dropped_attributes_count,
    }
}
fn be(v: impl Into<u128>, width: usize) -> Vec<u8> {
    v.into().to_be_bytes()[16 - width..].to_vec()
}

fn span(s: &OtapSpan) -> Result<Span, EncodeError> {
    let r = &s.record;
    let end = r
        .start_unix_ns
        .checked_add(r.duration_ns)
        .ok_or(EncodeError::ValueOutOfRange {
            column: "end_time_unix_nano",
        })?;
    let status = match r.status {
        SpanStatus::Ok => Status {
            message: String::new(),
            code: schema::STATUS_CODE_OK,
        },
        SpanStatus::Error(code) => Status {
            message: error_message(code).to_owned(),
            code: schema::STATUS_CODE_ERROR,
        },
    };
    Ok(Span {
        trace_id: be(r.context.trace_id(), 16),
        span_id: be(r.context.span_id(), 8),
        trace_state: s
            .trace_state
            .as_ref()
            .map_or(String::new(), |t| t.as_str().to_owned()),
        parent_span_id: if r.parent_span_id == 0 {
            Vec::new()
        } else {
            be(r.parent_span_id, 8)
        },
        flags: u32::from(r.context.trace_flags()),
        name: span_name(r.schema).to_owned(),
        kind: s.kind as i32,
        start_time_unix_nano: r.start_unix_ns,
        end_time_unix_nano: end,
        attributes: key_values(&exported_attributes(s)?),
        dropped_attributes_count: s.dropped_attributes_count,
        events: s
            .events
            .iter()
            .map(|e| span::Event {
                time_unix_nano: e.time_unix_ns,
                name: e.name.to_owned(),
                attributes: key_values(&e.attributes),
                dropped_attributes_count: e.dropped_attributes_count,
            })
            .collect(),
        dropped_events_count: s.dropped_events_count,
        links: s
            .links
            .iter()
            .map(|l| span::Link {
                trace_id: be(l.trace_id, 16),
                span_id: be(l.span_id, 8),
                trace_state: l
                    .trace_state
                    .as_ref()
                    .map_or(String::new(), |t| t.as_str().to_owned()),
                attributes: key_values(&l.attributes),
                dropped_attributes_count: l.dropped_attributes_count,
                flags: l.flags.unwrap_or(0),
            })
            .collect(),
        dropped_links_count: s.dropped_links_count,
        status: Some(status),
    })
}

/// `ExportTraceServiceRequest` for grouped spans.
pub fn traces_request(
    groups: &[ResourceGroup<OtapSpan>],
) -> Result<ExportTraceServiceRequest, EncodeError> {
    let mut resource_spans = Vec::with_capacity(groups.len());
    for g in groups {
        let mut scope_spans = Vec::with_capacity(g.scopes.len());
        for sg in &g.scopes {
            scope_spans.push(ScopeSpans {
                scope: Some(scope(&sg.scope)),
                spans: sg.items.iter().map(span).collect::<Result<_, _>>()?,
                schema_url: sg.scope.schema_url.unwrap_or("").to_owned(),
            });
        }
        resource_spans.push(ResourceSpans {
            resource: Some(resource(&g.resource)),
            scope_spans,
            schema_url: g.resource.schema_url.unwrap_or("").to_owned(),
        });
    }
    Ok(ExportTraceServiceRequest { resource_spans })
}

fn exemplar(e: &Exemplar) -> proto::Exemplar {
    proto::Exemplar {
        filtered_attributes: key_values(&e.filtered_attributes),
        time_unix_nano: e.time_unix_ns,
        value: Some(match e.value {
            NumberValue::Int(v) => exemplar::Value::AsInt(v),
            NumberValue::Double(v) => exemplar::Value::AsDouble(v),
        }),
        span_id: e.span_id.map_or(Vec::new(), |s| be(s, 8)),
        trace_id: e.trace_id.map_or(Vec::new(), |t| be(t, 16)),
    }
}

fn histogram_point(p: &HistogramPoint<'_>) -> HistogramDataPoint {
    let (bucket_counts, explicit_bounds) = buckets(p.histogram);
    HistogramDataPoint {
        attributes: key_values(&p.attributes),
        start_time_unix_nano: p.start_unix_ns,
        time_unix_nano: p.time_unix_ns,
        count: p.histogram.total_count(),
        sum: Some(p.histogram.sum() as f64),
        bucket_counts,
        explicit_bounds,
        exemplars: p.exemplars.iter().map(exemplar).collect(),
        flags: 0,
        min: p.histogram.min().map(|v| v as f64),
        max: p.histogram.max().map(|v| v as f64),
    }
}

fn number_point(p: &NumberPoint) -> NumberDataPoint {
    NumberDataPoint {
        attributes: key_values(&p.attributes),
        start_time_unix_nano: p.start_unix_ns.unwrap_or(0),
        time_unix_nano: p.time_unix_ns,
        value: Some(match p.value {
            NumberValue::Int(v) => number_data_point::Value::AsInt(v),
            NumberValue::Double(v) => number_data_point::Value::AsDouble(v),
        }),
        exemplars: p.exemplars.iter().map(exemplar).collect(),
        flags: p.flags,
    }
}

fn exp_point(p: &ExpHistogramPoint) -> ExponentialHistogramDataPoint {
    let side = |b: &ExpBuckets| {
        Some(Buckets {
            offset: b.offset,
            bucket_counts: b.counts.clone(),
        })
    };
    ExponentialHistogramDataPoint {
        attributes: key_values(&p.attributes),
        start_time_unix_nano: p.start_unix_ns.unwrap_or(0),
        time_unix_nano: p.time_unix_ns,
        count: p.count,
        sum: p.sum,
        scale: p.scale,
        zero_count: p.zero_count,
        positive: side(&p.positive),
        negative: side(&p.negative),
        flags: p.flags,
        exemplars: p.exemplars.iter().map(exemplar).collect(),
        min: p.min,
        max: p.max,
        zero_threshold: p.zero_threshold,
    }
}

/// A new, empty `Metric` for the row `key` describes.
fn metric_for(key: MetricKey) -> Metric {
    let temporality = key.temporality.unwrap_or(0);
    Metric {
        name: key.name.to_owned(),
        description: key.description.unwrap_or("").to_owned(),
        unit: key.unit.to_owned(),
        data: Some(match key.metric_type {
            schema::METRIC_TYPE_GAUGE => metric::Data::Gauge(Gauge {
                data_points: Vec::new(),
            }),
            schema::METRIC_TYPE_SUM => metric::Data::Sum(Sum {
                data_points: Vec::new(),
                aggregation_temporality: temporality,
                is_monotonic: key.monotonic.unwrap_or(false),
            }),
            schema::METRIC_TYPE_HISTOGRAM => metric::Data::Histogram(Histogram {
                data_points: Vec::new(),
                aggregation_temporality: temporality,
            }),
            _ => metric::Data::ExponentialHistogram(ExponentialHistogram {
                data_points: Vec::new(),
                aggregation_temporality: temporality,
            }),
        }),
    }
}

fn push_point(metric: &mut Metric, p: &MetricPoint<'_>) {
    match (metric.data.as_mut().expect("set by metric_for"), p) {
        (metric::Data::Gauge(g), MetricPoint::Number(p)) => g.data_points.push(number_point(p)),
        (metric::Data::Sum(s), MetricPoint::Number(p)) => s.data_points.push(number_point(p)),
        (metric::Data::Histogram(h), MetricPoint::Histogram(p)) => {
            h.data_points.push(histogram_point(p))
        }
        (metric::Data::ExponentialHistogram(h), MetricPoint::ExpHistogram(p)) => {
            h.data_points.push(exp_point(p))
        }
        _ => unreachable!("metric rows are keyed by point kind"),
    }
}

/// `ExportMetricsServiceRequest` for grouped cumulative histograms.
pub fn metrics_request(
    groups: &[ResourceGroup<HistogramPoint<'_>>],
) -> ExportMetricsServiceRequest {
    metric_points_request(&to_metric_groups(groups))
}

/// `ExportMetricsServiceRequest` for grouped points of every kind:
/// consecutive points of one scope with the same name, unit, description,
/// kind and temporality share one `Metric`, as in OTAP. Exponential
/// histogram points are exported as given (OTAP export also checks them).
pub fn metric_points_request(
    groups: &[ResourceGroup<MetricPoint<'_>>],
) -> ExportMetricsServiceRequest {
    let resource_metrics = groups
        .iter()
        .map(|g| ResourceMetrics {
            resource: Some(resource(&g.resource)),
            scope_metrics: g
                .scopes
                .iter()
                .map(|sg| {
                    let mut metrics: Vec<Metric> = Vec::new();
                    let mut prev: Option<MetricKey> = None;
                    for p in &sg.items {
                        let key = p.key();
                        if prev != Some(key) {
                            metrics.push(metric_for(key));
                            prev = Some(key);
                        }
                        push_point(metrics.last_mut().expect("pushed"), p);
                    }
                    ScopeMetrics {
                        scope: Some(scope(&sg.scope)),
                        metrics,
                        schema_url: sg.scope.schema_url.unwrap_or("").to_owned(),
                    }
                })
                .collect(),
            schema_url: g.resource.schema_url.unwrap_or("").to_owned(),
        })
        .collect();
    ExportMetricsServiceRequest { resource_metrics }
}

fn log(l: &LogRecord) -> proto::LogRecord {
    proto::LogRecord {
        time_unix_nano: l.time_unix_ns.unwrap_or(0),
        observed_time_unix_nano: l.observed_time_unix_ns.unwrap_or(0),
        severity_number: i32::from(l.severity.number()),
        severity_text: l.severity_text.unwrap_or("").to_owned(),
        body: l.body.as_ref().map(any),
        attributes: key_values(&l.attributes),
        dropped_attributes_count: l.dropped_attributes_count,
        flags: l.flags,
        trace_id: l.trace_id.map_or(Vec::new(), |t| be(t, 16)),
        span_id: l.span_id.map_or(Vec::new(), |s| be(s, 8)),
        event_name: l.event_name.unwrap_or("").to_owned(),
    }
}

/// `ExportLogsServiceRequest` for grouped log records.
pub fn logs_request(groups: &[ResourceGroup<LogRecord>]) -> ExportLogsServiceRequest {
    ExportLogsServiceRequest {
        resource_logs: groups
            .iter()
            .map(|g| ResourceLogs {
                resource: Some(resource(&g.resource)),
                scope_logs: g
                    .scopes
                    .iter()
                    .map(|sg| ScopeLogs {
                        scope: Some(scope(&sg.scope)),
                        log_records: sg.items.iter().map(log).collect(),
                        schema_url: sg.scope.schema_url.unwrap_or("").to_owned(),
                    })
                    .collect(),
                schema_url: g.resource.schema_url.unwrap_or("").to_owned(),
            })
            .collect(),
    }
}
