//! `include ::Q::R` inside a class that also declares a nested `Q::R`
//! names the TOP-LEVEL `Q::R`. Ingest dropped the root and the lexical
//! qualification of library-class includes then retargeted the include
//! to the nested module: the emitted class mixed in the wrong module.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const TOP: &str = "module Q\n  module R\n    def who = :top\n    def top_only = 1\n  end\nend\n";
const PROBE: &str = "class RootedProbe\n  module Q\n    module R\n      def who = :local\n    end\n  end\n  include ::Q::R\nend\n";
const RELATIVE: &str = "class RelativeProbe\n  module Q\n    module R\n      def who = :local\n    end\n  end\n  include Q::R\nend\n";
const ASSERTIONS: &str = r#"
raise "rooted include: #{RootedProbe.new.who.inspect}" unless RootedProbe.new.who == :top
raise "rooted include lost the top module" unless RootedProbe.new.top_only == 1
raise "relative include: #{RelativeProbe.new.who.inspect}" unless RelativeProbe.new.who == :local
"#;

#[test]
fn rooted_include_mixes_in_the_top_level_module_like_native() {
    let native = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("{TOP}\n{PROBE}\n{RELATIVE}\n{ASSERTIONS}"))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    emit_and_run::real_blog()
        .write("app/services/q/r.rb", TOP)
        .write("app/services/rooted_probe.rb", PROBE)
        .write("app/services/relative_probe.rb", RELATIVE)
        .run_ruby(ASSERTIONS)
        .assert_passes();
}

#[test]
fn rooted_include_is_typed_against_the_top_level_module() {
    use roundhouse::analyze::{Analyzer, diagnose};
    use roundhouse::det::HashMap;
    use std::path::PathBuf;
    let probe = PROBE.replace(
        "  include ::Q::R\n",
        "  include ::Q::R\n\n  def use_top = top_only\nend\nclass RootedProbe\n",
    );
    let tree: HashMap<PathBuf, Vec<u8>> = [
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\nend\n"),
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("app/services/q/r.rb", TOP),
        ("app/services/rooted_probe.rb", probe.as_str()),
    ]
    .into_iter()
    .map(|(p, s)| (PathBuf::from(p), s.as_bytes().to_vec()))
    .collect();
    let mut app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingests");
    let probe = app.library_classes.iter().find(|c| c.name.0.as_str() == "RootedProbe").unwrap();
    assert_eq!(
        probe.includes.iter().map(|i| i.0.as_str()).collect::<Vec<_>>(),
        ["::Q::R"],
        "the root is kept where `RootedProbe::Q` shadows `Q`"
    );
    Analyzer::new(&app).analyze(&mut app);
    let text = diagnose(&app).iter().map(|d| d.message.clone()).collect::<Vec<_>>().join("\n");
    assert!(!text.contains("includes unresolved"), "{text}");
    assert!(!text.contains("`top_only`"), "{text}");
    let probe = app.library_classes.iter().find(|c| c.name.0.as_str() == "RootedProbe").unwrap();
    let use_top = probe.methods.iter().find(|m| m.name.as_str() == "use_top").unwrap();
    // `top_only` exists only on the top-level module, and returns 1.
    assert_eq!(use_top.body.ty, Some(roundhouse::ty::Ty::Int), "{text}");
}
