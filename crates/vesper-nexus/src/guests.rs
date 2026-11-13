//! Guest components and modules used by the tests and examples, written in
//! component and core text format so CI needs no guest toolchain.

/// Scenario numbers understood by [`BLOB_PLUGIN`]'s `run(scenario, arg)`.
pub mod scenario {
    /// ctor(7), borrow `tag`, `consume` the own: returns `tag * 1000 + consumed`.
    pub const HAPPY: u32 = 0;
    /// ctor, long `lend`, then `resource.drop` the own while lent: host traps.
    pub const DROP_WHILE_LENT: u32 = 1;
    /// ctor, `lend`, `unlend`, drop: returns unlend's result (1).
    pub const LEND_UNLEND_DROP: u32 = 2;
    /// ctor, drop, drop again: Wasmtime's handle table traps.
    pub const DOUBLE_DROP: u32 = 3;
    /// ctor(arg) and return its raw integer, keeping the own in the guest.
    pub const LEAK_RAW: u32 = 4;
    /// `peek-raw(arg)`: the tag, or -1 for any invalid integer.
    pub const PEEK_RAW: u32 = 5;
    /// ctor, `consume`, `consume` the moved handle again: Wasmtime traps.
    pub const USE_AFTER_MOVE: u32 = 6;
    /// ctor and return a `lend` ticket without ending it.
    pub const LEAK_LEND: u32 = 7;
    /// `unlend(arg)`: 1 if the ticket was this tenant's open lend.
    pub const UNLEND: u32 = 8;
    /// Busy loop `arg` iterations; returns `arg`.
    pub const SPIN: u32 = 9;
    /// Loop forever.
    pub const FOREVER: u32 = 10;
    /// ctor, drop, then borrow the dropped handle: Wasmtime traps.
    pub const BORROW_AFTER_DROP: u32 = 11;
}

/// A component implementing `world plugin` from [`crate::PLUGIN_WIT`].
pub const BLOB_PLUGIN: &str = r#"
(component
  (import "vesper:nexus/host@0.1.0" (instance $host
    (export "blob" (type $blob (sub resource)))
    (export "[constructor]blob" (func (param "tag" u32) (result (own $blob))))
    (export "[method]blob.tag" (func (param "self" (borrow $blob)) (result u32)))
    (export "[method]blob.lend" (func (param "self" (borrow $blob)) (result u64)))
    (export "[method]blob.raw" (func (param "self" (borrow $blob)) (result u64)))
    (export "unlend" (func (param "ticket" u64) (result bool)))
    (export "consume" (func (param "b" (own $blob)) (result u32)))
    (export "peek-raw" (func (param "raw" u64) (result s32)))
  ))
  (alias export $host "blob" (type $blob))
  (core func $ctor (canon lower (func $host "[constructor]blob")))
  (core func $tag (canon lower (func $host "[method]blob.tag")))
  (core func $lend (canon lower (func $host "[method]blob.lend")))
  (core func $raw (canon lower (func $host "[method]blob.raw")))
  (core func $unlend (canon lower (func $host "unlend")))
  (core func $consume (canon lower (func $host "consume")))
  (core func $peek (canon lower (func $host "peek-raw")))
  (core func $drop (canon resource.drop $blob))
  (core module $m
    (import "h" "ctor" (func $ctor (param i32) (result i32)))
    (import "h" "tag" (func $tag (param i32) (result i32)))
    (import "h" "lend" (func $lend (param i32) (result i64)))
    (import "h" "raw" (func $raw (param i32) (result i64)))
    (import "h" "unlend" (func $unlend (param i64) (result i32)))
    (import "h" "consume" (func $consume (param i32) (result i32)))
    (import "h" "peek" (func $peek (param i64) (result i32)))
    (import "h" "drop" (func $drop (param i32)))
    (func (export "run") (param $s i32) (param $arg i64) (result i64)
      (local $h i32) (local $t i32) (local $k i64) (local $i i64)
      ;; 0: happy path
      (if (i32.eq (local.get $s) (i32.const 0)) (then
        (local.set $h (call $ctor (i32.const 7)))
        (local.set $t (call $tag (local.get $h)))
        (return (i64.extend_i32_u (i32.add
          (i32.mul (local.get $t) (i32.const 1000))
          (call $consume (local.get $h)))))))
      ;; 1: drop while a host lend is outstanding
      (if (i32.eq (local.get $s) (i32.const 1)) (then
        (local.set $h (call $ctor (i32.const 1)))
        (drop (call $lend (local.get $h)))
        (call $drop (local.get $h))
        (return (i64.const 0))))
      ;; 2: lend, unlend, drop
      (if (i32.eq (local.get $s) (i32.const 2)) (then
        (local.set $h (call $ctor (i32.const 2)))
        (local.set $k (call $lend (local.get $h)))
        (local.set $t (call $unlend (local.get $k)))
        (call $drop (local.get $h))
        (return (i64.extend_i32_u (local.get $t)))))
      ;; 3: double drop
      (if (i32.eq (local.get $s) (i32.const 3)) (then
        (local.set $h (call $ctor (i32.const 3)))
        (call $drop (local.get $h))
        (call $drop (local.get $h))
        (return (i64.const 0))))
      ;; 4: leak an own into the guest and expose its raw integer
      (if (i32.eq (local.get $s) (i32.const 4)) (then
        (local.set $h (call $ctor (i32.wrap_i64 (local.get $arg))))
        (return (call $raw (local.get $h)))))
      ;; 5: resolve an arbitrary integer
      (if (i32.eq (local.get $s) (i32.const 5)) (then
        (return (i64.extend_i32_s (call $peek (local.get $arg))))))
      ;; 6: use after move
      (if (i32.eq (local.get $s) (i32.const 6)) (then
        (local.set $h (call $ctor (i32.const 6)))
        (drop (call $consume (local.get $h)))
        (drop (call $consume (local.get $h)))
        (return (i64.const 0))))
      ;; 7: leave a lend open
      (if (i32.eq (local.get $s) (i32.const 7)) (then
        (local.set $h (call $ctor (i32.const 7)))
        (return (call $lend (local.get $h)))))
      ;; 8: unlend an arbitrary ticket
      (if (i32.eq (local.get $s) (i32.const 8)) (then
        (return (i64.extend_i32_u (call $unlend (local.get $arg))))))
      ;; 9: bounded busy loop
      (if (i32.eq (local.get $s) (i32.const 9)) (then
        (block $done
          (loop $l
            (br_if $done (i64.ge_u (local.get $i) (local.get $arg)))
            (local.set $i (i64.add (local.get $i) (i64.const 1)))
            (br $l)))
        (return (local.get $i))))
      ;; 10: unbounded loop
      (if (i32.eq (local.get $s) (i32.const 10)) (then
        (loop $l (br $l))))
      ;; 11: borrow after drop
      (if (i32.eq (local.get $s) (i32.const 11)) (then
        (local.set $h (call $ctor (i32.const 11)))
        (call $drop (local.get $h))
        (return (i64.extend_i32_u (call $tag (local.get $h))))))
      (i64.const -2))
  )
  (core instance $hi
    (export "ctor" (func $ctor))
    (export "tag" (func $tag))
    (export "lend" (func $lend))
    (export "raw" (func $raw))
    (export "unlend" (func $unlend))
    (export "consume" (func $consume))
    (export "peek" (func $peek))
    (export "drop" (func $drop)))
  (core instance $i (instantiate $m (with "h" (instance $hi))))
  (func (export "run") (param "scenario" u32) (param "arg" u64) (result s64)
    (canon lift (core func $i "run")))
)
"#;
