//! Week-7 graph suites.
//!
//! The transfer cases in this file are migration oracles. They ran
//! against the transfer path that existed before the `lm-graph`
//! engine, and they keep the same result after the migration. They
//! cover cycles and shared subgraphs, which the week-4 transfer tests
//! never reached.

use lm_testkit::{run_allowed, run_text};
use lm_vm::VmConfig;

fn allowed(source: &str) -> String {
    run_allowed("t.lm", source, &["Vm"]).unwrap()
}

fn run(name: &str, source: &str) -> String {
    run_text(name, source, VmConfig::default()).unwrap()
}

/// The node class the cyclic cases share.
const NODE: &str = "\
class Node
  value: Int
  next: Option[Node] = None

  def init(mut self, value: Int)
    self.value = value
  end
end
";

// ---------------------------------------------------------------
// Cyclic transfer.
// ---------------------------------------------------------------

/// A two-node cycle crosses the boundary as an argument of
/// `activate` and keeps its cycle in the destination heap.
#[test]
fn a_frozen_two_node_cycle_crosses_as_a_program_argument() {
    let source = format!(
        "{NODE}
def go(): Int with Vm
  a = Node(1)
  b = Node(2)
  a.next = Some(b)
  b.next = Some(a)
  a.freeze()
  vm = sys.vm.Vm().activate_or_fault(do |n: Node|: Int
    case n.next
    in Some(m)
      case m.next
      in Some(k) then k.value * 100 + m.value * 10 + n.value
      in None    then 0
      end
    in None then 0
    end
  end, args: (a,))
  case vm.run()
  in Done(v)  then v
  in Fault(_) then -1
  end
end

go()
"
    );
    assert_eq!(allowed(&source), "Done(121)");
}

/// The cycle keeps its identity: the node two steps along the cycle
/// is the same object as the root.
#[test]
fn a_transferred_cycle_keeps_reference_identity() {
    let source = format!(
        "{NODE}
def go(): Bool with Vm
  a = Node(1)
  b = Node(2)
  a.next = Some(b)
  b.next = Some(a)
  a.freeze()
  vm = sys.vm.Vm().activate_or_fault(do |n: Node|: Bool
    case n.next
    in Some(m)
      case m.next
      in Some(k) then k == n
      in None    then false
      end
    in None then false
    end
  end, args: (a,))
  case vm.run()
  in Done(v)  then v
  in Fault(_) then false
  end
end

go()
"
    );
    assert_eq!(allowed(&source), "Done(true)");
}

/// A cycle built inside a child machine crosses back through the
/// terminal result.
#[test]
fn a_cycle_crosses_back_through_the_terminal_result() {
    let source = format!(
        "{NODE}
def go(): Int with Vm
  vm = sys.vm.Vm().activate_or_fault(do ||: Node
    a = Node(7)
    b = Node(5)
    a.next = Some(b)
    b.next = Some(a)
    a.freeze()
  end, args: ())
  case vm.run()
  in Done(n)
    case n.next
    in Some(m)
      case m.next
      in Some(k) then k.value * 10 + m.value
      in None    then 0
      end
    in None then 0
    end
  in Fault(_) then -1
  end
end

go()
"
    );
    assert_eq!(allowed(&source), "Done(75)");
}

/// A mutable cycle crosses as a mutable copy. The cyclic walk must
/// not treat the cycle as a termination proof, and the copy must not
/// share one node with the source.
#[test]
fn a_partly_mutable_cycle_crosses_as_a_copy() {
    let source = format!(
        "{NODE}
def go(): Int with Vm
  a = Node(1)
  b = Node(2)
  a.next = Some(b)
  b.next = Some(a)
  vm = sys.vm.Vm().activate_or_fault(do |n: Node|: Int
    case n.next
    in Some(m)
      case m.next
      in Some(k) then k.value * 100 + m.value * 10 + n.value
      in None    then 0
      end
    in None then 0
    end
  end, args: (a,))
  a.value = 9
  case vm.run()
  in Done(v)  then v
  in Fault(_) then -1
  end
end

go()
"
    );
    // 121 proves two things: the copy closed its own cycle, and the
    // later write into the source never reached the copy.
    assert_eq!(allowed(&source), "Done(121)");
}

