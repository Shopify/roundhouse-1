//! Constructor keywords on the CRuby tree.
//!
//! The shared constructor lowering refuses `Class.new(k: v)` where the
//! effective `new` cannot be verified or the keywords cannot be moved into
//! a flattened `initialize`'s positional slots. Those refusals exist for
//! targets that keep ingest's flattened keywords positional. The CRuby
//! tree writes flattened keywords back, so it runs those calls as written
//! (`expr::CONSTRUCTOR_KEYWORDS_AS_WRITTEN`), restores calls the lowering
//! did move (`lower::keyword_marks`), and writes an optional keyword
//! beside a kept `**rest` as a keyword (`Param::respelled_keywords_from`).
//!
//! Native Ruby is the oracle: the same library and script run on CRuby
//! directly and in the emitted tree must print the same lines.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LIBRARY: &str = r##"class KcBase
  attr_reader :shop

  def initialize(shop: nil)
    @shop = shop
  end
end

class KcRequest < KcBase
  attr_reader :api

  def initialize(api: nil, **args)
    super(**args)
    @api = api || "default-api"
  end
end

class KcHooked
  def self.method_added(_name)
  end

  attr_reader :label, :step

  def initialize(label: "default", step: 1)
    @label = label
    @step = step
  end
end

class KcOrdered
  attr_reader :first, :second

  def initialize(first: 1, second: 2)
    @first = first
    @second = second
  end
end

class KcEncoder
  attr_reader :resource, :pool_size, :limit

  def initialize(resource, pool_size: 1, limit: 100)
    @resource = resource
    @pool_size = pool_size
    @limit = limit
  end
end

class KcCalls
  Subject = Struct.new(:id, keyword_init: true)

  def self.build(options)
    request = KcRequest.new(api: "a", shop: "s")
    hooked = KcHooked.new(step: 3)
    ordered = KcOrdered.new(second: 5, first: 4)
    splatted = KcOrdered.new(**options)
    filled = KcOrdered.new(second: 7)
    encoder = KcEncoder.new("r", limit: 9)
    subject = Subject.new(id: 4)
    [
      [request.api, request.shop],
      [hooked.label, hooked.step],
      [ordered.first, ordered.second],
      [splatted.first, splatted.second],
      [filled.first, filled.second],
      [encoder.resource, encoder.pool_size, encoder.limit],
      subject.id,
    ]
  end
end
"##;

const ASSERTIONS: &str = r##"
KcCalls.build({ second: 9 }).each { |row| p row }
"##;

fn native() -> String {
    let out = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("{LIBRARY}\n{ASSERTIONS}"))
        .output()
        .expect("spawn ruby");
    assert!(out.status.success(), "native oracle failed:\n{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("utf8")
}

fn emitted_kc_sources(dir: &std::path::Path) -> String {
    let mut out = String::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir).expect("read emitted dir").flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            out.push_str(&emitted_kc_sources(&path));
        } else if path.extension().is_some_and(|e| e == "rb") {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            if text.contains("Kc") {
                out.push_str(&format!("# {}\n{text}\n", path.display()));
            }
        }
    }
    out
}

#[test]
fn constructor_keywords_bind_as_native_ruby_binds_them() {
    let expected = native();
    assert_eq!(expected.lines().count(), 7, "oracle printed:\n{expected}");
    let run = emit_and_run::real_blog()
        .write("app/lib/kc_calls.rb", LIBRARY)
        .run_ruby(ASSERTIONS);
    let emitted = emitted_kc_sources(&run.emitted.join("app"));
    assert_eq!(
        run.stdout, expected,
        "emitted CRuby differs from native\nstderr:\n{}\n{emitted}",
        run.stderr
    );
    for written in [
        "def initialize(api: nil, **args)",
        "KcHooked.new(step: 3)",
        "KcOrdered.new(second: 5, first: 4)",
        "KcOrdered.new(**options)",
        "KcOrdered.new(second: 7)",
        "KcEncoder.new(\"r\", limit: 9)",
    ] {
        assert!(emitted.contains(written), "expected {written:?} in the emitted tree:\n{emitted}");
    }
    assert!(
        !emitted.contains("constructor keyword arguments not supported"),
        "no constructor refusal stub on the CRuby tree:\n{emitted}"
    );
}
