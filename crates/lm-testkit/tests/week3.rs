//! Week-3 static rules: generics, tuples, enums, patterns, flow
//! refinement, casts, and effect rows. Each rule has positive and
//! negative coverage, plus depth-guard cases for the new recursive
//! productions.

use lm_testkit::{compile_text, run_text};
use lm_vm::VmConfig;

fn code_of(source: &str) -> String {
    let rendered = compile_text("t.lm", source).unwrap_err();
    // The rendered text starts with `error[CODE]:`.
    rendered[6..11].to_string()
}

fn runs(source: &str) -> String {
    run_text("t.lm", source, VmConfig::default()).unwrap()
}

#[test]
fn generic_arity_and_application_rules() {
    // A generic function needs matching explicit arguments.
    assert_eq!(
        code_of("def id[T](x: T): T\n  x\nend\nid[Int, Int](1)\n"),
        "E1024"
    );
    // A generic class type needs its arguments.
    assert_eq!(
        code_of(
            "class Box[T]\n  v: T\n  def init(mut self, v: T)\n    self.v = v\n  end\nend\n\
                 b: Box = Box(1)\nb\n"
        ),
        "E1024"
    );
    assert_eq!(
        code_of(
            "class Box[T]\n  v: T\n  def init(mut self, v: T)\n    self.v = v\n  end\nend\n\
                 b: Box[Int, Int] = Box(1)\nb\n"
        ),
        "E1024"
    );
    // A non-generic class takes no arguments.
    assert_eq!(
        code_of("class Plain\n  x: Int = 0\nend\np: Plain[Int] = Plain()\np\n"),
        "E1024"
    );
    // Generic classes stay outside inheritance in this slice.
    assert_eq!(
        code_of("class Base\nend\nclass Sub[T] < Base\nend\n1\n"),
        "E1024"
    );
    assert_eq!(
        code_of(
            "class Base[T]\n  v: T\n  def init(mut self, v: T)\n    self.v = v\n  end\nend\n\
                 class Sub < Base\nend\n1\n"
        ),
        "E1024"
    );
    // Classes cannot declare effect parameters.
    assert_eq!(code_of("class C[effect e]\nend\n1\n"), "E1024");
}

#[test]
fn generic_inference_and_ambiguity() {
    // Inference from arguments.
    assert_eq!(
        runs("def id[T](x: T): T\n  x\nend\nid(41) + 1\n"),
        "Done(42)"
    );
    // Inference from the expected result.
    assert_eq!(
        runs("def make[T](): [T]\n  []\nend\nxs: [Int] = make()\nxs.len()\n"),
        "Done(0)"
    );
    // Explicit arguments settle an ambiguous call.
    assert_eq!(
        runs("def make[T](): [T]\n  []\nend\nmake[String]().len()\n"),
        "Done(0)"
    );
    // No unique solution without them.
    assert_eq!(
        code_of("def make[T](): [T]\n  []\nend\nx = make()\nx\n"),
        "E1045"
    );
    // Conflicting argument types never invent a type.
    assert_eq!(
        code_of("def pair[T](a: T, b: T): T\n  a\nend\npair(1, \"x\")\n"),
        "E1004"
    );
    // The first argument fixes the parameter; a later mismatch is an
    // error, not a silent widening. Explicit arguments select a
    // common supertype.
    assert_eq!(
        code_of(
            "class Animal\nend\nclass Dog < Animal\nend\nclass Cat < Animal\nend\n\
                 def pick[T](a: T, b: T): T\n  a\nend\n\
                 pick(Dog(), Cat()) is Dog\n"
        ),
        "E1004"
    );
    assert_eq!(
        runs(
            "class Animal\nend\nclass Dog < Animal\nend\nclass Cat < Animal\nend\n\
              def pick[T](a: T, b: T): T\n  a\nend\n\
              pick[Animal](Dog(), Cat()) is Dog\n"
        ),
        "Done(true)"
    );
    // A bare generic constructor without context is ambiguous.
    assert_eq!(code_of("x = None\nx\n"), "E1045");
    // The expected type selects the arm.
    assert_eq!(runs("x: Option[Int] = None\nx.is_none()\n"), "Done(true)");
}