// ---------------------------------------------------------------
// Shared-subgraph transfer.
// ---------------------------------------------------------------

/// A diamond keeps its sharing: both branches of the copy point at
/// one destination object, not at two equal copies.
#[test]
fn a_shared_subgraph_stays_shared_after_transfer() {
    let source = "\
class Leaf
  tag: Int

  def init(mut self, tag: Int)
    self.tag = tag
  end
end

class Pair
  left: Leaf
  right: Leaf

  def init(mut self, left: Leaf, right: Leaf)
    self.left = left
    self.right = right
  end
end

class Diamond
  a: Pair
  b: Pair

  def init(mut self, a: Pair, b: Pair)
    self.a = a
    self.b = b
  end
end

def go(): Bool with Vm
  leaf = Leaf(3)
  top = Pair(leaf, leaf)
  bottom = Pair(leaf, leaf)
  d = Diamond(top, bottom)
  d.freeze()
  vm = sys.vm.Vm().activate_or_fault(do |g: Diamond|: Bool
    g.a.left == g.a.right and g.a.left == g.b.right and g.a.left.tag == 3
  end, args: (d,))
  case vm.run()
  in Done(v)  then v
  in Fault(_) then false
  end
end

go()
";
    assert_eq!(allowed(source), "Done(true)");
}

/// A shared leaf crosses back inside a terminal tuple and stays one
/// object there.
#[test]
fn a_shared_subgraph_stays_shared_through_a_terminal_tuple() {
    let source = "\
class Leaf
  tag: Int

  def init(mut self, tag: Int)
    self.tag = tag
  end
end

def go(): Bool with Vm
  vm = sys.vm.Vm().activate_or_fault(do ||: (Leaf, Leaf)
    leaf = Leaf(9)
    pair = (leaf, leaf)
    pair.freeze()
  end, args: ())
  case vm.run()
  in Done(p)  then p[0] == p[1] and p[0].tag == 9
  in Fault(_) then false
  end
end

go()
";
    assert_eq!(allowed(source), "Done(true)");
}

// ---------------------------------------------------------------
// The canonical digest, from guest code.
// ---------------------------------------------------------------

/// The digest depends on the graph, never on the heap slots the
/// objects happen to use.
#[test]
fn the_guest_digest_ignores_allocation_order() {
    let source = format!(
        "{NODE}
def ring(): Node
  a = Node(1)
  b = Node(2)
  a.next = Some(b)
  b.next = Some(a)
  a.freeze()
end

first = ring()
spare = [Node(7), Node(8), Node(9)]
second = ring()
first.digest() == second.digest()
"
    );
    assert_eq!(run("t.lm", &source), "Done(true)");
}

/// A digest compares by value across different heap slots.
#[test]
fn digest_equality_is_by_value() {
    let source = "\
xs = [1, 2, 3]
xs.freeze()
ys = [1, 2, 3]
ys.freeze()
xs.digest() == ys.digest()
";
    assert_eq!(run("t.lm", source), "Done(true)");
}

/// A digest of a graph that is not frozen faults.
#[test]
fn the_digest_needs_a_frozen_graph() {
    let source = "xs = [1, 2, 3]\nxs.digest()\n";
    assert_eq!(run("t.lm", source), "Fault(UnsendableValue)");
}

