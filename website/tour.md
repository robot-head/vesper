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
fn next(n: u64) -> u64 { wrapping_add(n, 1) }
fn main() -> u64 { next(7) }
~~~

## Choose a branch

Both arms of this `if` produce a value. Passing `3` selects the upper limit of two seats. Try `1` to take the other arm.

~~~vesper
fn reserve(wanted: u64) -> u64 {
    if wanted > 2 { 2 } else { wanted }
}
fn main() -> u64 { reserve(3) }
~~~

## Name a condition

A comparison produces a `bool`. Extract the comparison into a function so the caller can ask the same question about different requests.

~~~vesper
fn fits(wanted: u64, free: u64) -> bool {
    wanted <= free
}
fn main() -> i64 { if fits(3, 2) { 1 } else { 0 } }
~~~

## Repeat an update

A `for` loop binds each value of a range in turn. The upper end of `1..4` is excluded, so this program adds one, two, and three and returns `6`.

~~~vesper
fn main() -> u64 {
    let mut total: u64 = 0;
    for n in 1..4 { total = wrapping_add(total, n); }
    total
}
~~~

## Include the endpoint

The equals sign in `..=` includes the last value. Change only the range in the previous program. The final iteration now adds four, for a total of `10`.

~~~vesper
for n in 1..=4 { total = wrapping_add(total, n); }
~~~

## Read an exit status

A calculation can return a negative integer. Save this small example as `lesson.ves`. The evaluator prints `-1`. When you build a native executable and run it from a Linux shell, the shell sees the low eight bits of the return value.

~~~vesper
fn main() -> i64 { -1 }
~~~

~~~sh
vesper run lesson.ves
vesper build lesson.ves -o lesson
./lesson
echo $?
~~~

The last command prints `255`. Use zero for success and a small positive failure code when another program needs to interpret your executable’s status.

# Data and patterns

## Group two fields

A `struct` keeps related values together. This record stores a room’s capacity and current use. A field access picks out one value. Running the program prints `2`.

~~~vesper
copy struct Room { seats: u64, used: u64 }
fn main() -> u64 {
    let room = Room { seats: 8, used: 2 };
    room.used
}
~~~

## Copy a record

The `copy` declaration permits another independent value. Change the copy’s field and return the original field. It still holds `2`.

~~~vesper
copy struct Room { seats: u64, used: u64 }
fn main() -> u64 {
    let room = Room { seats: 8, used: 2 };
    let mut other = room;
    other.used = 3;
    room.used
}
~~~

## Read through a loan

Use an `in` parameter when a helper only reads its argument. The caller keeps the room and can read it again after the call.

~~~vesper
copy struct Room { seats: u64, used: u64 }
fn full(in room: Room) -> bool { room.used == room.seats }
fn main() -> u64 {
    let room = Room { seats: 8, used: 2 };
    if full(in room) { 0 } else { room.seats }
}
~~~

## Lend a field for writing

An `inout` parameter gives the callee exclusive access for the call. Here the caller lends just the counter. Reading it after the call gives `3`.

~~~vesper
fn book(inout used: u64) { used = wrapping_add(used, 1); }
fn main() -> u64 {
    let mut used: u64 = 2;
    book(inout used);
    used
}
~~~

## Limit a helper to one field

A field projection in the signature names the part of the record a helper may change. The caller can lend `seats` for reading during the same call because the writable field is `used`.

~~~vesper
copy struct Room { seats: u64, used: u64 }
fn fill(inout room.{used}: Room, in limit: u64) {
    room.used = limit;
}
fn main() -> u64 {
    let mut room = Room { seats: 8, used: 2 };
    fill(inout room, in room.seats);
    room.used
}
~~~

## Find a conflicting loan

What happens if two writable arguments name the same counter? Check this program. Both parameters would have exclusive access to `count` during one call. To fix the overlap, introduce a second mutable counter and lend it as the second argument.

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
copy enum Booking { Open, Held }
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
copy enum Booking { Open, Held(i64) }
fn main() -> i64 {
    match Booking::Held(3) {
        Booking::Open => 0,
        Booking::Held(n) => n,
    }
}
~~~

## Cover every case

In the previous program, remove the `Open` arm and check the file. The checker asks you to handle the missing variant. A wildcard handles every remaining case. Put it last.

~~~vesper
match Booking::Held(3) {
    Booking::Held(n) => n,
    _ => 0,
}
~~~

## Represent an absent value

Use `Option[T]` when the only alternatives are a value of type `T` and its absence. Change `Some(3)` to `None` to make this program return the fallback.

~~~vesper
fn seat(value: Option[i64]) -> i64 {
    match value { Some(n) => n, None => 0 }
}
fn main() -> i64 { seat(Some(3)) }
~~~

