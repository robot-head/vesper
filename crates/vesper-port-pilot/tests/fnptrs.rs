//! The callback-handle library (#1502) end to end: `vesper/fnptrs.ves`
//! linked through the committed `port/fnptrs.rs`. A copy struct holding C
//! function pointers the library looked up crosses as a result, a lent
//! mirror and a by-value argument; a lone pointer crosses on its own; and
//! every pointer Rust hands back still calls the function it named.
//! Vesper build only.
#![cfg(feature = "vesper-port")]

use vesper_port_pilot::fnptrs::{self, Table, repr};

fn table() -> Table {
    fnptrs::open(3).expect("the shim knows add and mul")
}

#[test]
fn a_table_of_pointers_comes_back_and_is_called_through() {
    let t = table();
    assert_ne!(t.add, t.mul);
    assert_eq!(t.scale, 3);
    let lent = repr::Table::from_value(&t);
    assert_eq!(fnptrs::combine(&lent, 4, 5), 27);
    assert_eq!(lent.to_value(), t);
}

#[test]
fn single_pointers_cross_both_ways() {
    let t = table();
    let lent = repr::Table::from_value(&t);
    let mul = fnptrs::pick(&lent, true);
    let add = fnptrs::pick(&lent, false);
    assert_eq!(mul, t.mul);
    assert_eq!(add, t.add);
    assert_eq!(fnptrs::call(mul, 6, 7), 42);
    assert_eq!(fnptrs::call(add, 6, 7), 13);
    assert_eq!(fnptrs::missing(), None);
}

#[test]
fn an_inout_table_is_rewritten_in_place() {
    let t = table();
    let mut lent = repr::Table::from_value(&t);
    fnptrs::swap(&mut lent);
    assert_eq!(lent.add, t.mul);
    assert_eq!(lent.mul, t.add);
    // (4 * 5) + 3 through the swapped pointers.
    assert_eq!(fnptrs::combine(&lent, 4, 5), 23);
}
