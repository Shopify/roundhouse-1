//! Array and number methods the TypeScript emit translates, on typed
//! receivers, with the empty-Array, not-found and negative answers Ruby
//! gives.

use super::emit_and_run::{self, Overlay};

pub const PROBE: &str = r#"class ArrayProbe
  def self.run(xs, words)
    none = xs.select { |x| x > 9 }
    seen = []
    xs.each_with_index { |x, i| seen.push(x * i) }
    [
      xs.sum, xs.sum { |x| x * 2 }, none.sum,
      xs.select { |x| x > 1 }, xs.filter { |x| x != 2 }, xs.reject { |x| x > 1 },
      xs.collect { |x| x + 1 }, xs.detect { |x| x > 1 }, xs.detect { |x| x > 9 },
      xs.flat_map { |x| [x, x] }, [nil, 1, nil, 2].compact, [1, 1, 2, 3, 3].uniq,
      xs.max, xs.min, none.max, none.min,
      xs.first(2), xs.take(5), xs.drop(1), xs.drop(9), xs.last(2), xs.last(0), xs.dup,
      xs.count { |x| x > 1 }, [1, 2, 1].count(1),
      xs.inject(10) { |a, x| a + x }, xs.reduce { |a, x| a * x }, none.inject { |a, x| a + x },
      xs.index(2), xs.index(9), xs.index { |x| x > 2 }, words.find_index { |w| w == "b" },
      seen
    ]
  end
end
"#;

/// `JSON.generate(ArrayProbe.run([1, 2, 3], ["a", "b"]))` under CRuby.
#[allow(dead_code)]
pub const EXPECTED: &str = "[6,12,0,[2,3],[1,3],[1],[2,3,4],2,null,[1,1,2,2,3,3],[1,2],[1,2,3],3,1,null,null,[1,2],[1,2,3],[2,3],[],[2,3],[],[1,2,3],2,2,16,6,null,1,null,2,1,[0,2,6]]";

pub const NUMBER_PROBE: &str = r#"class NumberProbe
  def self.run(n, m, f, out)
    out << 1 << 2
    acc = []
    n.times { |i| acc.push(i) }
    [out, acc, n / m, n % m, -n % m, n ** 2, f / 2, n.to_s, f.to_i, n.odd?, n.even?, n.zero?, n.positive?, n.negative?,
     (-n).abs, n.pred, n.succ, n.fdiv(2), f.floor, f.ceil, f.round, (-f).round, n.round, n.clamp(0, 2), n.between?(1, 3),
     n <=> m, m <=> n, n <=> 7, n == m]
  end
end
"#;

/// `JSON.generate(NumberProbe.run(7, 2, 2.5, [0]))` under CRuby.
#[allow(dead_code)]
pub const NUMBER_EXPECTED: &str = "[[0,1,2],[0,1,2,3,4,5,6],3,1,1,49,1.25,\"7\",2,true,false,false,true,false,7,6,8,3.5,2,3,3,-3,7,2,false,1,-1,0,false]";

pub fn overlay() -> Overlay {
    emit_and_run::real_blog()
        .write("app/models/array_probe.rb", PROBE)
        .write("app/models/number_probe.rb", NUMBER_PROBE)
        .edit(
            "app/controllers/articles_controller.rb",
            "  def index\n",
            "  def index\n    @probe = ArrayProbe.run([1, 2, 3], [\"a\", \"b\"])\n    @numbers = NumberProbe.run(7, 2, 2.5, [0])\n",
        )
}
