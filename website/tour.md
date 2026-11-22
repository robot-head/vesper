# Linear ownership

## Require a decision at scope exit

A linear value must be consumed on every path. The editor creates a ticket and returns with it still live. Look for `E0302`, then destructure `t` with `let Ticket { seat } = t;` and return the seat number. Select Run to see `3`.

~~~vesper E0302
linear struct Ticket {
    seat: u64
}
fn main() -> u64 {
    let t = Ticket {
        seat: 3
    };
    0
}
~~~

~~~vesper
linear struct Ticket {
    seat: u64
}

fn main() -> u64 {
    let t = Ticket {
        seat: 3
    };
    let Ticket {
        seat
    } = t;
    seat
}
~~~

## Finish on both paths

When the customer attends, `redeem` consumes the ticket and returns the seat. The other branch leaves it live. Destructure `ticket` in the `else` branch so both branches account for it. Run with `true` to get `3` and with `false` to get `0`.

~~~vesper
linear struct Ticket {
    seat: u64
}
impl Ticket {
    fn redeem(sink self) -> u64 {
        let Ticket {
            seat
        }
        = self;
        seat
    }
}

fn finish(sink ticket: Ticket, attended: bool) -> u64 {
    if attended {
        ticket.redeem()
    } else {
        let Ticket {
            seat
        }
        = ticket;
        0
    }
}
fn main() -> u64 {
    finish(
        Ticket {
            seat: 3
        },
        true
    )
}
~~~

~~~vesper E0309
linear struct Ticket {
    seat: u64
}
impl Ticket {
    fn redeem(sink self) -> u64 {
        let Ticket {
            seat
        }
        = self;
        seat
    }
}

fn finish(sink ticket: Ticket, attended: bool) -> u64 {
    if attended {
        ticket.redeem()
    } else {
        0
    }
}
fn main() -> u64 {
    finish(
        Ticket {
            seat: 3
        },
        true
    )
}
~~~

# Field projections

## Limit a helper to one field

A field projection in the signature names the part of the record a helper may change. The caller can lend `seats` for reading during the same call because the writable field is `used`.

~~~vesper
copy struct Room {
    seats: u64,
    used: u64
}
fn fill(inout room.{used}: Room, in limit: u64) {
    room.used = limit;
}
fn main() -> u64 {
    let mut room = Room {
        seats: 8,
        used: 2
    };
    fill(inout room, in room.seats);
    room.used
}
~~~

## Find a conflicting loan

The first argument lends only `room.used`, because that is the projection named in `transfer`. The second argument asks for exclusive access to the same field. Hover over the underlined argument to see `E0301` and where the earlier loan began. Change the second argument to `inout room.seats` to give the call two disjoint loans.

~~~vesper E0301
copy struct Room {
    seats: u64,
    used: u64,
}

fn transfer(inout room.{used}: Room, inout other: u64) {
    room.used = other;
    other = 0;
}

fn main() -> u64 {
    let mut room = Room {
        seats: 8,
        used: 2
    };
    transfer(inout room, inout room.used);
    room.used
}
~~~

~~~vesper
copy struct Room {
    seats: u64,
    used: u64,
}

fn transfer(inout room.{used}: Room, inout other: u64) {
    room.used = other;
    other = 0;
}

fn main() -> u64 {
    let mut room = Room {
        seats: 8,
        used: 2
    };
    transfer(inout room, inout room.seats);
    room.used
}
~~~

# Test sugar

## Isolate one failing row

Vesper expands each `// @case` row into a named test. The rows share one assertion. Run `test_reserve::over` to see its failure. It expects three seats when the function correctly returns two. Fix the expected value, then change the test name to `test_reserve::exact` and run the other row.

~~~vesper
fn reserve(wanted: u64, free: u64) -> u64 {
    if wanted > free {
        free
    } else {
        wanted
    }
}
test {
    // @case over(3, 2, 3)
    // @case exact(2, 2, 2)
    fn test_reserve(wanted: u64, free: u64, expected: u64) -> bool {
        reserve(wanted, free) == expected
    }
}
~~~

# Lexical effects

## Pass an effect to the caller

The editor calls `next` from `advance`. Since addition can overflow, the call performs `Fail[ArithOverflow]`. Add `eff [Fail[ArithOverflow]]` to `advance` to let it pass the effect to its caller.

~~~vesper E0401
fn next(n: i64) -> i64 eff [Fail[ArithOverflow]] {
    n + 1
}

fn advance(n: i64) -> i64 {
    next(n)
}
~~~

~~~vesper
fn next(n: i64) -> i64 eff [Fail[ArithOverflow]] {
    n + 1
}

fn advance(n: i64) -> i64 eff [Fail[ArithOverflow]] {
    next(n)
}
~~~

## Exercise the failure path

Adding one to `MAX_I64` triggers overflow. The editor calls `next` with no handler. Wrap the call in `handle Fail[ArithOverflow] in { … } with { … }`. Use `return value => value` for success and `raise(error, _) => 0` for overflow. Once the checker accepts the program, run it to see the handler return `0`.

~~~vesper E0401
fn next(n: i64) -> i64 eff [Fail[ArithOverflow]] {
    n + 1
}

const MAX_I64: i64 = 9223372036854775807;

