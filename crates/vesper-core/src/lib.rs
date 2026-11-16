//! Shared syntax/HIR value types and integer semantics.
//!
//! This dependency-free compiler boundary (apart from serialization) contains
//! no checker, lowering, backend, or runtime. The checked World and CFG analyses
//! remain in the vesper facade until their mutual dependencies are separated.
pub mod ast;
pub mod clock_quotient;
pub mod field_tree;
pub mod ghost;
pub mod guarded_graph;
pub mod lifo;
pub mod modal_kind;
pub mod mode_cross;
pub mod mode_vec;
pub mod numeric;
pub mod out_slice;
pub mod out_tree;
pub mod pcg;
pub mod resource_grade;
pub mod slice_projections;