/// A frozen builder is not digestible: it has no canonical
/// encoding, so the digest mode rejects it.
#[test]
fn the_digest_rejects_a_nondigestible_shape() {
    let source = "\
sb = StringBuilder()
sb.append(\"text\")
sb.freeze()
sb.digest()
";
    assert_eq!(run("t.lm", source), "Fault(BoundaryViolation)");
}

/// A different graph shape gives a different digest, and equal
/// shapes with different contents differ too.
#[test]
fn different_graphs_digest_differently() {
    let source = "\
a = [1, 2]
a.freeze()
b = [2, 1]
b.freeze()
c = [[1], [2]]
c.freeze()
a.digest() != b.digest() and a.digest() != c.digest()
";
    assert_eq!(run("t.lm", source), "Done(true)");
}

// ---------------------------------------------------------------
// The Week 7 runnable examples.
// ---------------------------------------------------------------

#[test]
fn week7_examples_have_checked_output() {
    let read = |path: &str| {
        std::fs::read_to_string(lm_testkit::repo_root().join(path)).expect("example reads")
    };
    assert_eq!(
        run(
            "cycle-digest.lm",
            &read("examples/06-graphs/cycle-digest.lm")
        ),
        "Done(72b2bf56758dad3a96809ab7391f4f70bac1937fd85446fe90e781175bbe6a0a)"
    );
    assert_eq!(
        run(
            "brace-closure.lm",
            &read("examples/06-graphs/brace-closure.lm")
        ),
        "Done(42)"
    );
}

// ---------------------------------------------------------------
// Code and class identity.
// ---------------------------------------------------------------

/// A class crosses the digest by verified semantic identity, never by
/// its numeric slot. An unrelated class declared first moves the slot
/// of `Point` and must not move the digest.
#[test]
fn a_class_digests_by_semantic_identity_not_by_slot() {
    let point = "\
class Point
  x: Int

  def init(mut self, x: Int)
    self.x = x
  end
end

p = Point(1)
p.freeze()
p.digest()
";
    let shifted = format!(
        "\
class Other
  y: Int

  def init(mut self, y: Int)
    self.y = y
  end
end

{point}"
    );
    let plain = run("t.lm", point);
    assert_eq!(plain, run("t.lm", &shifted));
    // A different class body does move the digest.
    let renamed = "\
class Point
  z: Int

  def init(mut self, v: Int)
    self.z = v
  end
end

p = Point(1)
p.freeze()
p.digest()
";
    assert_ne!(plain, run("t.lm", renamed));
}

/// A closure crosses the digest by the definition hash of its code,
/// never by its numeric function slot.
#[test]
fn a_closure_digests_by_semantic_identity_not_by_slot() {
    let closure = "\
f = do |x: Int|: Int
  x + 1
end

pair = (f,)
pair.freeze()
pair.digest()
";
    let shifted = format!(
        "\
def unrelated(n: Int): Int
  n * 3
end

{closure}"
    );
    let plain = run("t.lm", closure);
    assert_eq!(plain, run("t.lm", &shifted));
    // A different body does move the digest.
    let changed = closure.replace("x + 1", "x + 2");
    assert_ne!(plain, run("t.lm", &changed));
}

/// The shape table is one declaration point, and the readable dump
/// covers every shape.
#[test]
fn the_shape_table_declares_every_column() {
    let dump = lm_vm::dump_shapes();
    assert_eq!(dump.lines().count(), 31);
    for line in dump.lines() {
        assert!(line.contains("boundary="), "{line}");
        assert!(line.contains("digestible="), "{line}");
        assert!(line.contains("snapshot="), "{line}");
        assert!(line.contains("children="), "{line}");
    }
}

/// A control envelope encodes each member independently
/// (specification 16.1). Sharing therefore holds inside one
/// transferred value and not across two members of one `args` view.
#[test]
fn the_control_envelope_encodes_each_member_independently() {
    let shared_inside = "\
class Box
  value: Int = 1
end

def go(): Bool with Vm
  a = Box()
  a.freeze()
  pair = (a, a)
  pair.freeze()
  vm = sys.vm.Vm().activate_or_fault(do |p: (Box, Box)|: Bool
    p[0] == p[1]
  end, args: (pair,))
  case vm.run()
  in Done(v)  then v
  in Fault(_) then false
  end
end

go()
";
    assert_eq!(allowed(shared_inside), "Done(true)");
    let shared_across = "\
class Box
  value: Int = 1
end

def go(): Bool with Vm
  a = Box()
  a.freeze()
  vm = sys.vm.Vm().activate_or_fault(do |p: Box, q: Box|: Bool
    p == q
  end, args: (a, a))
  case vm.run()
  in Done(v)  then v
  in Fault(_) then false
  end
end

go()
";
    assert_eq!(allowed(shared_across), "Done(false)");
}
