# First programs

## Return a number

Save this program as `lesson.ves`. The type after the arrow says what `main` returns. Here `i64` means a signed 64-bit integer.

~~~vesper
fn main() -> i64 {
    7
}
~~~

From the repository root, run the command below. The evaluator prints `7`. Change the number and run it again. The tour uses `vesper` as shorthand for this executable.

~~~sh
target/debug/vesper run lesson.ves
~~~

## Name a value

Give the number a name with `let`. The semicolon ends the binding. Leave the final expression without a semicolon so the function returns its value.

~~~vesper
fn main() -> i64 {
    let seats = 7;
    seats
}
~~~

## Choose an integer type

An annotation after a name fixes its type. This counter holds an unsigned 64-bit value. The function returns the same type as the counter.

~~~vesper
fn main() -> u64 {
    let seats: u64 = 7;
    seats
}
~~~

## Change a local

Add `mut` when a binding needs a new value. Run this program to get `8`. Remove `mut` and run `vesper check lesson.ves` to see which assignment the checker rejects.

~~~vesper
fn main() -> u64 {
    let mut seats: u64 = 7;
    seats = 8;
    seats
}
~~~

## Pass an argument

Move the update into a function. Calling `next(7)` binds `n` to `7`. The built-in `wrapping_add` adds modulo the size of the integer type.

~~~vesper
fn next(n: u64) -> u64 {
    wrapping_add(n, 1)
}
fn main() -> u64 {
    next(7)
}
~~~

## Choose a branch

Both arms of this `if` produce a value. Passing `3` selects the upper limit of two seats. Try `1` to take the other arm.

~~~vesper
fn reserve(wanted: u64) -> u64 {
    if wanted > 2 {
        2
    } else {
        wanted
    }
}
fn main() -> u64 {
    reserve(3)
}
~~~

## Name a condition

A comparison produces a `bool`. Extract the comparison into a function so the caller can ask the same question about different requests.

~~~vesper
fn fits(wanted: u64, free: u64) -> bool {
    wanted <= free
}
fn main() -> i64 {
    if fits(3, 2) {
        1
    } else {
        0
    }
}
~~~

## Repeat an update

A `for` loop binds each value of a range in turn. The upper end of `1..4` is excluded, so this program adds one, two, and three and returns `6`.

~~~vesper
fn main() -> u64 {
    let mut total: u64 = 0;
    for n in 1..4 {
        total = wrapping_add(total, n);
    }
    total
}
~~~

## Include the endpoint

The equals sign in `..=` includes the last value. This loop adds one through four and returns `10`. Change the range to `1..4` to stop before four.

~~~vesper
fn main() -> u64 {
    let mut total: u64 = 0;
    for n in 1..=4 {
        total = wrapping_add(total, n);
    }
    total
}
~~~

## Read an exit status

A calculation can return a negative integer. Save this small example as `lesson.ves`. The evaluator prints `-1`. When you build a native executable and run it from a Linux shell, the shell sees the low eight bits of the return value.

~~~vesper
fn main() -> i64 {
    -1
}
~~~

~~~sh
$ vesper run lesson.ves
-1
$ vesper build lesson.ves -o lesson
built lesson
$ ./lesson
$ echo $?
255
~~~

The last command prints `255`. Use zero for success and a small positive failure code when another program needs to interpret your executable’s status.

<!-- figure:exit -->

# Data and patterns

## Group two fields

A `struct` keeps related values together. This record stores a room’s capacity and current use. A field access picks out one value. Running the program prints `2`.

~~~vesper
copy struct Room {
    seats: u64,
    used: u64
}
fn main() -> u64 {
    let room = Room {
        seats: 8,
        used: 2
    };
    room.used
}
~~~

## Copy a record

The `copy` declaration permits another independent value. Change the copy’s field and return the original field. It still holds `2`.

~~~vesper
copy struct Room {
    seats: u64,
    used: u64
}
fn main() -> u64 {
    let room = Room {
        seats: 8,
        used: 2
    };
    let mut other = room;
    other.used = 3;
    room.used
}
~~~

## Read through a loan

