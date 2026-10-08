//! `for` is `each` over the collection with the enclosing scope's
//! locals (`ingest::expr::ingest_for`); tests/for_loop_cruby.rs runs it
//! against CRuby. A loop variable other than plain locals is refused.
use roundhouse::expr::{Expr, ExprNode, LValue};

fn ingest(body: &str) -> Result<Expr, String> {
    let result = ruby_prism::parse(body.as_bytes());
    let program = result.node().as_program_node().unwrap();
    let node = program.statements().body().iter().find(|node| node.as_for_node().is_some()).expect("for input");
    roundhouse::ingest::ingest_expr(&node, body).map_err(|e| e.to_string())
}

/// The names declared ahead of the `each`, and the block's parameters.
fn shape(body: &str) -> (Vec<String>, Vec<String>) {
    let expr = ingest(body).expect("for is lowered");
    let ExprNode::Seq { exprs } = &*expr.node else { panic!("not a Seq: {expr:?}") };
    let (each, declared) = exprs.split_last().unwrap();
    let declared = declared
        .iter()
        .map(|e| match &*e.node {
            ExprNode::Assign { target: LValue::Var { name, .. }, value } => {
                assert!(matches!(&*value.node, ExprNode::Var { name: read, .. } if read == name), "{e:?}");
                name.as_str().to_string()
            }
            other => panic!("not a declaration: {other:?}"),
        })
        .collect();
    let ExprNode::Send { method, block: Some(block), .. } = &*each.node else { panic!("not an each: {each:?}") };
    assert_eq!(method.as_str(), "each");
    let ExprNode::Lambda { params, .. } = &*block.node else { panic!("not a block: {block:?}") };
    (declared, params.iter().map(|p| p.as_str().to_string()).collect())
}

#[test]
fn the_loop_variable_and_body_locals_are_the_enclosing_scope_s() {
    let (declared, params) = shape("x = 10; for x in items; y = x; z ||= 1; [1].each { |i| own = i }; end; x");
    assert_eq!(declared, ["x", "y", "z"]);
    assert_eq!(params, ["__rh_for_x"]);
}

#[test]
fn destructuring_takes_the_yielded_values_as_block_parameters() {
    let (declared, params) = shape("for key, value in items; puts key, value; end");
    assert_eq!(declared, ["key", "value"]);
    assert_eq!(params, ["__rh_for_key", "__rh_for_value"]);
}

#[test]
fn a_loop_variable_other_than_plain_locals_is_refused() {
    for body in ["for @x in items; end", "for a, *rest in items; end", "for self.x in items; end"] {
        let error = ingest(body).expect_err(body);
        assert!(error.contains("`for` with a loop variable other than plain locals"), "{body}: {error}");
    }
}
