//! Block and lambda parameters that Lambda IR cannot hold are refused
//! by name, never dropped.
//!
//! Lambda IR carries required, rest, post and block parameters. Ingest
//! read only those, so Core's `lambda do |item:, context: 1| … end`
//! emitted as `lambda do … end`, and `proc { |x, k: 3| }` as
//! `proc { |x| }`, with bodies reading names nothing bound. Optional,
//! keyword and named keyword-rest parameters are now refused at the
//! block: an error diagnostic that names them, and a block whose body
//! raises `NotImplementedError` when called. The block stays in place,
//! so a `define_method` still defines its method and the statement
//! around it still runs (Core's `WebUrlHelpersFactory` builds its
//! helpers at load with `do |params: {}, **args|` blocks).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const PROBE: &str = r##"module Blockparamlab
  class Probe
    def self.keywords
      f = lambda do |item:, context: 1|
        [item, context]
      end
      f.call(item: 2)
    end

    def self.optional
      [1].map { |x, k = 3| x + k }
    end

    def self.keyword_rest
      proc { |x, **opts| [x, opts] }.call(1, a: 2)
    end

    def self.stabby
      ->(x, k: 3) { x + k }.call(1)
    end

    def self.representable
      [
        [[1, 2], 3].each_slice(2).map { |(a, b), *rest| [a, b, rest] },
        [4].map { |x, **| x },
        ->(*, payload) { payload }.call(5, 6),
      ]
    end
  end
end
"##;

#[test]
fn unrepresented_block_parameters_are_refused_by_name() {
    let (emitted, errors) = emit_and_run::real_blog()
        .write("app/lib/blockparamlab/probe.rb", PROBE)
        .emit(BuildTarget::Ruby);
    let all = errors.join("\n");
    for (method, named) in [
        (
            "keywords",
            "parameters `item:`, `context: …` not represented",
        ),
        ("optional", "parameter `k = …` not represented"),
        ("keyword_rest", "parameter `**opts` not represented"),
        ("stabby", "parameter `k: …` not represented"),
    ] {
        assert!(
            all.contains(named),
            "{method}: no diagnostic naming {named}:\n{all}"
        );
    }
    assert_eq!(
        errors
            .iter()
            .filter(|e| e.contains("not represented in block IR"))
            .count(),
        4,
        "one refusal per block:\n{all}"
    );

    let source = std::fs::read_to_string(emitted.join("app/models/blockparamlab/probe.rb"))
        .expect("emitted probe");
    let parse = ruby_prism::parse(source.as_bytes());
    let parse_errors: Vec<String> = parse.errors().map(|e| e.message().to_string()).collect();
    assert!(parse_errors.is_empty(), "{parse_errors:?} in:\n{source}");
    assert!(
        !source.contains("lambda do\n"),
        "the keyword lambda was emitted without its parameters:\n{source}"
    );
    assert!(
        !source.contains("|x, opts|"),
        "the keyword rest was emitted as a positional:\n{source}"
    );

    let main = emitted.join("main").display().to_string();
    let script = format!(
        "require {main:?}\nMain.configure_default_adapter!\nrequire_relative \"app/models/blockparamlab/probe\"\n\
         p Blockparamlab::Probe.representable\n\
         %i[keywords optional keyword_rest stabby].each do |m|\n  \
           Blockparamlab::Probe.public_send(m)\n  puts \"#{{m}} ran\"\n\
         rescue NotImplementedError => e\n  puts \"#{{m}} refused\"\n\
         end\n"
    );
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(script)
        .current_dir(&emitted)
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("spawn ruby");
    assert!(
        output.status.success(),
        "emitted program failed in {}:\n{}\n{source}",
        emitted.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "[[[1, 2, [3]]], [4], 6]\nkeywords refused\noptional refused\nkeyword_rest refused\nstabby refused\n",
        "{source}"
    );
}
