//! Core protocol tests.

use lm_testkit::run_allowed;
use lm_vm::{RecordingHost, VmConfig, World};

fn run(source: &str) -> Result<String, String> {
    run_allowed("core-protocols.lm", source, &[])
}

#[test]
fn core_protocols_compose_across_native_values() {
    let source = r#"
values = [3, 1, 2]
values.sort()

left = {1: "one", 2: "two"}
right = {2: "two", 1: "one"}

first = Set[Int]()
first.add(2)
first.add(1)
second = Set[Int]()
second.add(1)
second.add(2)

key = (1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16)
table = {key: 99}

option: Option[Int] = Some(4)
nested_some: Option[Option[Int]] = Some(None)
nested_none: Option[Option[Int]] = None
nested_table = {nested_some: 1, nested_none: 2}
result: Result[Int, String] = Ok(5)
slice = values.slice_view(0, 2)

(
  display(values),
  values.min().expect("minimum"),
  values.max().expect("maximum"),
  left == right,
  hash_of(left) == hash_of(right),
  first == second,
  hash_of(first) == hash_of(second),
  table.at(key),
  display(option),
  display(nested_some),
  hash_of(nested_some) != hash_of(nested_none),
  (nested_table.len(), nested_table.at(nested_some), nested_table.at(nested_none)),
  display(result),
  display(slice),
  (1, 2).compare((1, 3)).is_less(),
  values.compare([1, 2, 4]).is_less()
)
"#;
    assert_eq!(
        run(source).unwrap(),
        "Done((\"[1, 2, 3]\", 1, 3, true, true, true, true, 99, \"Some(4)\", \"Some(None)\", true, (2, 1, 2), \"Ok(5)\", \"[1, 2]\", true, true))"
    );
}

#[test]
fn builder_copies_own_independent_storage() {
    let source = r#"
text = StringBuilder()
text.append("a")
text_copy = text.copy()
text_copy.append("b")

bytes = ByteBuffer()
bytes.append(1)
bytes_copy = bytes.copy()
bytes_copy.append(2)

(text.build(), text_copy.build(), bytes.build().len(), bytes_copy.build().len())
"#;
    assert_eq!(run(source).unwrap(), "Done((\"a\", \"ab\", 1, 2))");
}

#[test]
fn error_is_one_common_display_bound() {
    let source = r#"
def message[E: Error](error: E): String
  builder = StringBuilder()
  builder.append("error: ")
  error.append_to(builder)
  builder.finish()
end

(
  message(FsError.Closed),
  message(RestoreError.RestoreLimitExceeded),
  message(ProcError.InUse)
)
"#;
    assert_eq!(
        run(source).unwrap(),
        "Done((\"error: file handle is closed\", \"error: the restored world exceeds its limit\", \"error: the proc is in use\"))"
    );
}

#[test]
fn an_unmet_collection_protocol_names_its_premise() {
    let source = r#"
final class Plain
end

display([Plain()])
"#;
    let error = run(source).expect_err("the missing protocol must reject");
    assert!(error.contains("does not conform to `Display`"), "{error}");
    assert!(
        error.contains("because `Plain` does not conform to `Display`"),
        "{error}"
    );
}

#[test]
fn map_tombstones_preserve_order_views_and_protocols() {
    let source = r#"
table: Map[Int, Int] = {}
for value in Range(0, 20)
  table.put(value, value * 10)
end
table.remove(3)
table.remove(7)

sum = 0
for key, _ in table
  sum = sum + key
end

before = (
  table.len(),
  table.keys().at(3),
  table.values().at(6),
  table.entries().at(7),
  sum
)

for value in Range(0, 10)
  table.remove(value)
end
table.put(3, 30)

dense: Map[Int, Int] = {}
for value in Range(10, 20)
  dense.put(value, value * 10)
end
dense.put(3, 30)

(
  before,
  table.len(),
  table.keys().at(0),
  table.keys().at(10),
  table == dense,
  hash_of(table) == hash_of(dense)
)
"#;
    assert_eq!(
        run(source).unwrap(),
        "Done(((18, 4, 80, (9, 90), 180), 11, 10, 3, true, true))"
    );
}

#[test]
fn frozen_classes_are_stable_keys_without_a_freeze_call() {
    let source = r#"
frozen class Key implements Hashable
  value: Int

  def init(mut self, value: Int)
    self.value = value
  end

  def __eq__(self, other: Key): Bool
    self.value == other.value
  end

  def __hash__(self): Int
    hash_combine(17, self.value)
  end
end

frozen class Label[T] implements Display when T: Display
  value: T

  def init(mut self, value: T)
    self.value = value
  end

  def append_to(self, mut builder: StringBuilder) when T: Display
    self.value.append_to(builder)
    ()
  end
end

table = {Key(4): "four"}
(table.at(Key(4)), display(Label[String]("ready")))
"#;
    assert_eq!(run(source).unwrap(), "Done((\"four\", \"ready\"))");
}

#[test]
fn frozen_classes_reject_mutable_storage_and_receivers() {
    let field = run(r#"
frozen class Bad
  values: List[Int] = []
end
Bad()
"#)
    .expect_err("a mutable field must reject");
    assert!(field.contains("not always frozen"), "{field}");

    let method = run(r#"
frozen class Bad
  value: Int = 0
  def change(mut self)
    self.value = 1
  end
end
Bad()
"#)
    .expect_err("a mutable method must reject");
    assert!(
        method.contains("permits `mut self` only in `init`"),
        "{method}"
    );

    let argument = run(r#"
frozen class Box[T]
  value: T
  def init(mut self, value: T)
    self.value = value
  end
end
Box[List[Int]]([])
"#)
    .expect_err("a mutable type argument must reject");
    assert!(
        argument.contains("always-frozen type arguments"),
        "{argument}"
    );
}

#[test]
fn map_keys_require_deep_freezing() {
    let source = r#"
key = ([1], 2)
table = {key: 3}
table.len()
"#;
    assert_eq!(run(source).unwrap(), "Fault(MutableMapKey)");
}

#[test]
fn a_mutable_map_key_fault_names_the_repair() {
    let source = "table = {([1], 2): 3}\ntable.len()\n";
    let bytes =
        lm_testkit::compile_to_bytes("mutable-map-key.lm", source).expect("the source compiles");
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the artifact loads");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Fault(MutableMapKey)");
    let fault = world.root_fault().expect("the root machine faulted");
    assert_eq!(
        fault.message,
        "freeze the key before insertion, or declare a suitable `frozen class`"
    );
}
