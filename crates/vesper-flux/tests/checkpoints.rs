use vesper_flux::*;
fn limits() -> Limits {
    Limits::default()
}
fn cp() -> CheckpointLimits {
    CheckpointLimits::default()
}
fn graph() -> Circuit {
    let mut b = CircuitBuilder::new(limits()).unwrap();
    let a = b.input(1).unwrap();
    let c = b.input(1).unwrap();
    let u = b.union(a, a).unwrap();
    let j = b.equijoin(u, c, 0, 0).unwrap();
    let out = b.equijoin(j, a, 0, 0).unwrap();
    b.finish(out).unwrap()
}
fn policy() -> CdcPolicy {
    CdcPolicy {
        bindings: vec![
            SourceBinding {
                partition_id: 3,
                schema_id: 10,
                input: 0,
                first_lsn: 0,
                weights: WeightPolicy::Unit,
            },
            SourceBinding {
                partition_id: 7,
                schema_id: 20,
                input: 1,
                first_lsn: 5,
                weights: WeightPolicy::NonZero,
            },
        ],
        max_batch_bytes: 4096,
        max_records: 32,
    }
}
fn executor() -> Executor {
    let mut e = graph().executor();
    e.bind_sources(&policy()).unwrap();
    e
}
fn header(partition: u32, schema: u16, lsn: u64, weight: i64) -> CdcHeader {
    CdcHeader {
        source_lsn: lsn,
        event_ts_ms: -55,
        z_weight: weight,
        partition_id: partition,
        schema_id: schema,
        flags: 0,
    }
}
fn batch(e: &Executor, a_lsn: u64, b_lsn: u64, wa: i64, wb: i64) -> Vec<u8> {
    let mut bytes = e.encode_cdc_record(header(3, 10, a_lsn, wa), &[2]).unwrap();
    bytes.extend(e.encode_cdc_record(header(7, 20, b_lsn, wb), &[2]).unwrap());
    bytes
}
fn resign(image: &mut [u8]) {
    let n = image.len() - 32;
    let checksum = blake3::hash(&image[..n]);
    image[n..].copy_from_slice(checksum.as_bytes());
}
fn single() -> Circuit {
    let mut b = CircuitBuilder::new(limits()).unwrap();
    let a = b.input(1).unwrap();
    b.finish(a).unwrap()
}
fn set(entries: &[(Vec<i64>, i64)]) -> ZSet {
    ZSet::consolidate(1, entries, limits()).unwrap()
}

