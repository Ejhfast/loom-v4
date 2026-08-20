//! Week-10 native collection and iteration behavior.

use lm_bytecode::{BcType, ExtendedInstr, Instr};
use lm_compiler::{compile_module, link, CompileEnv, LinkEnv, LinkUnit};
use lm_heap::{Object, StructuralEpoch};
use lm_source::SourceFile;
use lm_testkit::{compile_text, run_allowed};
use lm_vm::snapshot::{codec, ImageReason, LoadLimits};
use lm_vm::{RecordingHost, RootEvent, Vm, VmConfig, World};

fn run(source: &str) -> (String, lm_vm::HeapStats) {
    let module = compile_text("collections.lm", source).expect("the source compiles");
    let loaded = lm_vm::load(module).expect("the module loads");
    let mut vm = Vm::new(&loaded, VmConfig::default());
    let outcome = vm.run();
    (vm.show_outcome(&outcome), vm.heap().stats())
}

fn outcome(source: &str) -> String {
    run(source).0
}

fn error(source: &str) -> String {
    compile_text("collections.lm", source).expect_err("the source must fail")
}

#[test]
fn typed_option_values_allocate_no_guest_object() {
    let (some, some_heap) = run("Some(7)\n");
    assert_eq!(some, "Done(Some(7))");
    assert_eq!(some_heap.slots, 0);

    let (none, none_heap) = run("x: Option[Int] = None\nx\n");
    assert_eq!(none, "Done(None)");
    assert_eq!(none_heap.slots, 0);
}

#[test]
fn collection_get_uses_one_native_lookup() {
    let module = compile_text(
        "collections.lm",
        "xs = [1]\nm = {1: 2}\n(xs.get(0), m.get(1))\n",
    )
    .expect("the source compiles");
    let mut list_get = 0;
    let mut map_get = 0;
    let entry = &module.funcs[module.entry as usize];
    for instruction in entry.blocks.iter().flatten() {
        list_get += usize::from(matches!(
            instruction,
            Instr::Extended(ExtendedInstr::ListGet { .. })
        ));
        map_get += usize::from(matches!(
            instruction,
            Instr::Extended(ExtendedInstr::MapGet { .. })
        ));
    }
    assert_eq!(list_get, 1);
    assert_eq!(map_get, 1);
    assert!(entry
        .blocks
        .iter()
        .flatten()
        .all(|instruction| !matches!(instruction, Instr::Call(_) | Instr::CallG { .. })));
}

#[test]
fn an_unused_map_put_discards_its_option_result() {
    let module = compile_text(
        "collections.lm",
        "table: Map[Int, Int] = {}\ntable.put(1, 2)\ntable.put(1, 3)\n",
    )
    .expect("the source compiles");
    let puts: Vec<bool> = module.funcs[module.entry as usize]
        .blocks
        .iter()
        .flatten()
        .filter_map(|instruction| match instruction {
            Instr::MapPut { discard, .. } => Some(*discard),
            _ => None,
        })
        .collect();
    assert_eq!(puts, vec![true, false]);
}