fn main() -> i64 {
    next(MAX_I64)
}
~~~

~~~vesper
fn next(n: i64) -> i64 eff [Fail[ArithOverflow]] {
    n + 1
}

const MAX_I64: i64 = 9223372036854775807;

fn main() -> i64 {
    handle Fail[ArithOverflow] in {
        next(MAX_I64)
    } with {
        return value => value,
        raise(error, _) => 0,
    }
}
~~~

## Use the resumed result

A reservation expires at time `100`. Testing that boundary against the wall clock can produce different results from one run to the next. Here `eff [Clock]` declares that `expired` asks for the time. A local handler supplies the readings.

The `now` arm calls `resume` with two values. The first is the current reading, which becomes the result of `Clock::now()`. The second is the handler state for the next operation. A starting tick of `99` produces `99` and then `100`. Change it to `98` and run the test again.

~~~vesper
linear effect Clock {
    fn now() -> i64;
}

fn expired(deadline: i64) -> bool eff [Clock] {
    Clock::now() >= deadline
}

test {
    fn test_crosses_deadline() -> bool {
        handle Clock state tick = 99 in {
            let before = expired(100);
            let after = expired(100);
            !before && after
        } with {
            return (crossed, _) => crossed,
            now(tick, resume) => resume(tick, wrapping_add(tick, 1)),
        }
    }
}
~~~

The separation also applies to operations that may suspend. Callers of such an effect use an ordinary function call and declare or handle the effect. The clock test uses local handler state. When you need parallel work, Pulse uses fibers scheduled on worker threads. The runtime chapter shows how to launch work in a nursery, join the children, and lend disjoint fields to parallel workers.

<!-- figure:pulse-guide -->

## Prove a divisor is nonzero

The condition gives the checker a fact about `n` on the division path. Remove the condition and observe the missing failure effect.

~~~vesper
fn divide(n: u64) -> u64 {
    if n > 0 { 84 / n } else { 0 }
}

fn main() -> u64 {
    divide(2)
}
~~~

# Allocation contracts

## Allocate in a lexical region

The box belongs to `scratch`. The checker rejects the attempt to let it outlive that region. Read it inside the block with `scratch.get(value)` so only the integer leaves the region.

~~~vesper E0791
fn main() -> i64 {
    let result = region scratch(1024) {
        let value = scratch.box(42);
        value
    };
    result
}
~~~

~~~vesper
fn main() -> i64 {
    let result = region scratch(1024) {
        let value = scratch.box(42);
        scratch.get(value)
    };
    result
}
~~~

## Carry an existing allocation forward

The `@fip` contract checks whether a function can reuse memory in place. The editor attempts to create a new box while it still owns the old one. Unpack `b` with a `Box(value)` pattern before constructing the replacement. Unpacking makes a reuse slot available to the checker.

~~~vesper E0804
@fip fn replace(sink b: Box[i64]) -> Box[i64] {
    Box::new(41)
}
~~~

~~~vesper
@fip fn replace(sink b: Box[i64]) -> Box[i64] {
    match b {
        Box(value) => Box::new(wrapping_add(value, 1)),
    }
}
~~~

## Group hot fields into tiles

The hot field is grouped across 16 points in a tile. The cold field stays out of that group. Remove the field annotations to see the layout diagnostic.

~~~vesper
copy tiled(16) struct Point {
    hot x: i64,
    cold y: i64,
}
~~~

# Capabilities and verification

## Check a masked vector load

Read up to 16 bytes at an offset using the baseline ISA profile. The masked load supplies zero for inactive lanes. Remove the capability from the effect row to see what the caller needs to supply.

~~~vesper
fn chunk(src: String, offset: u64) -> Vec[u8, 16] eff [Isa[Baseline128]] {
    Vec::load_masked[u8, 16](src, offset)
}
~~~

## Extract a Lean model

Select Extract Lean and find the definition for `advance`. It expresses the update to `index` as a returned value in the model. Change what the function writes and extract again. The generated definitions describe the source behavior that a separate Lean theorem can reason about.

~~~vesper
fn advance(inout index: u64, in config: u64) {
    index = wrapping_add(index, config);
}

fn main() -> u64 {
    let mut index: u64 = 1;
    advance(inout index, in 41);
    index
}
~~~

# Define syntax

## Define a new expression form

A Forge macro is invoked with `name!(...)`. A `sugar` declaration can add a grammar pattern. This rule looks for `twice`, an expression, and a block. It replaces the form with ordinary Vesper. Select Run to get `42`, then select Expand to see how Forge evaluates the captured expression once. Change the whitespace around the rule or call and select Format to see how Stylus lays out both.

~~~vesper
sugar twice: expr {
    rule (twice $x:expr $body:block) => { wrapping_add($x, $x) }
}

fn main() -> i64 {
    twice 21 {}
}
~~~

## Define your own sugar

This declaration adds `.+` as a left-associative operator with additive precedence. Its rule captures expressions on both sides and expands to wrapping addition. Change the template to multiplication, then select Run and Expand to check the new behavior.

~~~vesper
sugar plus_wrap: infix(`.+`, additive, left) {
    rule ($a:expr .+ $b:expr) => { wrapping_add($a, $b) }
}

fn main() -> i64 {
    41 .+ 1
}
~~~