#[test]
fn exact_32_byte_header_and_signed_wire_roundtrip() {
    let h = CdcHeader {
        source_lsn: 0x0102030405060708,
        event_ts_ms: i64::MIN,
        z_weight: -1,
        partition_id: 0x11223344,
        schema_id: 0x5566,
        flags: 0x7788,
    };
    let raw = h.encode();
    assert_eq!(raw.len(), 32);
    assert_eq!(&raw[..8], &[8, 7, 6, 5, 4, 3, 2, 1]);
    assert_eq!(
        &raw[24..],
        &[0x44, 0x33, 0x22, 0x11, 0x66, 0x55, 0x88, 0x77]
    );
    assert_eq!(CdcHeader::decode(&raw).unwrap(), h);
    for n in 0..32 {
        assert!(CdcHeader::decode(&raw[..n]).is_err());
    }
    assert!(CdcHeader::decode(&[0; 33]).is_err());
}
#[test]
fn resume_shared_join_retractions_equals_uninterrupted_and_batch() {
    let mut uninterrupted = executor();
    let circuit = graph();
    let mut inputs = vec![ZSet::empty(1, limits()).unwrap(); 2];
    let mut integrated = ZSet::empty(3, limits()).unwrap();
    let mut resumed = None;
    let weights = [(1, 3), (1, -1), (-1, 2), (1, -3), (-1, -1), (-1, 1)];
    for (i, (wa, wb)) in weights.into_iter().enumerate() {
        let bytes = batch(&uninterrupted, i as u64, 5 + i as u64, wa, wb);
        let result = uninterrupted.ingest_cdc(&bytes).unwrap();
        assert_eq!(result.records, 2);
        assert_eq!(result.max_event_ts_ms, -55);
        inputs[0] = inputs[0].plus(&set(&[(vec![2], wa)]), limits()).unwrap();
        inputs[1] = inputs[1].plus(&set(&[(vec![2], wb)]), limits()).unwrap();
        integrated = integrated.plus(&result.delta, limits()).unwrap();
        assert_eq!(integrated, circuit.batch(&inputs).unwrap());
        if let Some(e) = &mut resumed {
            let e: &mut Executor = e;
            assert_eq!(e.ingest_cdc(&bytes).unwrap().delta, result.delta);
            assert_eq!(e.materialized(), uninterrupted.materialized());
            assert_eq!(
                e.checkpoint(cp()).unwrap(),
                uninterrupted.checkpoint(cp()).unwrap()
            );
        }
        if i == 2 {
            let image = uninterrupted.checkpoint(cp()).unwrap();
            let mut fresh = executor();
            fresh.restore_checkpoint(&image, cp()).unwrap();
            resumed = Some(fresh);
        }
    }
    let mut resumed = resumed.unwrap();
    assert_eq!(resumed.ticks(), 6);
    assert_eq!(resumed.next_source_lsn(3), Some(6));
    assert_eq!(resumed.next_source_lsn(7), Some(11));
    assert!(resumed.materialized().is_empty());
    // A duplicated committed batch is rejected without changing checkpoint bytes.
    let before = resumed.checkpoint(cp()).unwrap();
    assert!(resumed.ingest_cdc(&batch(&resumed, 5, 10, 1, 1)).is_err());
    assert_eq!(before, resumed.checkpoint(cp()).unwrap());
}
#[test]
fn malformed_batches_and_failed_ticks_preserve_offsets_and_integrals() {
    let mut e = executor();
    let before = e.checkpoint(cp()).unwrap();
    let valid = batch(&e, 0, 5, 1, 2);
    for n in 1..valid.len() {
        if n == 40 {
            continue;
        }
        assert!(e.ingest_cdc(&valid[..n]).is_err(), "prefix {n}");
        assert_eq!(before, e.checkpoint(cp()).unwrap());
    }
    for (offset, value) in [(30, 1), (28, 99), (24, 9), (16, 0)] {
        let mut bad = valid.clone();
        bad[offset] = value;
        if offset == 16 {
            bad[16..24].fill(0);
        }
        assert!(e.ingest_cdc(&bad).is_err());
        assert_eq!(before, e.checkpoint(cp()).unwrap());
    }
    let mut gap = valid.clone();
    gap[..8].copy_from_slice(&1u64.to_le_bytes());
    assert!(e.ingest_cdc(&gap).is_err());
    assert!(e.ingest_cdc(&[]).is_err());
    assert_eq!(
        e.tick(&TickInput { deltas: vec![] }),
        Err(Error::SourceMode)
    );
    // Multiplication overflows at an actual differentiated join after decoding.
    let huge = batch(&e, 0, 5, 1, i64::MAX);
    assert_eq!(e.ingest_cdc(&huge).unwrap_err(), Error::Overflow);
    assert_eq!(before, e.checkpoint(cp()).unwrap());
    e.ingest_cdc(&valid).unwrap();
    assert_eq!(e.ticks(), 1);
    assert_eq!(e.next_source_lsn(3), Some(1));
}
#[test]
fn source_policy_offset_and_weight_edge_cases() {
    let mut e = single().executor();
    let mut p = CdcPolicy {
        bindings: vec![SourceBinding {
            partition_id: 1,
            schema_id: 1,
            input: 0,
            first_lsn: u64::MAX,
            weights: WeightPolicy::Unit,
        }],
        max_batch_bytes: 40,
        max_records: 1,
    };
    e.bind_sources(&p).unwrap();
    let raw = [header(1, 1, u64::MAX, 1).encode().as_slice(), &[0; 8]].concat();
    assert_eq!(e.ingest_cdc(&raw).unwrap_err(), Error::Overflow);
    assert_eq!(e.next_source_lsn(1), Some(u64::MAX));
    assert!(e.encode_cdc_record(header(1, 1, 0, 2), &[0]).is_err());
    assert!(e.encode_cdc_record(header(1, 1, 0, 1), &[]).is_err());
    assert!(e.bind_sources(&p).is_err());
    p.bindings[0].first_lsn = 0;
    p.bindings.push(p.bindings[0].clone());
    assert!(single().executor().bind_sources(&p).is_err());
    let mut e = executor();
    let mut raw = e.encode_cdc_record(header(3, 10, 0, 1), &[2]).unwrap();
    raw.extend(e.encode_cdc_record(header(3, 10, 0, -1), &[2]).unwrap());
    assert!(e.ingest_cdc(&raw).is_err());
    assert_eq!(e.ticks(), 0);
    // Two nonzero records can consolidate away while still committing their offsets.
    let mut raw = e.encode_cdc_record(header(3, 10, 0, 1), &[2]).unwrap();
    raw.extend(e.encode_cdc_record(header(3, 10, 1, -1), &[2]).unwrap());
    assert!(e.ingest_cdc(&raw).unwrap().delta.is_empty());
    assert_eq!(e.next_source_lsn(3), Some(2));
    e.checkpoint(cp()).unwrap();
}
#[test]
fn checksums_truncation_version_and_complete_graph_identity_are_transactional() {
    let mut e = executor();
    e.ingest_cdc(&batch(&e, 0, 5, 1, 2)).unwrap();
    let image = e.checkpoint(cp()).unwrap();
    let before = image.clone();
    for n in 0..image.len() {
        assert!(e.restore_checkpoint(&image[..n], cp()).is_err());
    }
    let mut bad = image.clone();
    let middle = bad.len() / 2;
    bad[middle] ^= 1;
    assert!(e.restore_checkpoint(&bad, cp()).is_err());
    for offset in [0, 8, 10, 12] {
        let mut bad = image.clone();
        bad[offset] ^= 1;
        resign(&mut bad);
        assert!(e.restore_checkpoint(&bad, cp()).is_err());
    }
    assert_eq!(before, e.checkpoint(cp()).unwrap());
    assert!(graph().executor().restore_checkpoint(&image, cp()).is_err()); // expected CDC policy must be bound first
    let mut p = policy();
    p.bindings[0].schema_id = 11;
    let mut other = graph().executor();
    other.bind_sources(&p).unwrap();
    assert!(other.restore_checkpoint(&image, cp()).is_err());
    // Same shape, different join key/constant/operator or output must not share identity.
    let mut b = CircuitBuilder::new(limits()).unwrap();
    let a = b.input(1).unwrap();
    let c = b.input(1).unwrap();
    let u = b.union(a, a).unwrap();
    let j = b.equijoin(u, c, 0, 0).unwrap();
    let mapped = b.map(a, vec![Field::Constant(2)]).unwrap();
    let out = b.equijoin(j, mapped, 0, 0).unwrap();
    let mut other = b.finish(out).unwrap().executor();
    other.bind_sources(&policy()).unwrap();
    assert!(other.restore_checkpoint(&image, cp()).is_err());
}
#[test]
fn canonical_rows_counts_and_semantic_state_validation_cannot_be_bypassed_by_rechecksum() {
    let mut e = single().executor();
    e.tick(&TickInput {
        deltas: vec![set(&[(vec![1], 1), (vec![2], 2)])],
    })
    .unwrap();
    let image = e.checkpoint(cp()).unwrap();
    for (offset, bytes) in [
        (57, 2u32.to_le_bytes().to_vec()),
        (61, u64::MAX.to_le_bytes().to_vec()),
        (77, 0i64.to_le_bytes().to_vec()),
        (85, 1i64.to_le_bytes().to_vec()),
        (44, 0u64.to_le_bytes().to_vec()),
    ] {
        let mut bad = image.clone();
        bad[offset..offset + bytes.len()].copy_from_slice(&bytes);
        resign(&mut bad);
        assert!(e.restore_checkpoint(&bad, cp()).is_err(), "offset {offset}");
    }
    let mut bad = image[..image.len() - 32].to_vec();
    bad.push(0);
    bad.extend([0; 32]);
    resign(&mut bad);
    assert!(e.restore_checkpoint(&bad, cp()).is_err());
    assert_eq!(e.checkpoint(cp()).unwrap(), image);
    let mut b = CircuitBuilder::new(limits()).unwrap();
    let a = b.input(1).unwrap();
    let root = b.union(a, a).unwrap();
    let mut e = b.finish(root).unwrap().executor();
    e.tick(&TickInput {
        deltas: vec![set(&[(vec![1], 1)])],
    })
    .unwrap();
    let image = e.checkpoint(cp()).unwrap();
    let mut bad = image.clone(); // second node starts after first node's 12+16 bytes
    bad[105..113].copy_from_slice(&9i64.to_le_bytes());
    resign(&mut bad);
    assert_eq!(
        e.restore_checkpoint(&bad, cp()),
        Err(Error::Checkpoint("inconsistent integral"))
    );
    assert_eq!(e.checkpoint(cp()).unwrap(), image);
}
#[test]
fn checkpoint_budgets_and_external_frontier_owners_are_not_restored() {
    let mut e = executor();
    e.ingest_cdc(&batch(&e, 0, 5, 1, 2)).unwrap();
    let image = e.checkpoint(cp()).unwrap();
    for small in [
        CheckpointLimits {
            max_bytes: 64,
            ..cp()
        },
        CheckpointLimits {
            max_total_rows: 0,
            ..cp()
        },
        CheckpointLimits {
            max_total_cells: 0,
            ..cp()
        },
        CheckpointLimits {
            max_nodes: 1,
            ..cp()
        },
        CheckpointLimits {
            max_sources: 0,
            ..cp()
        },
        CheckpointLimits {
            max_work: 0,
            ..cp()
        },
    ] {
        assert!(e.checkpoint(small).is_err());
        assert!(e.restore_checkpoint(&image, small).is_err());
    }
    let mut frontier = Frontier::new(0, 1).unwrap();
    let ticket = frontier.acquire(10).unwrap();
    e.restore_checkpoint(&image, cp()).unwrap();
    assert_eq!(frontier.live_holds(), 1);
    assert_eq!(frontier.watermark(), 10);
    frontier.retire(ticket).unwrap();
    assert_eq!(frontier.live_holds(), 0);
    e.restore_checkpoint(&image, cp()).unwrap();
    assert_eq!(frontier.live_holds(), 0);
}