#[test]
fn interface_contracts_cross_module_boundaries() {
    let library = compile_module(
        "lib.metrics",
        &SourceFile::new(
            "metrics.lm",
            "interface Sized\n  def len(self): Int\nend\n\
             final class Counter implements Sized\n  value: Int = 9\n\
               def len(self): Int\n    self.value\n  end\nend\n"
                .to_string(),
        ),
        &CompileEnv::new().freeze(),
        false,
    )
    .expect("the library compiles");
    let mut compile_env = CompileEnv::new();
    compile_env
        .bind_interface(library.interface.clone())
        .expect("the interface binds");
    compile_env
        .bind_root("metrics", "lib.metrics")
        .expect("the root binds");
    let main = compile_module(
        "app.main",
        &SourceFile::new(
            "main.lm",
            "use metrics\n\
             def size[T: metrics.Sized](value: T): Int\n  value.len()\nend\n\
             size(metrics.Counter())\n"
                .to_string(),
        ),
        &compile_env.freeze(),
        true,
    )
    .expect("the program compiles");
    let mut link_env = LinkEnv::new();
    for module in [&library, &main] {
        link_env
            .bind(LinkUnit {
                path: module.path.clone(),
                module: module.module.clone(),
                interface: module.interface.clone(),
            })
            .expect("the module binds");
    }
    let linked = link("app.main", &link_env.freeze()).expect("the program links");
    let loaded = lm_vm::load(linked.module).expect("the program loads");
    let mut vm = Vm::new(&loaded, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(9)");
}

#[test]
fn interface_bounds_infer_nonempty_effect_rows() {
    let source = r#"
interface Source[effect e]
  def next(mut self): Int with e
end

final class PureCounter implements Source[effect ()]
  def next(mut self): Int
    1
  end
end

final class LoudCounter implements Source[effect (Io.Print)]
  def next(mut self): Int with Io.Print
    sys.io.print("tick")
    2
  end
end

def drain[S: Source[effect (e)], effect e](mut source: S): Int with e
  source.next()
end

drain(PureCounter()) + drain(LoudCounter())
"#;
    assert_eq!(
        run_allowed("collections.lm", source, &["Io.Print"]).expect("the program runs"),
        "Done(3)"
    );
}

#[test]
fn native_for_covers_list_map_text_and_range() {
    let source = r#"
class Source
  calls: Int = 0

  def values(mut self): List[Int]
    self.calls = self.calls + 1
    [1, 2, 3]
  end
end

source = Source()
list_total = 0
for value in source.values()
  list_total = list_total + value
end

map_total = 0
for key, value in {2: 20, 1: 10}
  map_total = map_total + key + value
end

text_total = 0
for char in "aé"
  text_total = text_total + char.codepoint()
end

range_total = 0
for value in Range(2, 5)
  range_total = range_total + value
end

(list_total, map_total, text_total, range_total, source.calls)
"#;
    assert_eq!(outcome(source), "Done((6, 33, 330, 9, 1))");

    let module = compile_text("collections.lm", source).expect("the source compiles");
    let entry = &module.funcs[module.entry as usize];
    let instructions: Vec<&Instr> = entry.blocks.iter().flatten().collect();
    assert!(instructions
        .iter()
        .any(|item| matches!(item, Instr::Extended(ExtendedInstr::ListEpoch))));
    assert!(instructions
        .iter()
        .any(|item| matches!(item, Instr::Extended(ExtendedInstr::MapEpoch))));
    assert!(instructions
        .iter()
        .any(|item| { matches!(item, Instr::Native(lm_bytecode::NativeInstr::TextAtByte)) }));
    assert!(!instructions
        .iter()
        .any(|item| matches!(item, Instr::CallInterface { .. })));
}

#[test]
fn generic_for_uses_nominal_iterator_contracts() {
    let source = r#"
final class IntIterator implements Iterator
  type Item = Int

  values: List[Int]
  index: Int

  def init(mut self, values: List[Int])
    self.values = values
    self.index = 0
  end

  def next(mut self): Option[Int]
    if self.index < self.values.len()
      value = self.values.at(self.index)
      self.index = self.index + 1
      Some(value)
    else
      None
    end
  end
end

final class IntBag implements Iterable
  type Item = Int
  type Iter = IntIterator

  values: List[Int]

  def init(mut self, values: List[Int])
    self.values = values
  end

  def iterator(self): IntIterator
    IntIterator(self.values)
  end
end

def count[T: Iterable](values: T): Int
  total = 0
  for value in values
    total = total + 1
  end
  total
end

count(IntBag([4, 5, 6]))
"#;
    assert_eq!(outcome(source), "Done(3)");

    let module = compile_text("collections.lm", source).expect("the source compiles");
    let count = module
        .funcs
        .iter()
        .find(|function| function.name == "count")
        .expect("the count function exists");
    assert_eq!(
        count
            .blocks
            .iter()
            .flatten()
            .filter(|item| { matches!(item, Instr::CallInterface { .. }) })
            .count(),
        2
    );
}

#[test]
fn iterable_item_must_match_iterator_item() {
    let source = r#"
final class BadIterator implements Iterator
  type Item = String

  def next(mut self): Option[String]
    None
  end
end

final class BadIterable implements Iterable
  type Item = Int
  type Iter = BadIterator

  def iterator(self): BadIterator
    BadIterator()
  end
end

BadIterable()
"#;
    assert!(error(source).contains("Iterable.Item must equal Iterable.Iter.Item"));
}

#[test]
fn for_keeps_loop_control_semantics() {
    let source = r#"
def until_four(values: List[Int]): Int
  total = 0
  for value in values
    if value == 2
      continue
    end
    if value == 4
      break
    end
    total = total + value
  end
  total
end

def before_five(values: List[Int]): Int
  total = 0
  for value in values
    if value == 2
      continue
    end
    if value == 5
      return total
    end
    total = total + value
  end
  0 - 1
end

values = [1, 2, 3, 4, 5]
(until_four(values), before_five(values))
"#;
    assert_eq!(outcome(source), "Done((4, 8))");
}

#[test]
fn structural_mutation_invalidates_traversal() {
    assert!(error("xs = [1, 2]\nfor value in xs\n  xs.push(value)\nend\n0\n").contains("E1065"));
    assert!(error("m = {1: 10}\nfor key, value in m\n  m.put(2, 20)\nend\n0\n").contains("E1065"));
    assert_eq!(
        outcome("xs = [1, 2]\nalias = xs\nfor value in xs\n  alias.push(value)\nend\n0\n"),
        "Fault(CollectionModified)"
    );
    assert_eq!(
        outcome("xs = [1, 2]\nit = xs.iterator()\nit.next()\nxs.push(3)\nit.next()\n"),
        "Fault(CollectionModified)"
    );
    assert_eq!(
        outcome("m = {1: 10}\nalias = m\nfor key, value in m\n  alias.put(2, 20)\nend\n0\n"),
        "Fault(CollectionModified)"
    );
    assert_eq!(
        outcome("xs = [1, 2]\nview = xs.slice_view(0, 2)\nxs.reverse()\nview.len()\n"),
        "Fault(CollectionModified)"
    );
    let sorted = r#"
xs = [2, 1]
view = xs.slice_view(0, 2)
xs.sort_by() { |left: Int, right: Int|
  if left < right
    Ordering.Less
  elsif left == right
    Ordering.Equal
  else
    Ordering.Greater
  end
}
view.len()
"#;
    assert_eq!(outcome(sorted), "Fault(CollectionModified)");
}

#[test]
fn for_rejects_direct_mutation_paths() {
    let through_parameter = r#"
def replace(mut values: List[Int])
  values.set(0, 9)
end

values = [1, 2]
for value in values
  replace(values)
end
"#;
    assert!(error(through_parameter).contains("E1065"));

    let through_field = r#"
final class Holder
  values: List[Int]

  def init(mut self, values: List[Int])
    self.values = values
  end

  def extend(mut self)
    for value in self.values
      self.values.push(value)
    end
  end
end

Holder([1, 2]).extend()
"#;
    assert!(error(through_field).contains("E1065"));
}

#[test]
fn value_replacement_remains_valid_during_traversal() {
    let source = r#"
xs = [1, 2, 3]
xs_mut = xs
for value in xs
  xs_mut.set(0, xs.at(0) + 1)
end

table = {1: 10, 2: 20}
table_mut = table
for key, value in table
  table_mut.put(key, value + 1)
end

(xs, table)
"#;
    assert_eq!(outcome(source), "Done(([4, 2, 3], {1: 11, 2: 21}))");
}

#[test]
fn manual_iterators_return_native_options() {
    let source = r#"
list = [2]
list_iterator = list.iterator()
table = {3: 4}
map_iterator = table.iterator()
text_iterator = "é".iterator()
range_iterator = Range(5, 6).iterator()
(
  list_iterator.next(),
  list_iterator.next(),
  map_iterator.next(),
  map_iterator.next(),
  text_iterator.next(),
  text_iterator.next(),
  range_iterator.next(),
  range_iterator.next()
)
"#;
    assert_eq!(
        outcome(source),
        "Done((Some(2), None, Some((3, 4)), None, Some('é'), None, Some(5), None))"
    );
}

#[test]
fn list_operations_cover_mutation_and_copying() {
    let source = r#"
values = list_with_capacity[Int](8)
has_capacity = values.capacity() >= 8
values.extend([1, 2, 3])
values.set(1, 20)
popped = values.pop()
values.insert(1, 2)
removed = values.remove(2)
values.push(3)
values.push(4)
swapped = values.swap_remove(1)
copied = values.copy()
sliced = values.slice(1, 2)
joined = values.concat([9])
values.reverse()
values.truncate(2)
empty = [8]
empty.clear()
repeated = list_repeated[Int](7, 3)
(
  has_capacity,
  popped,
  removed,
  swapped,
  copied,
  sliced,
  joined,
  values,
  values.first(),
  values.last(),
  values.contains(4),
  empty.is_empty(),
  repeated
)
"#;
    assert_eq!(
        outcome(source),
        "Done((true, Some(3), 20, 2, [1, 4, 3], [4, 3], [1, 4, 3, 9], [3, 4], Some(3), Some(4), true, true, [7, 7, 7]))"
    );
}

#[test]
fn list_higher_order_operations_use_callbacks() {
    let source = r#"
class Total
  value: Int = 0

  def add(mut self, value: Int)
    self.value = self.value + value
  end
end

values = [1, 2, 3, 4, 5]
total = Total()
values.each() { |value: Int| total.add(value) }
mapped = values.map() { |value: Int| value * 2 }
filtered = values.filter() { |value: Int| value % 2 == 1 }
filtered_map = values.filter_map[Int]() { |value: Int|
  if value % 2 == 0
    Some(value * 10)
  else
    None
  end
}
folded = values.fold[Int](0) { |sum: Int, value: Int| sum + value }
sorted = [4, 1, 3, 2]
sorted.sort_by() { |left: Int, right: Int|
  if left < right
    Ordering.Less
  elsif left == right
    Ordering.Equal
  else
    Ordering.Greater
  end
}
(
  total.value,
  values.position() { |value: Int| value == 3 },
  values.find() { |value: Int| value > 3 },
  mapped,
  filtered,
  filtered_map,
  folded,
  values.any() { |value: Int| value == 5 },
  values.all() { |value: Int| value > 0 },
  sorted
)
"#;
    assert_eq!(
        outcome(source),
        "Done((15, Some(2), Some(4), [2, 4, 6, 8, 10], [1, 3, 5], [20, 40], 15, true, true, [1, 2, 3, 4]))"
    );
}

#[test]
fn map_operations_preserve_order_and_previous_values() {
    let source = r#"
class Factory
  calls: Int = 0

  def make(mut self): Int
    self.calls = self.calls + 1
    30
  end
end

table = map_with_capacity[String, Int](4)
first = table.put("a", 1)
second = table.put("a", 2)
table.put("b", 3)
factory = Factory()
inserted = table.get_or_insert_with("c") { || factory.make() }
existing = table.get_or_insert_with("c") { || factory.make() }
keys = table.keys_list()
values = table.values_list()
entries = table.entries_list()
mapped = table.map_values[Int]() { |key: String, value: Int| value * 2 }
table.retain() { |key: String, value: Int| value >= 3 }
removed = table.remove("c")
(
  first,
  second,
  inserted,
  existing,
  factory.calls,
  keys,
  values,
  entries,
  mapped,
  table,
  removed,
  table.has("a"),
  table.get("b"),
  table.at("b")
)
"#;
    assert_eq!(
        outcome(source),
        "Done((None, Some(1), 30, 30, 1, [\"a\", \"b\", \"c\"], [2, 3, 30], [(\"a\", 2), (\"b\", 3), (\"c\", 30)], {\"a\": 4, \"b\": 6, \"c\": 60}, {\"b\": 3}, Some(30), false, Some(3), 3))"
    );
}

#[test]
fn views_are_live_retaining_and_fail_fast() {
    let source = r#"
slice = [1, 2, 3].slice_view(1, 2)
table = {"a": 1, "b": 2}
keys = table.keys()
values = table.values()
entries = table.entries()
table.put("a", 9)
eager = table.values_list()
table.put("c", 3)
(
  slice.copy(),
  keys.at(0),
  values.at(0),
  entries.at(1),
  eager
)
"#;
    assert_eq!(outcome(source), "Fault(CollectionModified)");

    let live = r#"
slice = [1, 2, 3].slice_view(1, 2)
table = {"a": 1, "b": 2}
values = table.values()
table.put("a", 9)
eager = table.values_list()
table.put("a", 10)
(slice.copy(), values.at(0), eager)
"#;
    assert_eq!(outcome(live), "Done(([2, 3], 10, [9, 2]))");

    assert_eq!(
        outcome("xs = [1, 2]\nview = xs.slice_view(0, 1)\nxs.push(3)\nview.len()\n"),
        "Fault(CollectionModified)"
    );
    assert_eq!(
        outcome("xs = [1, 2]\nview = xs.slice_view(0, 1)\nview.freeze()\nxs.push(3)\n"),
        "Fault(FrozenWrite)"
    );
}

#[test]
fn callbacks_do_not_allocate_guest_closures() {
    let source = "values = [1, 2, 3]\nvalues.map() { |value: Int| value + 1 }\n";
    let module = compile_text("collections.lm", source).expect("the source compiles");
    let entry = &module.funcs[module.entry as usize];
    assert_eq!(
        entry
            .blocks
            .iter()
            .flatten()
            .filter(|item| { matches!(item, Instr::Extended(ExtendedInstr::MakeCallback { .. })) })
            .count(),
        1
    );
    assert!(!entry
        .blocks
        .iter()
        .flatten()
        .any(|item| matches!(item, Instr::MakeClosure { .. })));

    let loaded = lm_vm::load(module).expect("the module loads");
    let mut vm = Vm::new(&loaded, VmConfig::default());
    let result = vm.run();
    assert_eq!(vm.show_outcome(&result), "Done([2, 3, 4])");
    assert_eq!(vm.heap().stats().slots, 2);
}

#[test]
fn callbacks_forward_effects_and_default_to_nonescaping() {
    let source = r#"
def emit(values: List[Int]): Int with Io.Print
  values.each() { |value: Int| with Io.Print sys.io.print("{value}") }
  values.len()
end

emit([1, 2])
"#;
    assert_eq!(
        run_allowed("collections.lm", source, &["Io.Print"]).expect("the program runs"),
        "Done(2)"
    );

    let relay = r#"
def apply(f: (Int) -> Int): Int
  f(2)
end

def relay(f: (Int) -> Int): Int
  apply(f)
end

relay() { |value: Int| value + 3 }
"#;
    assert_eq!(outcome(relay), "Done(5)");

    for invalid in [
        "def leak(f: () -> Int): () -> Int\n  f\nend\nleak() { || 1 }\n",
        "def leak(f: () -> Int): Int\n  saved = f\n  saved()\nend\nleak() { || 1 }\n",
        "def leak(f: () -> Int): Int\n  pair = (f,)\n  pair[0]()\nend\nleak() { || 1 }\n",
        "def leak(f: () -> Int): Int\n  list = [f]\n  list.at(0)()\nend\nleak() { || 1 }\n",
        "def leak(f: () -> Int): Int\n  inner = do ||: Int f() end\n  inner()\nend\nleak() { || 1 }\n",
        "def id[T](value: T): T\n  value\nend\ndef leak(f: () -> Int): Int\n  id(f)()\nend\nleak() { || 1 }\n",
    ] {
        let failure = compile_text("collections.lm", invalid);
        assert!(failure.is_err(), "{invalid}");
        assert!(failure.unwrap_err().contains("E1064"), "{invalid}");
    }
}

#[test]
fn stored_functions_can_accept_nonescaping_callbacks() {
    let source = r#"
apply = do |value: Int, body: (Int) -> Int|: Int
  body(value)
end

apply(41) { |value: Int| value + 1 }
"#;
    assert_eq!(outcome(source), "Done(42)");
}

#[test]
fn escaping_allows_a_function_parameter_to_leave_its_call() {
    let source = r#"
def keep(escaping f: () -> Int): () -> Int
  f
end

saved = keep() { || 7 }
saved()
"#;
    assert_eq!(outcome(source), "Done(7)");
    let module = compile_text("collections.lm", source).expect("the source compiles");
    let entry = &module.funcs[module.entry as usize];
    assert!(entry
        .blocks
        .iter()
        .flatten()
        .any(|item| matches!(item, Instr::MakeClosure { .. })));
    assert!(!entry
        .blocks
        .iter()
        .flatten()
        .any(|item| { matches!(item, Instr::Extended(ExtendedInstr::MakeCallback { .. })) }));

    let invalid = "def bad(escaping value: Int): Int\n  value\nend\nbad(1)\n";
    let failure = error(invalid);
    assert!(failure.contains("E1064"));
    assert!(failure.contains("must have a function type"));
}

#[test]
fn active_callbacks_round_trip_through_snapshots() {
    let source = r#"
def go(): Int with Vm
  [1].each() { |value: Int| with Vm.SnapshotSelf
    case sys.vm.snapshot_self()
    in Ok(_) then ()
    in Err(_) then ()
    end
  }
  7
end

go()
"#;
    let module = compile_text("collections.lm", source).expect("the source compiles");
    let loaded = lm_vm::load(module).expect("the module loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Vm").expect("the grant exists");
    let result = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&result), "Done(7)");
    let image = world
        .last_snapshot()
        .expect("the callback creates an image");
    assert_eq!(image.world().machines[0].callbacks.len(), 1);

    let admitted = codec::load_external(image.bytes(), &loaded, LoadLimits::default())
        .expect("the callback image loads");
    let encoded = codec::encode(admitted.world(), usize::MAX).expect("the image encodes");
    assert_eq!(encoded.as_slice(), image.bytes().as_ref());

    let mut fresh = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let target = fresh.new_child(0).expect("the target exists");
    let root = fresh
        .restore_image(0, target, image)
        .expect("the callback image restores");
    let gate = fresh.next_gate();
    let restored = fresh
        .capture_snapshot(gate, root, false)
        .expect("the restored callback captures");
    assert_eq!(restored.world().machines[0].callbacks.len(), 1);
}

