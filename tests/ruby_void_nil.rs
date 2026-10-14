//! A `nil` statement that is not its sequence's value is not written.
//!
//! Lowerings leave a bare `nil` where they drop or replace a statement:
//! the inert `config.cache_classes = false` the environment program
//! consumed, a `class` hoisted out of a test block, survey mode's
//! placeholder for a construct ingest could not take. Written before
//! another statement, `ruby -w` reports "possibly useless use of nil in
//! void context", and Core's strict_warnings.rb raises
//! `StrictWarning::Offense` on that warning when the file loads (Core's
//! config/environments/test.rb: `if ENV["CLASS_RELOADING"]` /
//! `config.cache_classes = false` / `FrozenRecord::Base.auto_reloading =
//! true`). The sequence's own value, a tail `nil`, is kept, and so is a
//! refused statement that carries its diagnostic: it renders as its
//! `raise`.

use std::process::Command;

use roundhouse::diagnostic::DiagnosticKind;
use roundhouse::expr::{Expr, ExprNode, LValue, Literal};
use roundhouse::ident::Symbol;
use roundhouse::ingest::ingest_library_classes;
use roundhouse::span::Span;

fn node(n: ExprNode) -> Expr {
    Expr::new(Span::synthetic(), n)
}

fn nil() -> Expr {
    node(ExprNode::Lit { value: Literal::Nil })
}

fn call(name: &str) -> Expr {
    node(ExprNode::Send {
        recv: None,
        method: Symbol::from(name),
        args: vec![],
        block: None,
        parenthesized: false,
    })
}

fn seq(exprs: Vec<Expr>) -> Expr {
    node(ExprNode::Seq { exprs })
}

fn emit(e: &Expr) -> String {
    roundhouse::emit::ruby::emit_expr(e)
}

