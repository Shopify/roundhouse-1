//! A superclass written relative to the enclosing module, on the CRuby
//! tree against CRuby.
//!
//! shopify-money's `class Accounting < Fuzzy` sits inside `class Money;
//! module Parser`, so Ruby binds `Fuzzy` to `Money::Parser::Fuzzy`.
//! Ingest kept the parent as written (`Fuzzy`), which names no class.
//! So the subclass inherited nothing in analysis, and its `super(input,
//! currency, **options)` was refused as "keyword splat in ordinary
//! super": the destination's keyword ABI (`strict: false`) could not be
//! checked. The parent now resolves as Ruby's lookup does. The emitted
//! tree must answer what CRuby answers, line for line, including the
//! ArgumentError for a keyword the destination does not take, and
//! report no diagnostics.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

use roundhouse::project::BuildTarget;

const FUZZY: &str = r##"class Money
  module Parser
    class Fuzzy
      def self.parse(input, currency = nil, **options)
        new.parse(input, currency, **options)
      end

      def parse(input, currency = nil, strict: false)
        "#{kind}:#{input}|#{currency.inspect}|strict=#{strict}"
      end

      def kind
        "fuzzy"
      end
    end
  end
end
"##;

const ACCOUNTING: &str = r##"class Money
  module Parser
    class Accounting < Fuzzy
      def parse(input, currency = nil, **options)
        super(input.gsub(/\(\$?(.*?)\)/, '-\1'), currency, **options)
      end

      def kind
        "#{super}+accounting"
      end
    end
  end
end
"##;

const MONEY: &str = "class Money\nend\n";

const SCRIPT: &str = r##"
puts "superclass: #{Money::Parser::Accounting.superclass}"
puts "plain: #{Money::Parser::Accounting.parse("($5.00)")}"
puts "currency: #{Money::Parser::Accounting.parse("($1)", "USD")}"
puts "strict: #{Money::Parser::Accounting.parse("(2)", "CAD", strict: true)}"
puts "instance: #{Money::Parser::Accounting.new.parse("($3)", strict: false)}"
begin
  Money::Parser::Accounting.parse("1", nil, bogus: 1)
rescue ArgumentError => e
  puts "raised: #{e.message}"
end
puts "fuzzy: #{Money::Parser::Fuzzy.parse("($4)")}"
"##;

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(str::to_string).collect()
}

#[test]
fn a_superclass_relative_to_the_enclosing_module_runs_as_in_cruby() {
    let scratch = std::env::temp_dir().join(format!("roundhouse-namespaced-superclass-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(scratch.join("fuzzy.rb"), FUZZY).unwrap();
    std::fs::write(scratch.join("accounting.rb"), ACCOUNTING).unwrap();
    let native = emit_and_run::ruby()
        .arg("-W1")
        .arg("-e")
        .arg(format!(
            "load {:?}\nload {:?}\n{SCRIPT}",
            scratch.join("fuzzy.rb").display().to_string(),
            scratch.join("accounting.rb").display().to_string()
        ))
        .output()
        .expect("CRuby control");
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = lines(&native.stdout);
    for probe in [
        "superclass: Money::Parser::Fuzzy",
        "plain: fuzzy+accounting:-5.00|nil|strict=false",
        "strict: fuzzy+accounting:-2|\"CAD\"|strict=true",
        "raised: unknown keyword: :bogus",
    ] {
        assert!(expected.iter().any(|l| l == probe), "native control lacks {probe:?}: {expected:#?}");
    }

    let (tree, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/models/money.rb", MONEY)
        .write("app/models/money/parser/fuzzy.rb", FUZZY)
        .write("app/models/money/parser/accounting.rb", ACCOUNTING)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let run = Command::new("ruby")
        .current_dir(&tree)
        .env("BLOG_DB", ":memory:")
        .arg("-W1")
        .arg("-e")
        .arg(format!("require File.expand_path(\"main\", Dir.pwd)\nMain.configure_default_adapter!\n{SCRIPT}"))
        .output()
        .expect("spawn ruby");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "emitted tree failed: {stderr}");
    assert_eq!(lines(&run.stdout), expected, "emitted Ruby diverges from CRuby line for line\n{stderr}");
}
