//! Salsa-backed revisioned source inputs and memoized frontend queries.
use crate::{
    ast::Diagnostic,
    check::{self, Checked},
    parser::{self, Parse},
};
#[salsa::input]
pub struct SourceFile {
    #[returns(ref)]
    pub text: String,
}
#[salsa::db]
#[derive(Clone, Default)]
pub struct Database {
    storage: salsa::Storage<Self>,
}
#[salsa::db]
impl salsa::Database for Database {}
#[salsa::tracked(returns(ref), no_eq)]
pub fn parse_file(db: &dyn salsa::Database, file: SourceFile) -> Parse {
    parser::parse(file.text(db))
}
#[salsa::tracked(returns(ref), no_eq)]
pub fn check_file(db: &dyn salsa::Database, file: SourceFile) -> Checked {
    check::check(&parse_file(db, file).module)
}
#[salsa::tracked(returns(ref))]
pub fn diagnostics(db: &dyn salsa::Database, file: SourceFile) -> Vec<Diagnostic> {
    let p = parse_file(db, file);
    if p.diagnostics.is_empty() {
        check_file(db, file).diagnostics.clone()
    } else {
        p.diagnostics.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use salsa::Setter;
    #[test]
    fn memoization_and_invalidation() {
        let mut db = Database::default();
        let file = SourceFile::new(&db, "fn main() {}".into());
        let first = parse_file(&db, file) as *const Parse;
        assert_eq!(first, parse_file(&db, file) as *const Parse);
        assert!(diagnostics(&db, file).is_empty());
        file.set_text(&mut db).to("fn main() { unknown }".into());
        assert!(diagnostics(&db, file).iter().any(|d| d.code == "E0201"));
    }
}