Use an `in` parameter when a helper only reads its argument. The caller keeps the room and can read it again after the call.

~~~vesper
copy struct Room {
    seats: u64,
    used: u64
}
fn full(in room: Room) -> bool {
    room.used == room.seats
}
fn main() -> u64 {
    let room = Room {
        seats: 8,
        used: 2
    };
    if full(in room) {
        0
    } else {
        room.seats
    }
}
~~~

## Lend a field for writing

An `inout` parameter gives the callee exclusive access for the call. Here the caller lends just the counter. Reading it after the call gives `3`.

~~~vesper
fn book(inout used: u64) {
    used = wrapping_add(used, 1);
}
fn main() -> u64 {
    let mut used: u64 = 2;
    book(inout used);
    used
}
~~~

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

Both arguments name the same counter and ask for exclusive access. Give the second argument its own mutable counter, then run the program to verify that `reset` sets the first counter to `0`.

~~~vesper E0301
fn reset(inout first: u64, inout second: u64) {
    first = 0;
    second = 0;
}
fn main() -> u64 {
    let mut count: u64 = 3;
    reset(inout count, inout count);
    count
}
~~~

## Name two alternatives

An `enum` describes a choice. A booking is either open or held. Pattern matching lets each variant produce a different result.

~~~vesper
copy enum Booking {
    Open,
    Held
}
fn main() -> i64 {
    match Booking::Held {
        Booking::Open => 0,
        Booking::Held => 1,
    }
}
~~~

## Carry data in a variant

Put the reserved seat number inside `Held`. The pattern `Held(n)` gives that number a name in its arm. This version prints `3`.

~~~vesper
copy enum Booking {
    Open,
    Held(i64)
}
fn main() -> i64 {
    match Booking::Held(3) {
        Booking::Open => 0,
        Booking::Held(n) => n,
    }
}
~~~

## Cover every case

The editor starts with a match that handles a held booking. Add an arm for `Booking::Open` and make it return `0`. The checker considers every value the type can hold. Once the error clears, run the program and change the booking to try each arm.

~~~vesper E0212
copy enum Booking {
    Open,
    Held(i64)
}
fn main() -> i64 {
    let booking = Booking::Held(3);
    match booking {
        Booking::Held(n) => n,
    }
}
~~~

~~~vesper
copy enum Booking {
    Open,
    Held(i64)
}
fn main() -> i64 {
    let booking = Booking::Held(3);
    match booking {
        Booking::Held(n) => n,
        Booking::Open => 0,
    }
}
~~~

## Represent an absent value

Use `Option[T]` when the only alternatives are a value of type `T` and its absence. Change `Some(3)` to `None` to make this program return the fallback.

~~~vesper
fn seat(value: Option[i64]) -> i64 {
    match value {
        Some(n) => n,
        None => 0
    }
}
fn main() -> i64 {
    seat(Some(3))
}
~~~

# Ownership

## Give a ticket one owner

The ownership mode tells you what you owe when a value leaves scope. The parameter mode tells you what one call can do with it. An `in` borrow can read a ticket while you still own it. A `sink` call transfers ownership. The callee can then drop the ticket because it is affine.

<!-- figure:ownership-modes -->

<!-- anchor:use-after-move -->

The editor tries to transfer one ticket twice. Remove the second `release` call. Then try removing both calls. An affine owner can also let a value go out of scope.

~~~vesper
affine struct Ticket {
    seat: u64
}
fn release(sink ticket: Ticket) {}

fn main() -> u64 {
    let ticket = Ticket {
        seat: 3
    };
    release(sink ticket);
    0
}
~~~

~~~vesper E0303
affine struct Ticket {
    seat: u64
}
fn release(sink ticket: Ticket) {}

fn main() -> u64 {
    let ticket = Ticket {
        seat: 3
    };
    release(sink ticket);
    release(sink ticket);
    0
}
~~~

## Require a decision at scope exit

The editor creates a linear ticket and returns with it still live. Destructure `t` with `let Ticket { seat } = t;` and return the seat number. Destructuring takes the record apart and accounts for its ownership.

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

