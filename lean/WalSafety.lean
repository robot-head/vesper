-- WalSafety.lean (§8.3)
-- End-to-end transactional WAL safety proofs for `WriteTxn` and `appendBatch`:
-- * Lawful disjoint lens (`Broker.getIndexMetrics` / `Broker.setIndexMetrics`).
-- * `appendBatch_crc_err_aborts_cleanly`: on CRC validation failure, the WAL slot
--   is discarded, dirty buffer-pool slices are released, and the error propagates.
-- * `appendBatch_ok_commits_and_releases`: on valid CRC without counter overflow,
--   the batch commits cleanly with all pinned slices released.
--
-- No `sorry`, no `admit`, and no custom axioms are used.

namespace Vesper.WalSafety

inductive IoError where
  | crcMismatch
  | overflow
  | storageFull
  deriving DecidableEq, Repr

abbrev BufferPoolSlice := Nat
abbrev RecordBatch := List UInt8
abbrev SegmentedIndex := Nat

structure BrokerConfig where
  maxBatchBytes : Nat
  deriving DecidableEq, Repr

structure BrokerMetrics where
  batchesWritten : Nat
  deriving DecidableEq, Repr

structure Broker where
  index   : SegmentedIndex
  metrics : BrokerMetrics
  config  : BrokerConfig
  deriving DecidableEq, Repr

structure WriteTxn where
  walSlot    : Nat
  dirtyPages : BufferPoolSlice
  deriving DecidableEq, Repr

structure CommitReceipt where
  walSlot : Nat
  deriving DecidableEq, Repr

structure BrokerIndexMetricsProj where
  index          : SegmentedIndex
  batchesWritten : Nat
  deriving DecidableEq, Repr

/-- Synthesized lawful disjoint lens for `inout broker.{index, metrics}`. -/
@[simp] def Broker.getIndexMetrics (b : Broker) : BrokerIndexMetricsProj :=
  { index := b.index, batchesWritten := b.metrics.batchesWritten }

@[simp] def Broker.setIndexMetrics (b : Broker) (p : BrokerIndexMetricsProj) : Broker :=
  { b with index := p.index, metrics := { b.metrics with batchesWritten := p.batchesWritten } }

@[simp] theorem Broker.config_preserved_by_setIndexMetrics
    (b : Broker) (p : BrokerIndexMetricsProj) :
    (b.setIndexMetrics p).config = b.config := rfl

@[simp] theorem Broker.get_set_indexMetrics
    (b : Broker) (p : BrokerIndexMetricsProj) :
    (b.setIndexMetrics p).getIndexMetrics = p := rfl

structure RefStorageModel where
  activeWalSlots  : Nat
  pinnedSlices    : Nat
  flushedCount    : Nat
  recycledBatches : Nat
  deriving DecidableEq, Repr

abbrev EffM (α : Type) := RefStorageModel → RefStorageModel × Except IoError α

@[simp] def EffM.pure (a : α) : EffM α := fun st => (st, .ok a)

@[simp] def EffM.fail (e : IoError) : EffM α := fun st => (st, .error e)