# Ownership

## Give a ticket one owner

An `affine` ticket can move to another owner or go out of scope. Here the caller transfers its ticket to `release`. No code needs the ticket after that call.

~~~vesper
affine struct Ticket { seat: u64 }
fn release(sink ticket: Ticket) {}
fn main() -> u64 {
    let t = Ticket { seat: 3 };
    release(sink t);
    0
}
~~~

## Detect a second use

The second call below tries to move a ticket the caller has already given away. Check the file and identify which call must be removed.

~~~vesper E0303
affine struct Ticket { seat: u64 }
fn release(sink ticket: Ticket) {}
fn main() -> u64 {
    let t = Ticket { seat: 3 };
    release(sink t);
    release(sink t);
    0
}
~~~

## Require a decision at scope exit

Change the declaration to `linear` when forgetting the ticket is a bug. This complete program has an error. Run `vesper check lesson.ves` to see `E0302` at the unfinished obligation.

~~~vesper E0302
linear struct Ticket { seat: u64 }
fn main() -> u64 {
    let t = Ticket { seat: 3 };
    0
}
~~~

## Consume a ticket

Destructure the ticket to take it apart. Its `u64` seat number can then leave the function on its own. This version prints `3`.

~~~vesper
linear struct Ticket { seat: u64 }
fn main() -> u64 {
    let t = Ticket { seat: 3 };
    let Ticket { seat } = t;
    seat
}
~~~

## Give consumption a name

Define a method in an `impl` block. The method takes ownership through `sink self`. At the call site, `t.redeem()` records what happened to the ticket.

~~~vesper
linear struct Ticket { seat: u64 }
impl Ticket {
    fn redeem(sink self) -> u64 {
        let Ticket { seat } = self;
        seat
    }
}
fn main() -> u64 { Ticket { seat: 3 }.redeem() }
~~~

## Transfer the obligation

A function can return a live ticket. Keep the type and method from the previous step, add `issue`, and replace `main`. Now the caller of `issue` must account for the ticket.

~~~vesper
fn issue(seat: u64) -> Ticket { Ticket { seat: seat } }
fn main() -> u64 {
    let t = issue(8);
    t.redeem()
}
~~~

## Finish on both paths

Add this function beside `redeem`. The true arm returns the seat number. The false arm also consumes the ticket, then returns zero. Replacing the whole false arm with `0` leaves the ticket live on that path.

~~~vesper
fn finish(sink t: Ticket, attended: bool) -> u64 {
    if attended { t.redeem() } else {
        let Ticket { seat } = t;
        0
    }
}
~~~

## Inspect before consuming

Add a read-only helper to the ticket program. The loan ends after `number` returns. Calling `redeem` afterward is valid even for a linear ticket.

~~~vesper
fn number(in t: Ticket) -> u64 { t.seat }
fn main() -> u64 {
    let t = Ticket { seat: 3 };
    let seen = number(in t);
    wrapping_add(seen, t.redeem())
}
~~~

# Effects and recovery

## Choose wrapping arithmetic

At the end of the integer range, wrapping addition starts again at the beginning. This `u64` expression adds one to the largest representable value and returns zero.

~~~vesper
fn main() -> u64 { wrapping_add(18446744073709551615, 1) }
~~~

## Declare checked addition

Use `+` when overflow should report a failure. The effect row after the return type names the possible failure. Save this function and check it with `vesper check lesson.ves`.

~~~vesper
fn next(n: i64) -> i64 eff [Fail[ArithOverflow]] {
    n + 1
}
~~~

## Pass an effect to the caller

A caller can declare the same effect. Add this function beside `next`. Removing the effect row from `advance` leaves an unhandled failure in its body.

~~~vesper
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

Replace `next(41)` inside the handler with the call below. Its input is the greatest signed 64-bit integer. Adding one triggers `raise`, so the same handler now returns zero.

~~~vesper
next(9223372036854775807)
~~~

## Recover from division by zero

Division by zero raises `Fail[RuntimeError]`. This program catches the failure and returns zero. Change the divisor to four to take the ordinary return path and get `21`.

~~~vesper
fn divide(d: i64) -> i64 eff [Fail[RuntimeError]] { 84 / d }
fn main() -> i64 {
    handle Fail[RuntimeError] in { divide(0) } with {
        return value => value,
        raise(error, _) => 0,
    }
}
~~~

## Inspect the error

Replace the `raise` arm from the previous step with this one. Matching on the error lets a caller give division by zero its own recovery value. Run the program again with zero as the divisor.

~~~vesper
raise(error, _) => match error {
    RuntimeError::DivisionByZero => 42,
    RuntimeError::IndexOutOfBounds => 0,
},
~~~