#[test]
fn collection_views_round_trip_through_snapshots() {
    let source = r#"
slice = [1, 2, 3].slice_view(1, 2)
table = {"a": 1, "b": 2}
(slice, table.keys(), table.values(), table.entries())
"#;
    let module = compile_text("collections.lm", source).expect("the source compiles");
    let loaded = lm_vm::load(module).expect("the module loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let outcome = lm_proc::run_world(&mut world);
    let expected = world.show_outcome(&outcome);
    let gate = world.next_gate();
    let image = world
        .capture_snapshot(gate, 0, false)
        .expect("the terminal view snapshot succeeds");
    let admitted = codec::load_external(image.bytes(), &loaded, LoadLimits::default())
        .expect("the view snapshot loads");
    let encoded = codec::encode(admitted.world(), usize::MAX).expect("the image encodes");
    assert_eq!(encoded.as_slice(), image.bytes().as_ref());

    let mut fresh = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let target = fresh.new_child(0).expect("the target exists");
    let root = fresh
        .restore_image(0, target, &admitted)
        .expect("the view snapshot restores");
    let RootEvent::Done(value) = fresh.run_machine(root) else {
        panic!("the restored view result is terminal");
    };
    assert_eq!(
        format!("Done({})", fresh.show_result_of(root, value)),
        expected
    );
}

#[test]
fn snapshots_preserve_spare_collection_capacity() {
    let source = r#"
values: List[Int] = []
values.reserve(20)
values.push(1)
table: Map[Int, Int] = {}
table.reserve(20)
table.put(1, 2)
(values, table)
"#;
    let module = compile_text("collections.lm", source).expect("the source compiles");
    let loaded = lm_vm::load(module).expect("the module loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(([1], {1: 2}))");
    let gate = world.next_gate();
    let snapshot = world
        .capture_snapshot(gate, 0, false)
        .expect("the snapshot succeeds");
    let before = collection_capacities(snapshot.world());
    assert!(before.0 >= 20);
    assert!(before.1 >= 20);

    let bytes = codec::encode(snapshot.world(), usize::MAX).expect("the image encodes");
    let decoded = codec::decode(&bytes, LoadLimits::default()).expect("the image decodes");
    assert_eq!(collection_capacities(&decoded), before);

    let admitted = codec::load_external(&bytes, &loaded, LoadLimits::default())
        .expect("the image is admitted");
    let mut fresh = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let target = fresh.new_child(0).expect("the target exists");
    let root = fresh
        .restore_image(0, target, &admitted)
        .expect("the image restores");
    assert_eq!(heap_collection_capacities(fresh.heap_of(root)), before);
}

fn collection_capacities(image: &lm_vm::snapshot::Image) -> (usize, usize) {
    let mut list = None;
    let mut map = None;
    for object in image
        .machines
        .iter()
        .flat_map(|machine| machine.objects.iter())
    {
        match &object.object {
            Object::List { items, .. } if items.len() == 1 => list = Some(items.capacity()),
            Object::Map { entries, .. } if entries.len() == 1 => map = Some(entries.capacity()),
            _ => {}
        }
    }
    (
        list.expect("the image contains the list"),
        map.expect("the image contains the map"),
    )
}

fn heap_collection_capacities(heap: &lm_heap::Heap) -> (usize, usize) {
    let mut list = None;
    let mut map = None;
    heap.for_each_live(|_, _, object| match object {
        Object::List { items, .. } if items.len() == 1 => list = Some(items.capacity()),
        Object::Map { entries, .. } if entries.len() == 1 => map = Some(entries.capacity()),
        _ => {}
    });
    (
        list.expect("the heap contains the list"),
        map.expect("the heap contains the map"),
    )
}

#[test]
fn snapshot_decoder_rejects_epochs_outside_supported_range() {
    let module = compile_text("collections.lm", "[1]\n").expect("the source compiles");
    let loaded = lm_vm::load(module).expect("the module loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done([1])");
    let gate = world.next_gate();
    let snapshot = world
        .capture_snapshot(gate, 0, false)
        .expect("the snapshot succeeds");
    let mut image = snapshot.into_image();
    let epoch = image
        .machines
        .iter_mut()
        .flat_map(|machine| machine.objects.iter_mut())
        .find_map(|object| match &mut object.object {
            Object::List { epoch, .. } => Some(epoch),
            _ => None,
        })
        .expect("the snapshot contains a list");
    *epoch = StructuralEpoch(u32::MAX);

    let mut bytes = codec::encode(&image, usize::MAX).expect("the edited image encodes");
    let valid = u64::from(u32::MAX).to_le_bytes();
    let position = bytes
        .windows(valid.len())
        .position(|window| window == valid)
        .expect("the image contains the epoch");
    bytes[position..position + valid.len()]
        .copy_from_slice(&(u64::from(u32::MAX) + 1).to_le_bytes());
    let body = bytes.len() - 32;
    let hash = codec::container_hash(&bytes[..body]);
    bytes[body..].copy_from_slice(&hash);
    let failure =
        codec::decode(&bytes, LoadLimits::default()).expect_err("the oversized epoch must fail");
    assert_eq!(failure.reason, ImageReason::Layout);
    assert!(failure.detail.contains("collection epoch"));
}

#[test]
fn option_payloads_remain_distinct_inside_collections() {
    let source = r#"
values: List[Option[Int]] = [Some(1), None]
table: Map[Int, Option[Int]] = {1: Some(2), 2: None}
nested: Option[Option[Int]] = Some(None)
(values, table, nested)
"#;
    let (result, heap) = run(source);
    assert_eq!(
        result,
        "Done(([Some(1), None], {1: Some(2), 2: None}, Some(None)))"
    );
    assert_eq!(heap.slots, 3);
}

#[test]
fn option_digests_encode_each_semantic_wrapper() {
    let source = r#"
some_int: List[Option[Int]] = [Some(5)]
plain_int: List[Int] = [5]
some_none: List[Option[Option[Int]]] = [Some(None)]
plain_none: List[Option[Int]] = [None]
outer_none: List[Option[Option[Int]]] = [None]
some_int.freeze()
plain_int.freeze()
some_none.freeze()
plain_none.freeze()
outer_none.freeze()
(
  some_int.digest() != plain_int.digest(),
  some_none.digest() != plain_none.digest(),
  some_none.digest() != outer_none.digest()
)
"#;
    assert_eq!(outcome(source), "Done((true, true, true))");
}

#[test]
fn option_digests_follow_all_typed_object_edges() {
    let source = r#"
final class Box[T]
  value: T

  def init(mut self, value: T)
    self.value = value
  end
end

def held[T](value: T): Digest
  closure = do ||: T value end
  closure.digest()
end

some_box = Box[Option[Int]](Some(5))
plain_box = Box[Int](5)
some_tuple: (Option[Int],) = (Some(5),)
plain_tuple: (Int,) = (5,)
some_map: Map[Int, Option[Int]] = {1: Some(5)}
plain_map: Map[Int, Int] = {1: 5}
some_box.freeze()
plain_box.freeze()
some_tuple.freeze()
plain_tuple.freeze()
some_map.freeze()
plain_map.freeze()
(
  some_box.digest() != plain_box.digest(),
  some_tuple.digest() != plain_tuple.digest(),
  some_map.digest() != plain_map.digest(),
  held[Option[Int]](Some(5)) != held[Int](5)
)
"#;
    assert_eq!(outcome(source), "Done((true, true, true, true))");
}

#[test]
fn scalar_digest_calls_fail_during_checking() {
    let failure = error("1.digest()\n");
    assert!(failure.contains("E1026") || failure.contains("E1027"));
    assert!(!failure.contains("verifier"));
    let failure = error("1.freeze()\n");
    assert!(failure.contains("E1026") || failure.contains("E1027"));
    assert!(!failure.contains("verifier"));
}

#[test]
fn empty_map_constructors_check_their_key_type() {
    let source = r#"
final class Key
  value: Int = 1
end

Map[Key, Int]()
"#;
    assert!(error(source).contains("E1033"));
}

#[test]
fn repeated_loop_wildcards_bind_no_name() {
    let source = r#"
count = 0
for _, _ in {1: 2, 3: 4}
  count = count + 1
end
count
"#;
    assert_eq!(outcome(source), "Done(2)");
    assert!(error("for key, key in {1: 2}\nend\n").contains("E1010"));
}

#[test]
fn equality_gives_bare_none_the_other_operand_type() {
    let source = r#"
present: Option[Int] = Some(1)
absent: Option[Int] = None
(present == None, absent == None)
"#;
    assert_eq!(outcome(source), "Done((false, true))");
}

#[test]
fn text_map_queries_accept_all_text_values() {
    let source = r#"
text = " ell "
part = text.trim()
table: Map[String, Int] = {"ell": 7}
(table.has(part), table.get(part), table.at(part))
"#;
    assert_eq!(outcome(source), "Done((true, Some(7), 7))");
}

#[test]
fn the_verifier_rejects_callback_containers_and_type_arguments() {
    let source = r#"
def id[T](value: T): T
  value
end

def use_callback(f: (Int) -> Int): Int
  id[Int](f(1))
end

use_callback() { |value: Int| value + 1 }
"#;
    let module = compile_text("collections.lm", source).expect("the source compiles");
    let callback = module
        .types
        .iter()
        .position(|ty| matches!(ty, BcType::Callback(..)))
        .expect("the module has a callback type") as u32;

    let mut nested = module.clone();
    nested.types.push(BcType::List(callback));
    let failure = lm_verify::verify_module(&nested).expect_err("the nested callback verifies");
    assert!(failure.message.contains("inside another type"));

    let mut applied = module;
    applied.apps[0].types[0] = callback;
    let failure =
        lm_verify::verify_module(&applied).expect_err("the callback application verifies");
    assert!(failure.message.contains("nonescaping callback"));
}

#[test]
fn the_verifier_checks_iterable_associated_equality() {
    let source = r#"
final class NumbersIterator implements Iterator
  type Item = Int

  def next(mut self): Option[Int]
    None
  end
end

final class Numbers implements Iterable
  type Item = Int
  type Iter = NumbersIterator

  def iterator(self): NumbersIterator
    NumbersIterator()
  end
end

Numbers()
"#;
    let mut module = compile_text("collections.lm", source).expect("the source compiles");
    let iterable = module
        .interfaces
        .iter()
        .position(|interface| interface.key == "core.Iterable")
        .expect("the core interface exists") as u32;
    let conformance = module
        .conformances
        .iter_mut()
        .find(|item| item.application.interface == iterable && item.associated[0] == 2)
        .expect("the user conformance exists");
    conformance.associated[0] = 1;
    let failure = lm_verify::verify_module(&module).expect_err("the forged equality verifies");
    assert!(failure
        .message
        .contains("Iterable.Item must equal Iterable.Iter.Item"));
}

#[test]
fn option_payload_requires_the_some_case_type() {
    let mut module = compile_text(
        "collections.lm",
        "case Some(1)\nin Some(value) then value\nin None then 0\nend\n",
    )
    .expect("the source compiles");
    let payload = module
        .funcs
        .iter_mut()
        .flat_map(|function| function.blocks.iter_mut().flatten())
        .find(|item| matches!(item, Instr::Extended(ExtendedInstr::OptionPayload { .. })))
        .expect("the program reads a native payload");
    *payload = Instr::Extended(ExtendedInstr::OptionPayload { ty: 2 });
    let failure = lm_verify::verify_module(&module).expect_err("the forged payload verifies");
    assert!(failure.message.contains("OptionPayload"));
}

#[test]
fn the_verifier_rejects_forged_view_operations() {
    let mut list = compile_text(
        "collections.lm",
        "view = [1, 2].slice_view(0, 1)\nview.len()\n",
    )
    .expect("the list view compiles");
    let operation = list
        .funcs
        .iter_mut()
        .flat_map(|function| function.blocks.iter_mut().flatten())
        .find(|item| matches!(item, Instr::Extended(ExtendedInstr::ListIterLen)))
        .expect("the list view checks its epoch");
    *operation = Instr::Extended(ExtendedInstr::MapIterLen);
    let failure = lm_verify::verify_module(&list).expect_err("the forged list view verifies");
    assert!(failure.message.contains("map"));

    let mut map = compile_text("collections.lm", "view = {1: 2}.keys()\nview.at(0)\n")
        .expect("the map view compiles");
    let operation = map
        .funcs
        .iter_mut()
        .flat_map(|function| function.blocks.iter_mut().flatten())
        .find(|item| matches!(item, Instr::Extended(ExtendedInstr::MapKeyAt)))
        .expect("the map view reads a key");
    *operation = Instr::ListAt;
    let failure = lm_verify::verify_module(&map).expect_err("the forged map view verifies");
    assert!(failure.message.contains("list"));
}
