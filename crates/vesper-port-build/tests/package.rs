//! The package's exports, through Cargo (build.rs) and Bazel
//! (`vesper_port_object(package = ...)`), with the dependency's root module
//! and submodule and a second module of the root package all reached.
use vesper_port_build::{ArithOverflow, rect_perimeter, square_plus_triple};

#[test]
fn dependency_modules_are_linked() {
    // square (math root module) + triple (math::scale).
    assert_eq!(square_plus_triple(5), Ok(25 + 15));
    assert_eq!(square_plus_triple(-2), Ok(4 - 6));
}

#[test]
fn root_package_modules_are_linked() {
    // perimeter (app::shapes).
    assert_eq!(rect_perimeter(3, 4), Ok(14));
}

#[test]
fn failures_cross_the_package_boundary() {
    assert_eq!(square_plus_triple(i64::MAX), Err(ArithOverflow));
}
