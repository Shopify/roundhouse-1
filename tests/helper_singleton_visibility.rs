//! A helper module exposes its INSTANCE methods, and only to the views,
//! helpers and classes that mix it in — never its singleton.
//!
//! Core's `MoneyHelper` keeps a private `class << self; def format(money,
//! …)` next to instance `money_with_symbol` / `money_without_symbol` that
//! delegate to same-named singletons. Every helper method used to enter
//! the flat bare-call index and the rewrite ran over every emitted class,
//! so `format("%06i", 4242)` in a model became `MoneyHelper.format(…)`;
//! and the flip of the instance surface onto the module replaced the
//! singleton `money_with_symbol` with a copy that called itself.
//!
//! Each case runs the same expressions natively (plain Ruby over the
//! authored sources, a view simulated as an object extended with the
//! helpers) and against the emitted app, and compares the output.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const MONEY_HELPER: &str = r#"module MoneyHelper
  class << self
    def money_with_symbol(m)
      format(m)
    end

    private

    def format(m)
      "$#{m}"
    end
  end

  def money_with_symbol(m)
    "view:" + MoneyHelper.money_with_symbol(m)
  end

  def money_label(m)
    "price " + format("%.2f", m)
  end
end
"#;

const PAD_HELPER: &str = r#"module PadHelper
  module_function

  def padded(n)
    format("%04d", n)
  end
end
"#;

const PAD: &str = r#"class Pad
  def pad(n)
    format("%06i", n)
  end

  def self.class_pad(n)
    format("%03i", n)
  end
end
"#;

const SHOWN: &str = r#"class Shown
  include MoneyHelper

  def show(m)
    money_with_symbol(m)
  end
end
"#;

const PADDER: &str = r#"class Padder
  include PadHelper

  def go(n)
    padded(n)
  end
end
"#;

/// A concern's body runs as its includer, which includes the helper.
const PRICEABLE: &str = r#"module Priceable
  def label(n)
    money_label(n)
  end
end
"#;

const TICKET: &str = r#"class Ticket
  include MoneyHelper
  include Priceable
end
"#;

const PRICE_VIEW: &str =
    "<span><%= money_label(3) %>|<%= money_with_symbol(4) %>|<%= padded(7) %></span>\n";

/// The expressions both runs print, one per line. The last is the view.
const COMMON: &str = r#"puts Pad.new.pad(4242)
puts Pad.class_pad(5)
puts Shown.new.show(9)
puts Padder.new.go(12)
puts MoneyHelper.money_with_symbol(1)
puts PadHelper.padded(3)
puts Ticket.new.label(2)
"#;

fn native_output() -> String {
    let dir = std::env::temp_dir().join(format!("helper-singleton-native-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    for (name, src) in [
        ("money_helper.rb", MONEY_HELPER),
        ("pad_helper.rb", PAD_HELPER),
        ("pad.rb", PAD),
        ("shown.rb", SHOWN),
        ("padder.rb", PADDER),
        ("priceable.rb", PRICEABLE),
        ("ticket.rb", TICKET),
    ] {
        std::fs::write(dir.join(name), src).expect("write native source");
    }
    // Rails renders a template on a view object every helper is mixed
    // into; `padded` is a private (module_function) copy there.
    let script = format!(
        "%w[money_helper pad_helper pad shown padder priceable ticket].each {{ |f| require_relative f }}\n{COMMON}\
         view = Object.new.extend(MoneyHelper).extend(PadHelper)\n\
         print view.instance_eval {{ \"<span>#{{money_label(3)}}|#{{money_with_symbol(4)}}|#{{padded(7)}}</span>\\n\" }}\n"
    );
    std::fs::write(dir.join("main.rb"), script).expect("write native main");
    let out = emit_and_run::ruby().arg("main.rb").current_dir(&dir).output().expect("spawn ruby");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "native run failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8")
}

#[test]
fn emitted_helpers_match_native_visibility() {
    let native = native_output();
    assert_eq!(
        native,
        "004242\n005\nview:$9\n0012\n$1\n0003\nprice 2.00\n<span>price 3.00|view:$4|0007</span>\n",
        "native baseline"
    );
    let run = emit_and_run::real_blog()
        .write("app/helpers/money_helper.rb", MONEY_HELPER)
        .write("app/helpers/pad_helper.rb", PAD_HELPER)
        .write("app/models/pad.rb", PAD)
        .write("app/models/shown.rb", SHOWN)
        .write("app/models/padder.rb", PADDER)
        .write("app/models/concerns/priceable.rb", PRICEABLE)
        .write("app/models/ticket.rb", TICKET)
        .write("app/views/articles/_price.html.erb", PRICE_VIEW)
        .run_ruby(&format!("{COMMON}print Views::Articles.price(nil)\n"));
    run.assert_passes();
    assert_eq!(run.stdout, native, "emitted output differs from native");
}

/// A helper test's bare calls resolve on the test instance, which mixes in
/// the helper's instance methods only: `format` there is still Kernel's.
#[test]
fn helper_test_bare_calls_skip_the_singleton() {
    emit_and_run::real_blog()
        .write("app/helpers/money_helper.rb", MONEY_HELPER)
        .write(
            "test/helpers/money_helper_test.rb",
            "require \"test_helper\"\n\nclass MoneyHelperTest < ActionView::TestCase\n  test \"instance surface\" do\n    assert_equal \"price 1.00\", money_label(1)\n    assert_equal \"0042\", format(\"%04d\", 42)\n  end\nend\n",
        )
        .run_test("test/helpers/money_helper_test.rb")
        .assert_passes();
}

/// The registry Rails' view surface is read from: instance methods and
/// `module_function` copies, no singleton-defined methods.
#[test]
fn helper_index_holds_only_the_instance_surface() {
    // A module the helper includes contributes its instance surface too
    // (lobsters' ApplicationHelper includes lib/'s TimeAgoInWords), and
    // likewise not its singleton.
    let files: Vec<(&str, &str)> = vec![
        ("app/helpers/money_helper.rb", MONEY_HELPER),
        ("app/helpers/pad_helper.rb", PAD_HELPER),
        (
            "app/helpers/sizes_helper.rb",
            "module SizesHelper\n  include SizeTools\n\n  def size_label(n)\n    kib(n)\n  end\nend\n",
        ),
        (
            "lib/size_tools.rb",
            "module SizeTools\n  def self.format(n)\n    n.to_s\n  end\n\n  def kib(n)\n    format(\"%d KiB\", n / 1024)\n  end\nend\n",
        ),
        ("app/models/pad.rb", PAD),
    ];
    let tree = files
        .into_iter()
        .map(|(p, c)| (std::path::PathBuf::from(p), c.as_bytes().to_vec()))
        .collect();
    let app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest");
    let index: std::collections::BTreeMap<String, String> = app
        .helper_method_index
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.0.as_str().to_string()))
        .collect();
    let expected: std::collections::BTreeMap<String, String> = [
        ("kib", "SizeTools"),
        ("money_label", "MoneyHelper"),
        ("money_with_symbol", "MoneyHelper"),
        ("padded", "PadHelper"),
        ("size_label", "SizesHelper"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert_eq!(index, expected);
}
