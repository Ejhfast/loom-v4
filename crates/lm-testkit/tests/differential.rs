//! Differential oracle tests: pure programs must produce the same
//! terminal value text in the typed-HIR oracle and in the verified
//! bytecode VM.

use lm_testkit::oracle::oracle_run;
use lm_testkit::{lm_files, repo_root, run_text};
use lm_vm::VmConfig;

fn compare(name: &str, text: &str) {
    let vm = run_text(name, text, VmConfig::default())
        .unwrap_or_else(|e| panic!("{name}: vm failed: {e}"));
    let oracle = oracle_run(name, text).unwrap_or_else(|e| panic!("{name}: oracle failed: {e}"));
    assert_eq!(vm, oracle, "{name}: the oracle and the VM disagree");
}

/// Every run-pass case and every example is part of the corpus.
#[test]
fn oracle_agrees_on_the_run_pass_suite() {
    let mut count = 0;
    for dir in ["tests/run-pass", "tests/run-fault"] {
        for path in lm_files(&repo_root().join(dir)) {
            let text = std::fs::read_to_string(&path).expect("case reads");
            compare(&path.display().to_string(), &text);
            count += 1;
        }
    }
    assert!(count >= 20, "the corpus lost cases: {count}");
}

#[test]
fn oracle_agrees_on_the_examples() {
    for example in [
        "examples/01-basics/factorial.lm",
        "examples/01-basics/control.lm",
        "examples/02-objects/counter.lm",
        "examples/02-objects/counts.lm",
        "examples/02-objects/closures.lm",
        "examples/03-types/expr.lm",
        "examples/03-types/generics.lm",
    ] {
        let text = std::fs::read_to_string(repo_root().join(example)).expect("example reads");
        compare(example, &text);
    }
}

/// Hand-written corpus programs that exercise generics, enums,
/// patterns, tuples, refinement, and rows.
#[test]
fn oracle_agrees_on_the_feature_corpus() {
    let corpus: &[&str] = &[
        // Generic functions and explicit arguments.
        "def identity[T](x: T): T\n  x\nend\n(identity(1), identity[String](\"a\"))\n",
        // Generic class with methods.
        "class Box[T]\n  value: T\n  def init(mut self, value: T)\n    self.value = value\n  \
         end\n  def get(self): T\n    self.value\n  end\nend\nBox(Box(41)).get().get() + 1\n",
        // Core Option and Result methods.
        "a: Option[Int] = Some(1)\nb: Option[Int] = None\n\
         (a.is_some(), b.is_none(), a.value_or(0), b.value_or(9))\n",
        "r: Result[Int, String] = Err(\"bad\")\n(r.is_err(), r.value_or(3))\n",
        // Ordering, Pair, Range.
        "(Ordering.Less.reverse().is_greater(), Pair(1, 2).swap().first, Range(3, 9).len(), \
         Range(3, 9).has(3), Range(3, 9).has(9))\n",
        // Enums with nested patterns and methods.
        "enum Tree\n  Leaf(v: Int)\n  Node(l: Tree, r: Tree)\n\n  def sum(self): Int\n    \
         case self\n    in Leaf(v) then v\n    in Node(l, r) then l.sum() + r.sum()\n    \
         end\n  end\nend\nNode(Leaf(1), Node(Leaf(2), Leaf(3))).sum()\n",
        // Bool and Int literal cases.
        "def f(n: Int): String\n  case n\n  in 0 then \"zero\"\n  in -1 then \"neg\"\n  \
         in _ then \"other\"\n  end\nend\ncase true\nin true then f(-1)\nin false then f(0)\nend\n",
        // String literal cases.
        "def color(name: String): Int\n  case name\n  in \"red\" then 1\n  in \"blue\" then 2\n  \
         in _ then 0\n  end\nend\ncolor(\"red\") * 100 + color(\"blue\") * 10 + color(\"x\")\n",
        // Tuples, indexing, and one-element tuples.
        "t = (1, (\"a\", true), 3)\none = (5,)\n(t[2], t[1][0], one[0], ())\n",
        // Flow refinement and casts.
        "class Animal\nend\nclass Dog < Animal\n  def bark(self): Int\n    3\n  end\nend\n\
         def go(a: Animal): Int\n  if a is Dog\n    (a as Dog).bark()\n  else\n    0\n  \
         end\nend\ngo(Dog()) * 10 + go(Animal())\n",
        // Bad cast faults identically.
        "class Animal\nend\nclass Dog < Animal\nend\na: Animal = Animal()\na as Dog\n0\n",
        // List and map get.
        "xs = [1, 2, 3]\nm = {\"a\": 5}\n\
         (xs.get(2).value_or(0), xs.get(9).value_or(-1), m.get(\"a\").value_or(0), \
         m.get(\"b\").is_none())\n",
        // Case as a value with Never arms.
        "def pick(o: Option[Int]): Int\n  case o\n  in Some(v)\n    return v * 2\n  in None \
         then 7\n  end\nend\npick(Some(4)) + pick(None)\n",
        // Effect rows on higher-order calls, empty in practice.
        "def apply[T, U, effect e](x: T, f: (T) -> U with e): U with e\n  f(x)\nend\n\
         apply(3, do |n: Int|: Int n * n end)\n",
        // Closures over enum values.
        "make = do |v: Int|: Option[Int] Some(v) end\ncase make(6)\nin Some(v) then v\nin None \
         then 0\nend\n",
        // Freezing and frozen faults agree.
        "xs = [1]\nxs.freeze()\nxs.push(2)\nxs\n",
        // Shadowed prelude names.
        "enum Option\n  A\n  B\nend\nx: Option = A\ncase x\nin A then 1\nin B then 2\nend\n",
        // Wide joins over an enum family.
        "def flip(n: Int): Option[Int]\n  if n > 0\n    Some(n)\n  else\n    None\n  end\nend\n\
         (flip(2).value_or(0), flip(-2).value_or(0))\n",
        // Interpolation with case values.
        "o: Option[Int] = Some(2)\n\"value {o.value_or(0)}!\"\n",
        // Deeply mixed data.
        "rows = [(1, \"a\"), (2, \"b\")]\ntotal = 0\nnames = StringBuilder()\ni = 0\n\
         while i < rows.len()\n  total = total + rows.at(i)[0]\n  \
         names.append(rows.at(i)[1])\n  i = i + 1\nend\n(total, names.build())\n",
        // Labeled arguments reorder to declaration order.
        "def mix(a: Int, b: Int, c: Int): Int\n  a * 100 + b * 10 + c\nend\n\
         mix(1, c: 3, b: 2) + mix(c: 3, b: 2, a: 1)\n",
        // Sibling inference in literals and branch joins.
        "xs = [Some(1), None]\nm = {1: Some(\"one\"), 2: None}\n\
         o = if xs.len() > 1\n  None\nelse\n  Some(5)\nend\n\
         (xs.at(1).is_none(), m.at(2).is_none(), o.is_none())\n",
        // Nested sibling inference.
        "xs = [Some(None), Some(Some(1)), None]\ncase xs.at(1)\nin Some(Some(v)) then v\n\
         in Some(None) then 8\nin None then 9\nend\n",
        // A mutable closure parameter with a mutable argument.
        "xs: [Int] = [1]\nf = do |mut ys: [Int]|: () ys.push(2) end\nf(xs)\nxs.len()\n",
        // A nested exact-arm scrutinee is exhaustive.
        "s = Some(Some(3))\ncase s\nin Some(Some(v)) then v\nend\n",
    ];
    for (idx, text) in corpus.iter().enumerate() {
        compare(&format!("corpus[{idx}]"), text);
    }
}