/// `ruby -w -c` on `source`: its warnings, which must not include a
/// void-context one.
fn assert_no_void_warning(source: &str, what: &str) {
    let dir = std::env::temp_dir().join(format!("ruby-void-nil-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{what}.rb"));
    std::fs::write(&path, source).unwrap();
    let out = Command::new("ruby").arg("-w").arg("-c").arg(&path).output().expect("ruby is on PATH");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{what} does not parse:\n{stderr}\n{source}");
    assert!(!stderr.contains("void context"), "{what} warns:\n{stderr}\n{source}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_leading_nil_statement_is_not_written() {
    assert_eq!(emit(&seq(vec![nil(), call("foo")])), "foo");
    assert_eq!(emit(&seq(vec![call("foo"), nil(), nil(), call("bar")])), "foo\nbar");
}

#[test]
fn the_tail_nil_is_the_sequence_value_and_stays() {
    assert_eq!(emit(&seq(vec![call("foo"), nil()])), "foo\nnil");
    assert_eq!(emit(&seq(vec![nil()])), "nil");
    assert_eq!(emit(&seq(vec![nil(), nil()])), "nil");
    // At a value site the sequence is grouped and still answers nil.
    let assign = node(ExprNode::Assign {
        target: LValue::Var { id: roundhouse::ident::VarId(0), name: Symbol::from("x") },
        value: seq(vec![nil(), call("foo"), nil()]),
    });
    assert_eq!(emit(&assign), "x = (foo\nnil)");
}

#[test]
fn a_nested_statement_sequence_drops_its_void_tail() {
    // A Seq that is a statement of another renders flat into its list,
    // so its own last statement is in void context too.
    let inner = seq(vec![call("a"), nil()]);
    assert_eq!(emit(&seq(vec![inner, call("b")])), "a\nb");
    let only_nils = seq(vec![nil(), nil()]);
    assert_eq!(emit(&seq(vec![call("a"), only_nils, call("b")])), "a\nb");
    // The enclosing sequence's value is the nested one's value.
    let tail = seq(vec![call("a"), nil()]);
    assert_eq!(emit(&seq(vec![call("b"), tail])), "b\na\nnil");
}

#[test]
fn effect_free_reads_that_are_not_the_value_are_not_written() {
    let var = |name: &str| node(ExprNode::Var { id: roundhouse::ident::VarId(0), name: Symbol::from(name) });
    // `h = record.attributes; h.symbolize_keys!; h`, with the grounded
    // `symbolize_keys!` become its receiver.
    let assign = node(ExprNode::Assign {
        target: LValue::Var { id: roundhouse::ident::VarId(0), name: Symbol::from("h") },
        value: call("attributes"),
    });
    assert_eq!(emit(&seq(vec![assign, var("h"), var("h")])), "h = attributes\nh");
    let ivar = node(ExprNode::Ivar { name: Symbol::from("x") });
    let int = node(ExprNode::Lit { value: Literal::Int { value: 1 } });
    let sym = node(ExprNode::Lit { value: Literal::Sym { value: Symbol::from("k") } });
    let selfref = node(ExprNode::SelfRef);
    assert_eq!(emit(&seq(vec![ivar, int, sym, selfref, call("foo"), var("rec")])), "foo\nrec");
    // A call may have effects and stays wherever it sits.
    assert_eq!(emit(&seq(vec![call("foo"), call("bar")])), "foo\nbar");
}

#[test]
fn a_dropped_statement_keeps_the_blank_line_it_sat_after() {
    let mut dropped = nil();
    dropped.leading_blank_line = true;
    assert_eq!(emit(&seq(vec![call("a"), dropped, call("b")])), "a\n\nb");
}

#[test]
fn a_refused_statement_keeps_its_raise() {
    let mut refused = nil();
    refused.diagnostic = Some(DiagnosticKind::IngestRefused { detail: "shell command (backticks) is not modeled".into() });
    let text = emit(&seq(vec![refused, call("b")]));
    assert!(text.starts_with("(::Kernel.raise(::NotImplementedError, 'roundhouse: "), "{text}");
    assert!(text.contains("shell command (backticks) is not modeled"), "{text}");
    assert!(text.ends_with("\nb"), "{text}");
}

#[test]
fn an_environment_style_branch_with_a_dropped_write_is_warning_free() {
    // config/environments/test.rb:28 after the inert `config.cache_classes
    // = false` is consumed.
    let env = node(ExprNode::Send {
        recv: Some(node(ExprNode::Const { path: vec![Symbol::from("ENV")] })),
        method: Symbol::from("[]"),
        args: vec![node(ExprNode::Lit { value: Literal::Str { value: "CLASS_RELOADING".into() } })],
        block: None,
        parenthesized: false,
    });
    let write = node(ExprNode::Assign {
        target: LValue::Attr {
            recv: node(ExprNode::Const { path: vec![Symbol::from("FrozenRecord"), Symbol::from("Base")] }),
            name: Symbol::from("auto_reloading"),
        },
        value: node(ExprNode::Lit { value: Literal::Bool { value: true } }),
    });
    let branch = node(ExprNode::If { cond: env, then_branch: seq(vec![nil(), write]), else_branch: seq(vec![]) });
    let text = emit(&seq(vec![branch, call("after")]));
    assert_eq!(text, "if ENV[\"CLASS_RELOADING\"]\n  FrozenRecord::Base.auto_reloading = true\nend\nafter");
    assert_no_void_warning(&format!("def boot\n{text}\nend\n"), "environment_branch");
}

/// Survey mode's placeholder for a construct ingest could not take — here
/// bind) — before another statement: in a method
/// head, a rescue body and a lambda body.
#[test]
/// head, a rescue body and a lambda body.
fn survey_placeholders_before_a_statement_are_not_written() {
    let source = r#"class Probe
  def head(pair)
    (a, *b), c = pair
    puts c
  end

  def rescued(grouped)
    work(grouped)
  rescue StandardError => e
    (owner_type, *owner_id), = grouped
    report(e)
  end

  def deferred(pair)
    -> {
      (x, *y), z = pair
      build(z)
    }
  end
end
"#;
    roundhouse::ingest::survey::activate();
    let classes = ingest_library_classes(source.as_bytes(), "probe.rb").expect("ingest");
    let gaps = roundhouse::ingest::survey::drain();
    // Each gap is still recorded for the punch list.
    assert_eq!(gaps.len(), 3, "{gaps:?}");
    let probe = classes.iter().find(|c| c.name.0.as_str() == "Probe").expect("Probe");
    let mut emitted = String::from("class Probe\n");
    for m in &probe.methods {
        emitted.push_str(&roundhouse::emit::ruby::emit_method(m));
        emitted.push('\n');
    }
    emitted.push_str("end\n");
    assert!(emitted.contains("def head(pair)\n  puts c\nend"), "{emitted}");
    assert!(!emitted.lines().any(|l| l.trim() == "nil"), "{emitted}");
    assert!(emitted.contains("  rescue StandardError => e\n    report(e)\n  end"), "{emitted}");
    assert!(emitted.contains("-> { build(z) }"), "{emitted}");
    assert_no_void_warning(&emitted, "survey_placeholders");
}