#[test]
fn generic_bodies_are_shared_not_monomorphized() {
    // One `id` body serves two instantiations; the bytecode holds one
    // function for it.
    let module =
        compile_text("t.lm", "def id[T](x: T): T\n  x\nend\n(id(1), id(\"a\"))\n").unwrap();
    let count = module.funcs.iter().filter(|f| f.name == "id").count();
    assert_eq!(count, 1);
    // Two call sites share the module through type applications.
    assert!(module.apps.len() >= 2, "expected type applications");
}

#[test]
fn tuple_indexing_rules() {
    assert_eq!(runs("t = (5, \"a\")\nt[0]\n"), "Done(5)");
    assert_eq!(runs("t = (\"only\",)\nt[0]\n"), "Done(\"only\")");
    // Only compile-time literal indexing.
    assert_eq!(code_of("t = (1, 2)\ni = 0\nt[i]\n"), "E1048");
    // Out-of-range literal index.
    assert_eq!(code_of("t = (1, 2)\nt[2]\n"), "E1048");
    assert_eq!(code_of("t = (1, 2)\nt[-1]\n"), "E1048");
    // Tuple equality is structural (specification 6.4).
    assert_eq!(runs("(1,) == (1,)\n"), "Done(true)");
    // Tuples are covariant; lists are not.
    assert_eq!(
        runs(
            "class Animal\nend\nclass Dog < Animal\nend\n\
              def f(t: (Animal, Int)): Int\n  t[1]\nend\nf((Dog(), 3))\n"
        ),
        "Done(3)"
    );
}

#[test]
fn enum_declaration_rules() {
    // Duplicate arm.
    assert_eq!(code_of("enum E\n  A\n  A\nend\n1\n"), "E1040");
    // Arms after methods.
    assert_eq!(
        code_of("enum E\n  A\n  def f(self): Int\n    1\n  end\n  B\nend\n1\n"),
        "E1040"
    );
    // An enum cannot be constructed.
    assert_eq!(code_of("enum E\n  A\nend\nx = E()\nx\n"), "E1040");
    // An enum cannot be a parent.
    assert_eq!(code_of("enum E\n  A\nend\nclass C < E\nend\n1\n"), "E1040");
    // An enum has no `init`.
    assert_eq!(
        code_of("enum E\n  A\n  def init(mut self)\n  end\nend\n1\n"),
        "E1040"
    );
    // Enum names live beside class names.
    assert_eq!(code_of("enum E\n  A\nend\nclass E\nend\n1\n"), "E1010");
}

#[test]
fn pattern_rules() {
    // An unknown bare name is a binding, so the case is exhaustive.
    assert_eq!(
        runs(
            "enum E\n  A\nend\ndef f(e: E): Int\n  case e\n  in other then 1\n  \
              end\nend\nf(A)\n"
        ),
        "Done(1)"
    );
    // A parenthesized unknown constructor is an error.
    assert_eq!(
        code_of(
            "enum E\n  A\nend\ndef f(e: E): Int\n  case e\n  in B(x) then 1\n  \
                 end\nend\n1\n"
        ),
        "E1041"
    );
    // Arity mismatch in a constructor pattern.
    assert_eq!(
        code_of(
            "enum E\n  A(x: Int)\nend\ndef f(e: E): Int\n  case e\n  in A then 1\n  \
                 end\nend\n1\n"
        ),
        "E1041"
    );
    assert_eq!(
        code_of(
            "enum E\n  A(x: Int)\nend\ndef f(e: E): Int\n  case e\n  in A(a, b) then 1\n  \
                 end\nend\n1\n"
        ),
        "E1041"
    );
    // Duplicate binding names in one pattern.
    assert_eq!(
        code_of(
            "enum E\n  A(x: Int, y: Int)\nend\ndef f(e: E): Int\n  case e\n  \
                 in A(v, v) then v\n  end\nend\n1\n"
        ),
        "E1041"
    );
    // A literal pattern must match the scrutinee type.
    assert_eq!(
        code_of("case 1\nin true then 1\nin _ then 2\nend\n"),
        "E1041"
    );
    // Week 4: class constructor patterns destructure the scrutinee
    // class in declaration order.
    assert_eq!(
        runs("class C\n  x: Int = 7\nend\ncase C()\nin C(x) then x\nend\n"),
        "Done(7)"
    );
    // The pattern must name the scrutinee class with full arity.
    assert_eq!(
        code_of("class C\n  x: Int = 0\nend\nclass D\nend\ncase C()\nin D() then 1\nend\n"),
        "E1041"
    );
    assert_eq!(
        code_of("class C\n  x: Int = 0\nend\ncase C()\nin C() then 1\nend\n"),
        "E1041"
    );
    // The qualifier must name the scrutinee enum.
    assert_eq!(
        code_of(
            "enum E\n  A\nend\nenum F\n  B\nend\ndef f(e: E): Int\n  case e\n  \
                 in F.B then 1\n  in _ then 2\n  end\nend\n1\n"
        ),
        "E1041"
    );
}

