//! Real standalone structured quotation component; every emitted value is an
//! untrusted claim. Host approval and caller identities never live here.
wit_bindgen::generate!({path:"../../../wit/forge-quote",world:"sandboxed-quotation"});
use exports::vesper::forge_quote::quotation::Guest;
use vesper::forge_quote::types::*;
struct Fixture;
#[derive(Default)]
struct Pack {
    items: Vec<PackedItem>,
    diagnostics: Vec<PackedDiagnostic>,
    text: Vec<u8>,
}
impl Pack {
    fn text(&mut self, s: &str) -> Slice {
        let x = Slice {
            offset: self.text.len() as u32,
            len: s.len() as u32,
        };
        self.text.extend_from_slice(s.as_bytes());
        x
    }
    fn emit(&mut self, kind: TokenKind, s: &str) {
        let text = self.text(s);
        self.items.push(PackedItem::Generated(PackedGenerated {
            kind,
            text,
            reported_span: None,
            claimed_def: None,
        }));
    }
    fn slot(&mut self, n: u32) {
        self.items.push(PackedItem::InputSlot(n));
    }
    fn diagnostic(&mut self, severity: Severity, s: &str) {
        let message = self.text(s);
        self.diagnostics.push(PackedDiagnostic {
            severity,
            message,
            reported_span: None,
        });
    }
    fn finish(self) -> PackedResult {
        PackedResult {
            items: self.items,
            diagnostics: self.diagnostics,
            text: self.text,
        }
    }
}
impl Guest for Fixture {
    fn protocol_version() -> u32 {
        1
    }
    fn run(inv: Invocation) -> Result<PackedResult, PackedResult> {
        let mut p = Pack::default();
        if inv.protocol_version != 1 {
            p.diagnostic(Severity::Error, "bad protocol");
            return Err(p.finish());
        }
        match inv.entry.as_str() {
            "identity" => {
                p.slot(0);
                p.diagnostic(Severity::Note, "actual structured guest");
            }
            "generated" => p.emit(TokenKind::IntLiteral, "1"),
            "duplicate" => {
                p.emit(TokenKind::Open(Delimiter::Brace), "{");
                p.slot(0);
                p.emit(TokenKind::Punct, ";");
                p.slot(0);
                p.emit(TokenKind::Close(Delimiter::Brace), "}");
            }
            "capture" => {
                p.emit(TokenKind::Open(Delimiter::Brace), "{");
                p.emit(TokenKind::Ident, "let");
                p.emit(TokenKind::Ident, "x");
                p.emit(TokenKind::Punct, "=");
                p.emit(TokenKind::IntLiteral, "0");
                p.emit(TokenKind::Punct, ";");
                p.slot(0);
                p.emit(TokenKind::Close(Delimiter::Brace), "}");
            }
            "drift" => {
                p.slot(0);
                p.emit(TokenKind::Open(Delimiter::Paren), "(");
                p.emit(TokenKind::IntLiteral, "2");
                p.emit(TokenKind::Close(Delimiter::Paren), ")");
            }
            "forged-slot" => p.slot(u32::MAX),
            "bad-kind" => p.emit(TokenKind::Ident, "not one token"),
            "bad-utf8" => {
                p.text.push(0xff);
                p.items.push(PackedItem::Generated(PackedGenerated {
                    kind: TokenKind::Ident,
                    text: Slice { offset: 0, len: 1 },
                    reported_span: None,
                    claimed_def: None,
                }));
            }
            "bad-slice" => {
                p.items.push(PackedItem::Generated(PackedGenerated {
                    kind: TokenKind::Ident,
                    text: Slice {
                        offset: u32::MAX,
                        len: u32::MAX,
                    },
                    reported_span: None,
                    claimed_def: None,
                }));
            }
            "alias-bomb" => {
                p.text = vec![b'a'; 65536];
                for _ in 0..32 {
                    p.items.push(PackedItem::Generated(PackedGenerated {
                        kind: TokenKind::Ident,
                        text: Slice {
                            offset: 0,
                            len: 65536,
                        },
                        reported_span: None,
                        claimed_def: None,
                    }));
                }
            }
            "slot-bomb" => {
                for _ in 0..5000 {
                    p.slot(0)
                }
            }
            "call-connect" => {
                p.emit(TokenKind::Ident, "connect");
                p.emit(TokenKind::Open(Delimiter::Paren), "(");
                p.emit(TokenKind::Close(Delimiter::Paren), ")");
            }
            "read-input" => {
                match inv
                    .args
                    .first()
                    .and_then(|p| std::fs::read_to_string(p).ok())
                {
                    Some(text) => p.emit(TokenKind::IntLiteral, text.trim()),
                    None => {
                        p.diagnostic(Severity::Error, "cannot read declared input");
                        return Err(p.finish());
                    }
                }
            }
            "claim" => {
                p.emit(TokenKind::Ident, "connect");
                if let Some(PackedItem::Generated(t)) = p.items.last_mut() {
                    t.claimed_def = Some(DefId {
                        package: 999,
                        item: 999,
                    });
                }
            }
            "failure-items" => {
                p.slot(0);
                p.diagnostic(Severity::Error, "bad");
                return Err(p.finish());
            }
            "error-diagnostic" => {
                p.emit(TokenKind::IntLiteral, "1");
                p.diagnostic(Severity::Error, "reported error");
            }
            "fail" => {
                p.diagnostic(Severity::Error, "guest failed");
                return Err(p.finish());
            }
            "spin" => loop {
                std::hint::black_box(1u64.wrapping_mul(37));
            },
            "memory" => {
                let data = vec![1u8; 256 << 20];
                std::hint::black_box(data);
                p.emit(TokenKind::IntLiteral, "1");
            }
            "stdio" => {
                print!("{}", "x".repeat(1 << 20));
                p.emit(TokenKind::IntLiteral, "1");
            }
            "random" => p.emit(
                TokenKind::IntLiteral,
                &wasip2::random::random::get_random_u64().to_string(),
            ),
            "clock" => p.emit(
                TokenKind::IntLiteral,
                &wasip2::clocks::wall_clock::now().seconds.to_string(),
            ),
            "authority-denied" => {
                let okay =
                    std::env::vars().next().is_none() && std::fs::read("/etc/passwd").is_err();
                p.emit(TokenKind::IntLiteral, if okay { "1" } else { "0" });
            }
            "write-out" => {
                let _ = std::fs::write("/out/stray.txt", "not quotation output");
                p.emit(TokenKind::IntLiteral, "1");
            }
            _ => {
                p.diagnostic(Severity::Error, "unknown fixture entry");
                return Err(p.finish());
            }
        }
        Ok(p.finish())
    }
}
export!(Fixture);
