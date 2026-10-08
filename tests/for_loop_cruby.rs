//! `for x in coll` through ingest, lowering and Ruby emit, against CRuby
//! on the same source.
//!
//! `for` is `coll.each` whose loop variables and body locals belong to
//! the enclosing scope: after the loop the variable holds the last
//! element and a body local the last iteration's value (both nil when
//! nothing iterated), a body local carries from one iteration to the
//! next, an outer local is overwritten, and `for k, v in hash`
//! destructures as `|k, v|` does. activemerchant 1.137.0 parses gateway
//! responses with `for line in body.split('&')` (cardknox, psl_card,
//! usa_epay_transaction), builds XML with nested `for key, value in
//! params` (linkpoint), and active_utils' `Validateable#attributes=` is
//! `for key, value in attributes; send("#{key}=", value); end`; each was
//! refused and raised NotImplementedError where it ran.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SOURCE: &str = r##"class ForProbe
  Pair = Struct.new(:a, :b) do
    def each
      yield a, b
      yield b
      :each_done
    end
  end

  def self.parse(body)
    fields = {}
    for line in body.split("&")
      key, value = *line.scan(/^(\w+)=(.*)$/).flatten
      fields[key] = value.to_s
    end
    [fields, line, key]
  end

  def self.empty
    for item in []
      seen = item
    end
    [item, seen]
  end

  def self.carry(xs)
    out = []
    for x in xs
      prev = x * 10 if x == 1
      out << prev
    end
    [out, prev, x]
  end

  def self.outer(xs)
    x = :before
    for x in xs
    end
    x
  end

  def self.hash_pairs(h)
    out = []
    for k, v in h
      out << "#{k}=#{v}"
    end
    [out, k, v]
  end

  def self.yields
    out = []
    one = for z in Pair.new(1, 2)
      out << z
    end
    for a, b in Pair.new(3, 4)
      out << [a, b]
    end
    [out, one, a, b]
  end

  def self.control(xs)
    hit = for n in xs
      next if n.odd?
      break n * 100 if n > 2
    end
    [hit, n]
  end

  def self.nested(items)
    out = []
    for item in items
      for key, value in item
        out << [key, value]
        inner = key
      end
      items_seen = (items_seen || 0) + 1
    end
    [out, inner, items_seen, key]
  end

  def self.block_local(xs)
    for x in xs
      [1].each { |i| own = i }
    end
    defined?(own).inspect
  end

  def self.first_even(xs)
    for x in xs
      return x if x.even?
    end
    :none
  end

  attr_reader :log

  def initialize
    @log = []
  end

  def attributes=(attributes)
    for key, value in attributes
      send("#{key}=", value)
    end
  end

  def name=(value)
    @log << [:name, value]
  end

  def size=(value)
    @log << [:size, value]
  end
end
"##;

const SCRIPT: &str = r#"p ForProbe.parse("a=1&b=two&c=")
p ForProbe.empty
p ForProbe.carry([1, 2, 3])
p ForProbe.outer([7, 8])
p ForProbe.outer([])
p ForProbe.hash_pairs({ x: 1, y: 2 })
p ForProbe.yields
p ForProbe.control([1, 2, 3, 4])
p ForProbe.control([1, 2])
p ForProbe.nested([{ a: 1 }, { b: 2, c: 3 }])
p ForProbe.block_local([1])
p ForProbe.first_even([1, 4, 6])
p ForProbe.first_even([1])
probe = ForProbe.new
probe.attributes = { name: "n", size: 3 }
p probe.log
"#;

fn native_lines() -> Vec<String> {
    let native = emit_and_run::ruby().arg("-e").arg(format!("{SOURCE}\n{SCRIPT}")).output().expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    String::from_utf8_lossy(&native.stdout).lines().map(str::to_string).collect()
}

#[test]
fn for_loops_behave_as_in_cruby_after_emission() {
    let expected = native_lines();
    // The control exercises each scoping rule, or matching it would
    // prove nothing.
    assert_eq!(
        expected,
        [
            r#"[{"a" => "1", "b" => "two", "c" => ""}, "c=", "c"]"#,
            "[nil, nil]",
            "[[10, 10, 10], 10, 3]",
            "8",
            ":before",
            r#"[["x=1", "y=2"], :y, 2]"#,
            "[[1, 2, [3, 4], [4, nil]], :each_done, 4, nil]",
            "[400, 4]",
            "[[1, 2], 2]",
            "[[[:a, 1], [:b, 2], [:c, 3]], :c, 2, :c]",
            r#""nil""#,
            "4",
            ":none",
            r#"[[:name, "n"], [:size, 3]]"#,
        ],
        "native control"
    );
    let run = emit_and_run::real_blog().write("app/services/for_probe.rb", SOURCE).run_ruby(SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(actual, expected, "emitted Ruby diverges from CRuby line for line");
}