#[test]
fn identity_covers_same_shape_predicates_constants_edges_output_and_limits() {
    fn circuit(
        value: i64,
        constant: i64,
        edge: bool,
        output_input: bool,
        query_limits: Limits,
    ) -> Circuit {
        let mut b = CircuitBuilder::new(query_limits).unwrap();
        let a = b.input(1).unwrap();
        let f = b
            .filter(a, Predicate::AtLeast { column: 0, value })
            .unwrap();
        let m = b
            .map(if edge { a } else { f }, vec![Field::Constant(constant)])
            .unwrap();
        let u = b.union(m, m).unwrap();
        b.finish(if output_input { a } else { u }).unwrap()
    }
    let c = circuit(0, 7, false, false, limits());
    let mut live = c.executor();
    live.tick(&TickInput {
        deltas: vec![set(&[(vec![1], 1)])],
    })
    .unwrap();
    let image = live.checkpoint(cp()).unwrap();
    let mut fresh = circuit(0, 7, false, false, limits()).executor();
    fresh.restore_checkpoint(&image, cp()).unwrap();
    for other in [
        circuit(1, 7, false, false, limits()),
        circuit(0, 8, false, false, limits()),
        circuit(0, 7, true, false, limits()),
        circuit(0, 7, false, true, limits()),
        circuit(
            0,
            7,
            false,
            false,
            Limits {
                max_output: 4095,
                ..limits()
            },
        ),
    ] {
        assert_eq!(
            other.executor().restore_checkpoint(&image, cp()),
            Err(Error::Checkpoint("identity"))
        );
    }
}

