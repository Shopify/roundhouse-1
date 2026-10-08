//! The modules a file opens, read for a file ingest carried whole that
//! declares nothing of its own: kafka-shopify 5.2.3's writers.rb is
//! `module KafkaShopify; module Writers; autoload(...); end; end`. Its
//! modules hold no method, constant or call the ingest keeps, so it
//! produces no library class, and its `autoload`s are app/models.rb's
//! registrations. Natively a `require` of it still defines
//! `KafkaShopify::Writers`, the namespace those autoloads hang from;
//! the emitted program opens the same modules there
//! (`emit::ruby::library`'s file programs).

use ruby_prism::Node;

use super::util::module_name_path;

/// Each module the file opens, as a full path, outermost first, in
/// source order (`[["KafkaShopify"], ["KafkaShopify", "Writers"]]`).
/// `None` when the file opens a class (whose superclass a reopen
/// would have to repeat), a module by a path that is not constant, or
/// a module inside anything but a module: only the plain nesting of
/// modules is read.
pub fn opened_modules(source: &str) -> Option<Vec<Vec<String>>> {
    let parsed = ruby_prism::parse(source.as_bytes());
    let program = parsed.node();
    let program = program.as_program_node()?;
    let mut out = Vec::new();
    walk(program.statements().body().iter(), &[], &mut out)?;
    Some(out)
}

fn walk<'a>(statements: impl Iterator<Item = Node<'a>>, outer: &[String], out: &mut Vec<Vec<String>>) -> Option<()> {
    for statement in statements {
        if statement.as_class_node().is_some() || statement.as_singleton_class_node().is_some() {
            return None;
        }
        let Some(module) = statement.as_module_node() else { continue };
        let mut path = outer.to_vec();
        path.extend(module_name_path(&module)?);
        out.push(path.clone());
        if let Some(body) = module.body() {
            let Some(body) = body.as_statements_node() else { return None };
            walk(body.body().iter(), &path, out)?;
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::opened_modules;

    #[test]
    fn reads_nested_and_compact_modules() {
        let text = "module A\n  module B::C\n    autoload(:D, \"a/d\")\n  end\nend\nmodule E\nend\n";
        let paths = opened_modules(text).unwrap();
        let want: Vec<Vec<String>> = vec![
            vec!["A".into()],
            vec!["A".into(), "B".into(), "C".into()],
            vec!["E".into()],
        ];
        assert_eq!(paths, want);
        assert_eq!(opened_modules("module A\n  class B < C\n  end\nend\n"), None);
    }
}
