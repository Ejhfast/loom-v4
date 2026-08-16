//! Week-7 graph suites.
//!
//! The transfer cases in this file are migration oracles. They ran
//! against the transfer path that existed before the `lm-graph`
//! engine, and they keep the same result after the migration. They
//! cover cycles and shared subgraphs, which the week-4 transfer tests
//! never reached.

use lm_testkit::run_allowed;

fn allowed(source: &str) -> String {
    run_allowed("t.lm", source, &["Vm"]).unwrap()
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
/// `from_object` and keeps its cycle in the destination heap.
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
  vm = sys.vm.Vm().from_object(do |n: Node|: Int
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
  in Fault(_) then 0 - 1
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
  vm = sys.vm.Vm().from_object(do |n: Node|: Bool
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
  vm = sys.vm.Vm().from_object(do ||: Node
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
  in Fault(_) then 0 - 1
  end
end

go()
"
    );
    assert_eq!(allowed(&source), "Done(75)");
}

/// A mutable node inside a cyclic graph still rejects. The cyclic
/// walk must not treat the cycle as a termination proof.
#[test]
fn a_partly_mutable_cycle_still_rejects() {
    let source = format!(
        "{NODE}
def go(): String with Vm
  a = Node(1)
  b = Node(2)
  a.next = Some(b)
  b.next = Some(a)
  vm = sys.vm.Vm().from_object(do |n: Node|: Int
    n.value
  end, args: (a,))
  \"loaded\"
end

go()
"
    );
    assert_eq!(allowed(&source), "Fault(UnsendableValue)");
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
  vm = sys.vm.Vm().from_object(do |g: Diamond|: Bool
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
  vm = sys.vm.Vm().from_object(do ||: (Leaf, Leaf)
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