#[test]
fn source_offsets_must_agree_with_ticks_even_after_valid_rechecksum() {
    let mut e = executor();
    let image = e.checkpoint(cp()).unwrap();
    for next in [1u64, u64::MAX] {
        let mut bad = image.clone();
        bad[96..104].copy_from_slice(&next.to_le_bytes());
        resign(&mut bad);
        assert_eq!(
            e.restore_checkpoint(&bad, cp()),
            Err(Error::Checkpoint("offset tick count"))
        );
    }
    assert_eq!(e.checkpoint(cp()).unwrap(), image);
}

#[test]
fn ingestion_records_bytes_and_policy_limits_reject_transactionally() {
    let mut p = policy();
    p.max_records = 1;
    p.max_batch_bytes = 80;
    let mut e = graph().executor();
    e.bind_sources(&p).unwrap();
    let before = e.checkpoint(cp()).unwrap();
    let raw = batch(&e, 0, 5, 1, 1);
    assert_eq!(e.ingest_cdc(&raw).unwrap_err(), Error::Limit("CDC records"));
    let mut extra = raw.clone();
    extra.push(0);
    assert_eq!(e.ingest_cdc(&extra).unwrap_err(), Error::Limit("CDC bytes"));
    assert_eq!(e.checkpoint(cp()).unwrap(), before);
    for bad in [
        CdcPolicy {
            max_batch_bytes: 8 * 1024 * 1024 + 1,
            ..policy()
        },
        CdcPolicy {
            max_records: 65537,
            ..policy()
        },
        CdcPolicy {
            bindings: vec![],
            ..policy()
        },
    ] {
        assert!(graph().executor().bind_sources(&bad).is_err());
    }
}