## Consume a ticket

Destructure the ticket to take it apart. Its `u64` seat number can then leave the function on its own. This version prints `3`.

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
    }
    = t;
    seat
}
~~~

## Give consumption a name

Define a method in an `impl` block. The method takes ownership through `sink self`. At the call site, `t.redeem()` records what happened to the ticket.

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
fn main() -> u64 {
    Ticket {
        seat: 3
    }.redeem()
}
~~~

## Transfer the obligation

A function can return a live ticket. Keep the type and method from the previous step, add `issue`, and replace `main`. Now the caller of `issue` must account for the ticket.

~~~vesper
fn issue(seat: u64) -> Ticket {
    Ticket {
        seat: seat
    }
}
fn main() -> u64 {
    let t = issue(8);
    t.redeem()
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

## Inspect before consuming

Add a read-only helper to the ticket program. The loan ends after `number` returns. Calling `redeem` afterward is valid even for a linear ticket.

~~~vesper
fn number(in t: Ticket) -> u64 {
    t.seat
}
fn main() -> u64 {
    let t = Ticket {
        seat: 3
    };
    let seen = number(in t);
    wrapping_add(seen, t.redeem())
}
~~~

## Capture a value

A closure is a function that can use a value from the scope where you make it. In `|value| wrapping_add(value, amount)`, the name between bars is the input. The body uses `amount` from the surrounding function. That use is called a capture.

`offset(10)` returns the closure with `amount` set to ten. `fn @copy (i64) -> i64` is the type of the returned function. It takes one integer, returns an integer, and can be copied because its capture is copyable. `sink amount` gives the closure ownership of the value it needs after `offset` returns. Try calling `add_ten(2)` with a different input.

~~~vesper
fn offset(sink amount: i64) -> fn @copy (i64) -> i64 {
    | value | wrapping_add(value, amount)
}
fn main() -> i64 {
    let add_ten = offset(10);
    add_ten(2)
}
~~~

# Tests

## Write one test

A test function returns a Boolean. Save this as `lesson.ves` and run `vesper test lesson.ves`. The runner checks that requesting three seats with only two available reserves two.

~~~vesper
fn reserve(wanted: u64, free: u64) -> u64 {
    if wanted > free {
        free
    } else {
        wanted
    }
}
test {
    fn test_limit() -> bool {
        reserve(3, 2) == 2
    }
}
~~~

## Turn inputs into rows

Keep `reserve` and replace the test block. The `// @case` comments are table-driven test sugar. The runner calls `test_reserve` once per row and names each result. You only write the assertion once.

~~~vesper
test {
    // @case over(3, 2, 2)
    // @case exact(2, 2, 2)
    fn test_reserve(wanted: u64, free: u64, expected: u64) -> bool {
        reserve(wanted, free) == expected
    }
}
~~~

## Isolate one failing row

The `over` case asks for three seats when only two are free. The function returns `2`. This copy of the test expects `3` so you can see what the failure looks like. Select Run one case, then change the third argument of the `over` row to `2` and try again.

~~~sh
$ vesper test lesson.ves --filter test_reserve::over
lesson.ves:10:5: FAIL test_reserve::over (assertion failed: test returned false) [evaluator]
tests: 0 passed, 1 failed (0 file error(s))
~~~

# Effects and recovery

## Resume the computation

A reservation expires at time `100`. A test that reads the wall clock near a deadline can land on either side depending on when it runs. Give the test control over the reading so the same code reaches the boundary every time.

`Clock::now()` asks for a timestamp. `eff [Clock]` declares that request in the function signature. The handler's `now` arm supplies `100` with `resume(100)`. The comparison continues with that value and the test passes. Change it to `99` to see the test fail.

~~~vesper
linear effect Clock {
    fn now() -> i64;
}

fn expired(deadline: i64) -> bool eff [Clock] {
    Clock::now() >= deadline
}

test {
    fn test_expired() -> bool {
        handle Clock in {
            expired(100)
        } with {
            return answer => answer,
            now(resume) => resume(100),
        }
    }
}
~~~

<!-- figure:clock-test -->

## Use the resumed result

The reservation starts valid and becomes expired on the next reading. The handler keeps the test timeline in `tick`. Each call returns the current tick and carries the next tick into the next call. Starting at `99` supplies `99` and then `100`. Try starting at `98` to reproduce a test failure without waiting for a real clock.

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

## Declare checked addition

The signature says what callers can expect. `-> i64` gives the type on success. `eff [Fail[ArithOverflow]]` says addition can fail with overflow. A caller of `next` must state that it can pass the effect to its own caller or put a handler around the call.

~~~vesper
fn next(n: i64) -> i64 eff [Fail[ArithOverflow]] {
    n + 1
}
~~~

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

## Handle an overflow

Keep `next` and add this `main`. The `return` clause receives an ordinary result. If the addition overflows, `raise` supplies the fallback. Running this version prints `42`.

~~~vesper
fn main() -> i64 {
    handle Fail[ArithOverflow] in {
        next(41)
    } with {
        return value => value,
        raise(error, _) => 0,
    }
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

## Recover from division by zero

Division by zero raises `Fail[RuntimeError]`. This program catches the failure and returns zero. Change the divisor to four to take the ordinary return path and get `21`.

~~~vesper
fn divide(d: i64) -> i64 eff [Fail[RuntimeError]] {
    84 / d
}
fn main() -> i64 {
    handle Fail[RuntimeError] in {
        divide(0)
    } with {
        return value => value,
        raise(error, _) => 0,
    }
}
~~~

# Syntax sugar

## Bind a piece of syntax

Syntax sugar gives an existing operation a shorter form. The test runner, for example, turns each `@case` row into a call to the same test function. A Forge expression macro can fill a template with code from its call site. Here `$n: expr` accepts an expression, and `quote` describes the expression to insert when you write `next_seat!(41)`.

Forge accepts a limited set of templates. Replace `$n + 1` inside `quote` with `wrapping_add($n, 1)`. When the diagnostic clears, run the macro to see `42`.

~~~vesper
macro next_seat($n: expr) -> expr {
    quote {
        wrapping_add($n, 1)
    }
}
fn main() -> i64 {
    next_seat!(41)
}
~~~

~~~vesper FORGE_PARSE
macro next_seat($n: expr) -> expr {
    quote { $n + 1 }
}
fn main() -> i64 { next_seat!(41) }
~~~

## Format a user-defined spelling

Format the macro definition and its call. Put the definition on one line or change the spacing around `quote`. Select Format to see how Vesper lays out the source. The formatter keeps the call written as `next_seat!(41)`.

~~~sh
$ vesper fmt seats.ves
$ vesper fmt --check seats.ves
~~~

## Read a proposed sugar declaration

Forge expression macros use the `name!(...)` call shape. The manuscript’s proposed `sugar` declarations would let you define a form such as `ensure condition else error;`. This sample is part of that design. It captures the rest of the enclosing block so the rule can decide whether execution continues.

~~~vesper
pub sugar ensure : stmt emits eff [Fail] {
    rule (ensure $cond:expr else $err:expr ; $..rest:stmts) => {
        if $cond { $..rest } else { Fail::raise($err) }
    }
}
~~~

Read the two paths separately. A true condition continues with the remaining statements. A false condition raises the supplied error. The declaration exposes that possibility with `emits eff [Fail]`.

## Define your own sugar

Write a macro called `pair_seats` in `pairs.ves`. Give it one expression parameter and make it double that value with `wrapping_mul`. Call it with `6`. Predict the result before checking the solution below.

~~~vesper
macro pair_seats($count: expr) -> expr {
    quote {
        wrapping_mul($count, 2)
    }
}
fn main() -> i64 {
    pair_seats!(6)
}
~~~

~~~sh
vesper fmt pairs.ves
vesper fmt --check pairs.ves
vesper forge expand pairs.ves > expanded.ves
vesper run expanded.ves
~~~

You should see `12`. Change the argument to `wrapping_add(6, 1)` and run the expansion again. If your rule accepts an expression in that slot, the new result is `14`.