#[test]
fn exhaustiveness_rules() {
    // Bool cases must cover both literals or use a wildcard.
    assert_eq!(code_of("case true\nin true then 1\nend\n"), "E1042");
    assert_eq!(
        runs("case false\nin true then 1\nin false then 2\nend\n"),
        "Done(2)"
    );
    // Int and String cases always need an irrefutable arm.
    assert_eq!(code_of("case 3\nin 1 then 1\nin 2 then 2\nend\n"), "E1042");
    assert_eq!(code_of("case \"x\"\nin \"a\" then 1\nend\n"), "E1042");
    // Nested exhaustiveness over a generic family.
    assert_eq!(
        runs(
            "def f(o: Option[Option[Int]]): Int\n  case o\n  in Some(Some(n)) then n\n  \
              in Some(None) then -1\n  in None then -2\n  end\nend\nf(Some(None))\n"
        ),
        "Done(-1)"
    );
    assert_eq!(
        code_of(
            "def f(o: Option[Option[Int]]): Int\n  case o\n  in Some(Some(n)) then n\n  \
                 in None then -2\n  end\nend\n1\n"
        ),
        "E1042"
    );
    // Unreachable arms are compile errors, also through nesting.
    assert_eq!(
        code_of(
            "def f(o: Option[Int]): Int\n  case o\n  in Some(_) then 1\n  \
                 in Some(3) then 2\n  in None then 0\n  end\nend\n1\n"
        ),
        "E1043"
    );
    assert_eq!(code_of("case 1\nin _ then 1\nin 2 then 2\nend\n"), "E1043");
    // A constructor builds a value of the enum and not of the one arm
    // it names, so every arm stays reachable and a case must cover all
    // of them. This holds for any enum, not for the core ones alone.
    let colour = "enum Colour\n  Red\n  Green\n  Blue(shade: Int)\nend\n";
    assert_eq!(
        code_of(&format!("{colour}c = Red\ncase c\nin Red then 1\nend\n")),
        "E1042"
    );
    assert_eq!(
        runs(&format!(
            "{colour}c = Red\ncase c\nin Red then 1\nin Green then 2\nin Blue(s) then s\nend\n"
        )),
        "Done(1)"
    );
    // A wildcard covers the arms a program does not name.
    assert_eq!(
        runs(&format!(
            "{colour}c = Red\ncase c\nin Red then 1\nin _ then 0\nend\n"
        )),
        "Done(1)"
    );
    assert_eq!(
        code_of("x = Some(3)\ncase x\nin Some(v) then v\nend\n"),
        "E1042"
    );
    assert_eq!(
        runs("x = Some(3)\ncase x\nin Some(v) then v\nin None then 0\nend\n"),
        "Done(3)"
    );
    // Other scrutinee types need an irrefutable arm.
    assert_eq!(runs("case (1, 2)\nin t then t[0] + t[1]\nend\n"), "Done(3)");
    assert_eq!(code_of("xs = [1]\ncase xs\nin 1 then 1\nend\n"), "E1041");
}