#[test]
fn failed_late_integral_and_decode_work_limits_do_not_commit_source_offsets() {
    let mut e = single().executor();
    let p = CdcPolicy {
        bindings: vec![SourceBinding {
            partition_id: 1,
            schema_id: 1,
            input: 0,
            first_lsn: 0,
            weights: WeightPolicy::NonZero,
        }],
        max_batch_bytes: 1024,
        max_records: 8,
    };
    e.bind_sources(&p).unwrap();
    let first = e
        .encode_cdc_record(header(1, 1, 0, i64::MAX), &[1])
        .unwrap();
    e.ingest_cdc(&first).unwrap();
    let before = e.checkpoint(cp()).unwrap();
    let bad = e.encode_cdc_record(header(1, 1, 1, 1), &[1]).unwrap();
    assert_eq!(e.ingest_cdc(&bad).unwrap_err(), Error::Overflow);
    assert_eq!(e.next_source_lsn(1), Some(1));
    assert_eq!(e.checkpoint(cp()).unwrap(), before);
    let correction = e.encode_cdc_record(header(1, 1, 1, -1), &[1]).unwrap();
    e.ingest_cdc(&correction).unwrap();
    assert_eq!(e.next_source_lsn(1), Some(2));
    let mut b = CircuitBuilder::new(Limits {
        max_work: 0,
        ..limits()
    })
    .unwrap();
    let a = b.input(1).unwrap();
    let mut e = b.finish(a).unwrap().executor();
    e.bind_sources(&p).unwrap();
    assert_eq!(e.ingest_cdc(&first).unwrap_err(), Error::Limit("work"));
    assert_eq!(e.next_source_lsn(1), Some(0));
    assert_eq!(e.ticks(), 0);
    assert!(e.materialized().is_empty());
}

#[test]
fn checkpoint_preflights_exact_same_budget_restore_cost_before_emitting() {
    let mut e = single().executor();
    let unchanged = e.checkpoint(cp()).unwrap();
    // Regression: the old writer charged only 23 identity + 1 state operations,
    // while restoring also needs nine metadata reads. It must not emit at 24.
    for work in [24, 32] {
        let tight = CheckpointLimits {
            max_work: work,
            ..cp()
        };
        assert_eq!(e.checkpoint(tight), Err(Error::Limit("work")));
        assert_eq!(e.checkpoint(cp()).unwrap(), unchanged);
    }
    let exact = CheckpointLimits {
        max_work: 33,
        ..cp()
    };
    let image = e.checkpoint(exact).unwrap();
    let mut restored = single().executor();
    restored.restore_checkpoint(&image, exact).unwrap();
    assert_eq!(restored.checkpoint(exact).unwrap(), image);
    // One width-one row requires exactly two more charged reads, checked before
    // allocating the row. Failed tight restores retain the live nonempty owner.
    e.tick(&TickInput {
        deltas: vec![set(&[(vec![7], 1)])],
    })
    .unwrap();
    let nonempty = e.checkpoint(cp()).unwrap();
    assert_eq!(
        e.checkpoint(CheckpointLimits {
            max_work: 34,
            ..cp()
        }),
        Err(Error::Limit("work"))
    );
    assert_eq!(
        e.restore_checkpoint(
            &nonempty,
            CheckpointLimits {
                max_work: 34,
                ..cp()
            }
        ),
        Err(Error::Limit("work"))
    );
    assert_eq!(e.checkpoint(cp()).unwrap(), nonempty);
    let exact = CheckpointLimits {
        max_work: 35,
        ..cp()
    };
    let image = e.checkpoint(exact).unwrap();
    restored.restore_checkpoint(&image, exact).unwrap();
    assert_eq!(restored.materialized(), e.materialized());
    assert_eq!(restored.checkpoint(exact).unwrap(), image);
    // The exact preflight also includes source policy fields/next offsets.
    let policy = CdcPolicy {
        bindings: vec![SourceBinding {
            partition_id: 1,
            schema_id: 1,
            input: 0,
            first_lsn: 0,
            weights: WeightPolicy::Unit,
        }],
        max_batch_bytes: 1024,
        max_records: 8,
    };
    let mut source = single().executor();
    source.bind_sources(&policy).unwrap();
    let mut target = single().executor();
    target.bind_sources(&policy).unwrap();
    for (weight, boundary) in [(None, 50), (Some(1), 52)] {
        if let Some(weight) = weight {
            let bytes = source
                .encode_cdc_record(header(1, 1, 0, weight), &[7])
                .unwrap();
            source.ingest_cdc(&bytes).unwrap();
        }
        assert_eq!(
            source.checkpoint(CheckpointLimits {
                max_work: boundary - 1,
                ..cp()
            }),
            Err(Error::Limit("work"))
        );
        let exact = CheckpointLimits {
            max_work: boundary,
            ..cp()
        };
        let image = source.checkpoint(exact).unwrap();
        target.restore_checkpoint(&image, exact).unwrap();
        assert_eq!(target.checkpoint(exact).unwrap(), image);
    }
}