## Declare an operation

A user-defined effect gives a computation a way to ask its caller for a value. This declaration names the effect `Ask` and gives its operation an integer result.

~~~vesper
linear effect Ask { fn ask() -> i64; }
~~~

## Request a value

Keep the `Ask` declaration. This function’s effect row tells its caller that computing the answer requires a handler for `Ask`. The wrapping addition happens after `ask` supplies a value.

~~~vesper
fn compute() -> i64 eff [Ask] {
    wrapping_add(Ask::ask(), 1)
}
~~~

## Resume the computation

Add this `main` to the previous two declarations. At the request, `resume(20)` supplies the missing value. Execution continues inside `compute`, adds one, then passes `21` through the return arm.

~~~vesper
fn main() -> i64 {
    handle Ask in { compute() } with {
        return value => value,
        ask(resume) => resume(20),
    }
}
~~~

## Use the resumed result

Replace the `ask` arm. Here `resume(20)` returns `21` to the handler. The multiplication happens after that return, giving `42`. The continuation can be resumed only once.

~~~vesper
ask(resume) => {
    let answer = resume(20);
    wrapping_mul(answer, 2)
},
~~~

## Capture a value

A closure can remember a value from its enclosing scope. Calling `offset(10)` returns a function that adds ten. The caller supplies two and gets twelve.

~~~vesper
fn offset(sink amount: i64) -> fn @copy (i64) -> i64 {
    |value| wrapping_add(value, amount)
}
fn main() -> i64 {
    let add_ten = offset(10);
    add_ten(2)
}
~~~

# Tests and syntax sugar

## Write one test

A test function returns a Boolean. Save this as `lesson.ves` and run `vesper test lesson.ves`. The runner checks that requesting three seats with only two available reserves two.

~~~vesper
fn reserve(wanted: u64, free: u64) -> u64 {
    if wanted > free { free } else { wanted }
}
test {
    fn test_limit() -> bool { reserve(3, 2) == 2 }
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

Change the expected value in the `over` row from two to three. Run just that row with the command below. The case name identifies the failure. Restore the expected value and add a row for an empty room.

~~~sh
vesper test lesson.ves --filter test_reserve::over
~~~

## Read a macro call

Suppose your code often needs the seat number after the current one. You can give `wrapping_add(41, 1)` a shorter spelling. The exclamation mark tells Forge to expand a source macro. The next step supplies its definition.

~~~vesper
next_seat!(41)
~~~

## Bind a piece of syntax

Save this definition and the accompanying `main` as `seats.ves`. The parameter `$n: expr` accepts an expression. Inside `quote`, `$n` marks the spot where Forge inserts the argument.

~~~vesper
macro next_seat($n: expr) -> expr {
    quote { wrapping_add($n, 1) }
}
fn main() -> i64 { next_seat!(41) }
~~~

## Inspect the expansion

Run these commands from a POSIX shell. Forge checks the expansion and writes it as ordinary Vesper source. Look at `expanded.ves` to find the call to `wrapping_add`, then run it to get `42`.

~~~sh
vesper forge expand seats.ves > expanded.ves
vesper run expanded.ves
~~~

## Format a user-defined spelling

Change the spacing around `quote` or put the entire macro definition on one line. Format the file. The formatter understands the definition and keeps the call spelled `next_seat!(41)`.

~~~sh
vesper fmt seats.ves
vesper fmt --check seats.ves
~~~

The second command succeeds without changing the file. You can use it in CI to check that a committed file already has the formatter’s output.

## Check a quoted expression

Try writing an infix addition directly inside the quotation. Forge accepts a limited set of expression templates. It rejects this spelling at the plus sign while parsing the macro. Check it with `vesper forge check seats.ves`, then replace the quoted expression with `wrapping_add($n, 1)` and check again.

~~~vesper FORGE_PARSE
macro next_seat($n: expr) -> expr {
    quote { $n + 1 }
}
fn main() -> i64 { next_seat!(41) }
~~~

## Read a proposed sugar declaration

The manuscript’s proposed `sugar` form can also describe statement-shaped syntax. For example, this rule gives a guard a condition, a failure value, and the rest of the enclosing block. The current runnable examples use Forge expression macros.

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
    quote { wrapping_mul($count, 2) }
}
fn main() -> i64 { pair_seats!(6) }
~~~

~~~sh
vesper fmt pairs.ves
vesper fmt --check pairs.ves
vesper forge expand pairs.ves > expanded.ves
vesper run expanded.ves
~~~

You should see `12`. Change the argument to `wrapping_add(6, 1)` and run the expansion again. If your rule accepts an expression in that slot, the new result is `14`.