@[simp] def EffM.bind (m : EffM α) (f : α → EffM β) : EffM β := fun st =>
  match m st with
  | (st', .ok a) => f a st'
  | (st', .error e) => (st', .error e)

@[simp] def beginTxn (maxBytes : Nat) : EffM WriteTxn := fun st =>
  ({ st with
      activeWalSlots := st.activeWalSlots + 1,
      pinnedSlices := st.pinnedSlices + 1 },
   .ok { walSlot := st.activeWalSlots, dirtyPages := maxBytes })

@[simp] def flushWal (_slot : Nat) (_pages : BufferPoolSlice) : EffM Unit := fun st =>
  ({ st with flushedCount := st.flushedCount + 1 }, .ok ())

@[simp] def discardWal (_slot : Nat) : EffM Unit := fun st =>
  ({ st with activeWalSlots := st.activeWalSlots - 1 }, .ok ())

@[simp] def releaseSlice (_pages : BufferPoolSlice) : EffM Unit := fun st =>
  ({ st with pinnedSlices := st.pinnedSlices - 1 }, .ok ())

@[simp] def recycleBatch (_batch : RecordBatch) : EffM Unit := fun st =>
  ({ st with recycledBatches := st.recycledBatches + 1 }, .ok ())

@[simp] def WriteTxn.commit (self : WriteTxn) : EffM CommitReceipt :=
  EffM.bind (flushWal self.walSlot self.dirtyPages) fun () =>
  EffM.bind (releaseSlice self.dirtyPages) fun () =>
  EffM.pure { walSlot := self.walSlot }

@[simp] def WriteTxn.abort (self : WriteTxn) : EffM Unit :=
  EffM.bind (discardWal self.walSlot) fun () =>
  releaseSlice self.dirtyPages

def validateCrc (batch : RecordBatch) : Except IoError Unit :=
  match batch with
  | [] => .error .crcMismatch
  | _  => .ok ()

@[simp] def insertInPlace (idx : SegmentedIndex) (batch : RecordBatch) : SegmentedIndex :=
  idx + batch.length

@[simp] def checkedAddU64 (a b : Nat) : Except IoError Nat :=
  if a + b < 2 ^ 64 then .ok (a + b) else .error .overflow

@[simp] def appendBatch
    (brokerProj : BrokerIndexMetricsProj)
    (config     : BrokerConfig)
    (batch      : RecordBatch)
    : EffM (CommitReceipt × BrokerIndexMetricsProj) :=
  EffM.bind (beginTxn config.maxBatchBytes) fun tx =>
  match validateCrc batch with
  | .ok () =>
      match checkedAddU64 brokerProj.batchesWritten 1 with
      | .ok written' =>
          EffM.bind (WriteTxn.commit tx) fun receipt =>
          EffM.pure (receipt, { index := insertInPlace brokerProj.index batch, batchesWritten := written' })
      | .error err =>
          EffM.bind (recycleBatch batch) fun () =>
          EffM.bind (WriteTxn.abort tx) fun () =>
          EffM.fail err
  | .error err =>
      EffM.bind (recycleBatch batch) fun () =>
      EffM.bind (WriteTxn.abort tx) fun () =>
      EffM.fail err

@[simp] def RefStorageModel.run (st : RefStorageModel) (m : EffM α) :
    RefStorageModel × Except IoError α :=
  m st

/-- §8.3 Transactional Crash Atomicity: on CRC error, allocated WAL slots are
discarded and pinned buffer-pool slices are completely released. -/
theorem appendBatch_crc_err_aborts_cleanly
    (st : RefStorageModel) (proj : BrokerIndexMetricsProj)
    (cfg : BrokerConfig) (batch : RecordBatch) (crcErr : IoError)
    (h_crc : validateCrc batch = .error crcErr) :
    let (st', res) := RefStorageModel.run st (appendBatch proj cfg batch)
    res = .error crcErr ∧
    st'.activeWalSlots = st.activeWalSlots ∧
    st'.pinnedSlices   = st.pinnedSlices := by
  simp only [RefStorageModel.run, appendBatch, EffM.bind, beginTxn, h_crc,
    recycleBatch, WriteTxn.abort, discardWal, releaseSlice, EffM.fail]
  dsimp only
  refine ⟨rfl, ?_, ?_⟩ <;> omega

/-- On valid CRC without overflow, `appendBatch` commits, increments `flushedCount`,
and releases all pinned slices. -/
theorem appendBatch_ok_commits_and_releases
    (st : RefStorageModel) (proj : BrokerIndexMetricsProj)
    (cfg : BrokerConfig) (batch : RecordBatch)
    (h_crc : validateCrc batch = .ok ())
    (h_no_ovf : proj.batchesWritten + 1 < 2 ^ 64) :
    let (st', res) := RefStorageModel.run st (appendBatch proj cfg batch)
    st'.pinnedSlices = st.pinnedSlices ∧
    st'.flushedCount = st.flushedCount + 1 ∧
    st'.activeWalSlots = st.activeWalSlots + 1 ∧
    ∃ receipt proj', res = .ok (receipt, proj') ∧
      proj'.batchesWritten = proj.batchesWritten + 1 := by
  simp only [RefStorageModel.run, appendBatch, EffM.bind, beginTxn, h_crc,
    checkedAddU64, ite_eq_left h_no_ovf, WriteTxn.commit, flushWal, releaseSlice, EffM.pure]
  dsimp only
  refine ⟨?_, rfl, rfl, ⟨{ walSlot := st.activeWalSlots },
    { index := insertInPlace proj.index batch, batchesWritten := proj.batchesWritten + 1 },
    rfl, rfl⟩⟩
  omega

end Vesper.WalSafety

#print axioms Vesper.WalSafety.Broker.config_preserved_by_setIndexMetrics
#print axioms Vesper.WalSafety.Broker.get_set_indexMetrics
#print axioms Vesper.WalSafety.appendBatch_crc_err_aborts_cleanly
#print axioms Vesper.WalSafety.appendBatch_ok_commits_and_releases