#[test]
fn refinement_and_cast_rules() {
    let animals = "class Animal\nend\nclass Dog < Animal\n  def bark(self): Int\n    1\n  \
                   end\nend\n";
    // Refinement works in the true branch only.
    assert_eq!(
        runs(&format!(
            "{animals}def f(a: Animal): Int\n  if a is Dog\n    a.bark()\n  else\n    0\n  \
             end\nend\nf(Dog()) + f(Animal())\n"
        )),
        "Done(1)"
    );
    // The refinement does not survive the branch.
    assert_eq!(
        code_of(&format!(
            "{animals}def f(a: Animal): Int\n  if a is Dog\n    a.bark()\n  else\n    0\n  \
             end\n  a.bark()\nend\nf(Dog())\n"
        )),
        "E1026"
    );
    // Inside the branch the name holds the narrowed type, so a wider
    // assignment is a type error rather than a silent widening.
    assert_eq!(
        code_of(&format!(
            "{animals}def f(a: Animal): Int\n  if a is Dog\n    a = Animal()\n    a.bark()\n  \
             else\n    0\n  end\nend\nf(Dog())\n"
        )),
        "E1004"
    );
    // `is` and `as` reject unrelated or non-nominal operands.
    assert_eq!(
        code_of(&format!("{animals}class Cat < Animal\nend\nDog() is Cat\n")),
        "E1047"
    );
    assert_eq!(code_of("1 is Bool\n"), "E1047");
    assert_eq!(code_of("xs = [1]\nxs as [Int]\n"), "E1047");
    // Upcasts always succeed; downcasts test at run time.
    assert_eq!(
        runs(&format!("{animals}d = Dog()\na = d as Animal\na is Dog\n")),
        "Done(true)"
    );
    assert_eq!(
        runs(&format!("{animals}a: Animal = Animal()\na as Dog\n0\n")),
        "Fault(BadCast)"
    );
    // Enum values refine through `case`, not `is`; an arm name is not
    // a type expression. `is` against the family is trivially true.
    assert_eq!(
        runs("o: Option[Int] = Some(2)\nif o is Option[Int]\n  1\nelse\n  0\nend\n"),
        "Done(1)"
    );
}

#[test]
fn row_rules() {
    // A callee row must sit inside the caller row.
    assert_eq!(
        code_of("def go() with Io.Print\nend\ndef pure()\n  go()\nend\n1\n"),
        "E1046"
    );
    // Group inclusion covers exact operations.
    assert_eq!(
        runs("def go() with Io.Print\nend\ndef wide() with Io\n  go()\nend\n1\n"),
        "Done(1)"
    );
    // A group is not inside one exact operation.
    assert_eq!(
        code_of("def go() with Io\nend\ndef narrow() with Io.Print\n  go()\nend\n1\n"),
        "E1046"
    );
    // Week 4: the entry collects its row instead of rejecting; the
    // policy table decides at run time. An unmocked, unpassed
    // operation faults `PolicyDenied` when performed; here nothing
    // performs, so the program completes.
    assert_eq!(runs("def go() with Io.Print\nend\ngo()\n"), "Done(())");
    // Closures declare rows; calling charges the closure row into
    // the collected entry row.
    assert_eq!(runs("f = do || with Io.Print 1 end\nf()\n"), "Done(1)");
    assert_eq!(
        runs(
            "def hold(f: () -> Int with Io.Print): Int\n  1\nend\n\
              hold(do || with Io.Print 2 end)\n"
        ),
        "Done(1)"
    );
    // A pure closure fits an effectful expectation, not the reverse.
    assert_eq!(
        runs("def hold(f: () -> Int with Io.Print): Int\n  1\nend\nhold(do || 2 end)\n"),
        "Done(1)"
    );
    assert_eq!(
        code_of(
            "def hold(f: () -> Int): Int\n  f()\nend\n\
                 hold(do || with Io.Print 2 end)\n"
        ),
        "E1004"
    );
    // Effect variables flow through higher-order calls.
    assert_eq!(
        runs(
            "def apply[T, effect e](x: T, f: (T) -> T with e): T with e\n  f(x)\nend\n\
              apply(20, do |n: Int|: Int n * 2 end) + 2\n"
        ),
        "Done(42)"
    );
    // The declared row must include the effect variable.
    assert_eq!(
        code_of("def bad[T, effect e](x: T, f: (T) -> T with e): T\n  f(x)\nend\n1\n"),
        "E1046"
    );
    // An unknown effect name is rejected.
    assert_eq!(code_of("def f() with e\nend\n1\n"), "E1046");
    // `init` rows charge construction. A pure function cannot
    // construct the class; the entry collects the charge.
    assert_eq!(
        code_of(
            "class C\n  x: Int\n  def init(mut self) with Io.Print\n    self.x = 1\n  \
                 end\nend\ndef make(): Int\n  C().x\nend\nmake()\n"
        ),
        "E1046"
    );
    assert_eq!(
        runs(
            "class C\n  x: Int\n  def init(mut self) with Io.Print\n    self.x = 1\n  \
                 end\nend\nc = C()\nc.x\n"
        ),
        "Done(1)"
    );
    // Overrides may narrow but not widen; the widening case is the UI
    // test `row-widening-override.lm`. Narrowing compiles.
    assert!(compile_text(
        "t.lm",
        "class A\n  def f(self): Int with Io\n    1\n  end\nend\n\
         class B < A\n  def f(self): Int with Io.Print\n    2\n  end\nend\n1\n",
    )
    .is_ok());
    // Calling the effectful method from a pure function is rejected;
    // the entry collects the charge instead.
    assert_eq!(
        code_of(
            "class A\n  def f(self): Int with Io.Print\n    1\n  end\nend\n\
             def pure(): Int\n  A().f()\nend\npure()\n"
        ),
        "E1046"
    );
}

