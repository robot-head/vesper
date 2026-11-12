//! Shared syntax/HIR value types and integer semantics.
//!
//! This dependency-free compiler boundary (apart from serialization) contains
//! no checker, lowering, backend, or runtime. The checked World and CFG analyses
//! remain in the vesper facade until their mutual dependencies are separated.
pub mod ast;
pub mod numeric;