#[test]
fn qualified_constructors_and_shadowing() {
    // A qualified constructor builds a value of the enum, so an
    // annotation adds nothing and every arm stays reachable.
    assert_eq!(
        runs(
            "x: Option[Int] = Option.Some(3)\n\
              case x\nin Some(v) then v\nin None then 0\nend\n"
        ),
        "Done(3)"
    );
    assert_eq!(
        runs("x = Option.Some(3)\ncase x\nin Some(v) then v\nin None then 0\nend\n"),
        "Done(3)"
    );
    assert_eq!(runs("Ordering.Equal.is_equal()\n"), "Done(true)");
    // An unknown qualified arm is an error.
    assert_eq!(code_of("Ordering.Middle\n"), "E1041");
    // A local shadows a constructor name in expressions.
    assert_eq!(
        runs("enum E\n  Value(v: Int)\nend\nValue = 3\nValue\n"),
        "Done(3)"
    );
}

#[test]
fn get_methods_return_core_option() {
    assert_eq!(runs("xs = [1, 2]\nxs.get(1).value_or(0)\n"), "Done(2)");
    assert_eq!(runs("xs = [1, 2]\nxs.get(5).is_none()\n"), "Done(true)");
    assert_eq!(runs("xs = [1, 2]\nxs.get(-1).is_none()\n"), "Done(true)");
    assert_eq!(
        runs("m = {\"a\": 2}\nm.get(\"a\").value_or(0) + m.get(\"b\").value_or(10)\n"),
        "Done(12)"
    );
    // The faulting access is unchanged.
    assert_eq!(runs("xs = [1]\nxs.at(4)\n"), "Fault(IndexOutOfBounds)");
    assert_eq!(runs("m = {\"a\": 1}\nm.at(\"b\")\n"), "Fault(MissingKey)");
    // Case display uses constructor form.
    assert_eq!(runs("xs = [7]\nxs.get(0)\n"), "Done(Some(7))");
    assert_eq!(runs("xs = [7]\nxs.get(3)\n"), "Done(None)");
}

#[test]
fn mutual_recursion_checks_against_declared_signatures() {
    // Mutually recursive functions with rows.
    assert_eq!(
        runs(
            "def even(n: Int): Bool\n  if n == 0\n    true\n  else\n    odd(n - 1)\n  \
              end\nend\ndef odd(n: Int): Bool\n  if n == 0\n    false\n  else\n    \
              even(n - 1)\n  end\nend\neven(10)\n"
        ),
        "Done(true)"
    );
    // Mutually recursive enums.
    assert_eq!(
        runs(
            "enum AList\n  ANil\n  ACons(v: Int, next: BList)\nend\n\
              enum BList\n  BNil\n  BCons(v: Int, next: AList)\nend\n\
              def suma(l: AList): Int\n  case l\n  in ANil then 0\n  \
              in ACons(v, next) then v + sumb(next)\n  end\nend\n\
              def sumb(l: BList): Int\n  case l\n  in BNil then 0\n  \
              in BCons(v, next) then v + suma(next)\n  end\nend\n\
              suma(ACons(1, BCons(2, ANil)))\n"
        ),
        "Done(3)"
    );
    // A class whose field names its own enum family.
    assert_eq!(
        runs(
            "enum Tree\n  Leaf\n  Node(l: Tree, r: Tree)\nend\n\
              class Holder\n  t: Tree = Leaf\nend\n\
              h = Holder()\ncase h.t\nin Leaf then 1\nin Node(_, _) then 2\nend\n"
        ),
        "Done(1)"
    );
}

#[test]
fn depth_guards_cover_new_recursive_productions() {
    let run = |source: String| {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let rendered = compile_text("deep.lm", &source).unwrap_err();
                assert_eq!(&rendered[6..11], "E1022");
            })
            .unwrap()
            .join()
            .unwrap();
    };
    // Generic argument lists.
    let deep_generic = format!(
        "x: {}Int{} = 1\nx\n",
        "Box[".repeat(5_000),
        "]".repeat(5_000)
    );
    run(deep_generic);
    // Tuple types.
    let deep_tuple_ty = format!("x: {}Int,{} = 1\nx\n", "(".repeat(5_000), ")".repeat(5_000));
    run(deep_tuple_ty);
    // Tuple literals.
    let deep_tuple = format!("{}1,{}\n", "(".repeat(5_000), ")".repeat(5_000));
    run(deep_tuple);
    // Nested patterns.
    let deep_pattern = format!(
        "case None\nin {}None{} then 1\nin _ then 0\nend\n",
        "Some(".repeat(5_000),
        ")".repeat(5_000)
    );
    run(deep_pattern);
    // Nested case expressions inside an enum method body.
    let deep_case = format!(
        "enum E\n  A\n  def f(self): Int\n    {}1{}\n  end\nend\n1\n",
        "case 1 in _ then ".repeat(3_000),
        " end".repeat(3_000)
    );
    run(deep_case);
}

#[test]
fn moderate_new_nesting_compiles_and_runs() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            // A nested generic application inside the guard.
            let depth = 40;
            let ty = format!("{}Int{}", "Box[".repeat(depth), "]".repeat(depth));
            let mut build = "Box(1)".to_string();
            for _ in 1..depth {
                build = format!("Box({build})");
            }
            let source = format!(
                "class Box[T]\n  v: T\n  def init(mut self, v: T)\n    self.v = v\n  \
                 end\nend\nx: {ty} = {build}\n0\n"
            );
            assert_eq!(
                run_text("deep.lm", &source, VmConfig::default()).unwrap(),
                "Done(0)"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn tuple_equality_is_structural() {
    // Specification 6.4: equal static tuple types, element pairs
    // compared under the rules for their declared element types.
    assert_eq!(runs("(1, \"x\", true) == (1, \"x\", true)\n"), "Done(true)");
    assert_eq!(runs("(1, \"x\") == (2, \"x\")\n"), "Done(false)");
    assert_eq!(runs("(1, \"x\") != (2, \"x\")\n"), "Done(true)");
    // Nested tuples recurse.
    assert_eq!(runs("((1, 2), 3) == ((1, 2), 3)\n"), "Done(true)");
    assert_eq!(runs("((1, 2), 3) == ((1, 9), 3)\n"), "Done(false)");
    // A heap element compares by reference identity.
    assert_eq!(runs("xs = [1]\n(xs, 1) == (xs, 1)\n"), "Done(true)");
    assert_eq!(runs("xs = [1]\n(xs, 1) == ([1], 1)\n"), "Done(false)");
    // Unit elements are always equal.
    assert_eq!(runs("def u()\nend\n(u(), 1) == (u(), 1)\n"), "Done(true)");
}

#[test]
fn tuple_equality_static_rules() {
    // The sides need equal static tuple types.
    assert_eq!(code_of("(1, 2) == (1, \"x\")\n"), "E1004");
    // A type-variable element has no equality rule in a shared body.
    assert_eq!(
        code_of("def f[T](x: (T, Int)): Bool\n  x == x\nend\nf((1, 2))\n"),
        "E1017"
    );
}

#[test]
fn field_default_with_case_temporaries_runs() {
    // Review regression: the shifted temporaries of a field default
    // must move `next_scratch` past their new slots.
    assert_eq!(
        runs(
            "class C\n  a: Int = case 41 in x then x + 1 end\n  \
              b: Int = case (1, 2) in p then p[0] + p[1] end\nend\nc = C()\nc.a + c.b\n"
        ),
        "Done(45)"
    );
    assert_eq!(
        runs("class D\n  a: Int = case 6 in _ then 7 end\nend\nD().a\n"),
        "Done(7)"
    );
}
