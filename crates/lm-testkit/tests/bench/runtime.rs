use super::*;

#[test]
#[ignore]
fn bench_collection_operations() {
    let base = baseline();
    println!("LOOM\tcase\titers\tns_per_op\ttotal_ms");
    println!(
        "LOOM\t_baseline\t1\t{:.1}\t{:.3}",
        base.as_nanos() as f64,
        base.as_secs_f64() * 1e3
    );

    // Native list traversal creates no iterator or Option per element.
    report(
        "list_for",
        1_000_000,
        "xs: [Int] = []\ni = 0\nwhile i < 1000\n  xs.push(i)\n  i = i + 1\nend\n\
         rounds = 0\ns = 0\nwhile rounds < 1000\n  for value in xs\n    s = s + value\n  end\n\
           rounds = rounds + 1\nend\ns\n",
        base,
    );

    // A nonescaping callback avoids one closure object per call.
    report(
        "list_each",
        1_000_000,
        "class Total\n  value: Int = 0\n  def add(mut self, n: Int)\n    self.value = self.value + n\n  end\nend\n\
         xs: [Int] = []\ni = 0\nwhile i < 1000\n  xs.push(i)\n  i = i + 1\nend\n\
           total = Total()\nrounds = 0\nwhile rounds < 1000\n  xs.each() { |value: Int| total.add(value) }\n\
           rounds = rounds + 1\nend\ntotal.value\n",
        base,
    );

    // This eager pipeline applies three ordinary core algorithms.
    report(
        "list_pipeline",
        60_000,
        "xs: [Int] = []\ni = 0\nwhile i < 20000\n  xs.push(i)\n  i = i + 1\nend\n\
         mapped = xs.map[Int]() { |value: Int| value + 1 }\n\
         filtered = mapped.filter() { |value: Int| value % 2 == 0 }\n\
         filtered.fold[Int](0) { |sum: Int, value: Int| sum + value }\n",
        base,
    );

    // Map traversal passes the key and value without a tuple object.
    report(
        "map_each",
        1_000_000,
        "class Total\n  value: Int = 0\n  def add(mut self, key: Int, value: Int)\n    self.value = self.value + key + value\n  end\nend\n\
         table: {Int: Int} = {}\ni = 0\nwhile i < 1000\n  table.put(i, i)\n  i = i + 1\nend\n\
           total = Total()\nrounds = 0\nwhile rounds < 1000\n  table.each() { |key: Int, value: Int| total.add(key, value) }\n\
           rounds = rounds + 1\nend\ntotal.value\n",
        base,
    );
}

#[test]
#[ignore]
fn bench_proc_operations() {
    let source = "class Adder < Proc[Int]\n\
                  \x20 total: Int = 0\n\
                  \x20 def on_spawn(mut self): Int with Proc\n\
                  \x20   loop do\n\
                  \x20     case self.receive()\n\
                  \x20     in Msg(n)\n\
                  \x20       self.total = self.total + n\n\
                  \x20     in Closed\n\
                  \x20       return self.total\n\
                  \x20     end\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  h = Adder.spawn()\n\
                  i = 0\n\
                  while i < 20000\n  h.send(1)\n  i = i + 1\nend\n\
                  h.close()\n\
                  case h.done()\n\
                  in Ok(v)  then v\n\
                  in Err(_) then -1\n\
                  end\n";
    let elapsed = time_world(source, &["Proc"], config(), "Done(20000)");
    println!(
        "LOOM\tproc_send_receive\t20000\t{:.1}\t{:.3}",
        elapsed.as_nanos() as f64 / 20_000.0,
        elapsed.as_secs_f64() * 1e3
    );
}

#[test]
#[ignore]
fn bench_in_memory_branch() {
    let snapshot_reuse = r#"
def choose(): Int with Rand.Int
  sys.rand.int(0, 100)
end

def finish(run: Run[Int], answer: Int): Int with Vm
  case run.drive()
  in Asked(request)
    case request
    in Call(Rand.Int, call, (_, _))
      run.answer(call, answer)
      run.run().value_or(-1000)
    in _ then -2000
    end
  in Done(value) then value
  in Fault(_) then -3000
  end
end

original = sys.vm.Vm().activate_or_fault(choose, args: ())
case original.drive()
in Asked(_)
  image = case original.snapshot()
  in Ok(value) then value
  in Err(error) then panic(display(error))
  end
  total = 0
  index = 0
  while index < 100
    copy = case sys.vm.Vm().restore(image)
    in Ok(value) then value
    in Err(error) then panic(display(error))
    end
    total = total + finish(copy, index)
    index = index + 1
  end
  finish(original, 100)
  total
in Done(value) then value
in Fault(fault) then raise(fault)
end
"#;
    let snapshot_fresh = r#"
def choose(): Int with Rand.Int
  sys.rand.int(0, 100)
end

def finish(run: Run[Int], answer: Int): Int with Vm
  case run.drive()
  in Asked(request)
    case request
    in Call(Rand.Int, call, (_, _))
      run.answer(call, answer)
      run.run().value_or(-1000)
    in _ then -2000
    end
  in Done(value) then value
  in Fault(_) then -3000
  end
end

original = sys.vm.Vm().activate_or_fault(choose, args: ())
case original.drive()
in Asked(_)
  total = 0
  index = 0
  while index < 100
    image = case original.snapshot()
    in Ok(value) then value
    in Err(error) then panic(display(error))
    end
    copy = case sys.vm.Vm().restore(image)
    in Ok(value) then value
    in Err(error) then panic(display(error))
    end
    total = total + finish(copy, index)
    index = index + 1
  end
  finish(original, 100)
  total
in Done(value) then value
in Fault(fault) then raise(fault)
end
"#;
    let branch = r#"
def choose(): Int with Rand.Int
  sys.rand.int(0, 100)
end

def finish(run: Run[Int], answer: Int): Int with Vm
  case run.drive()
  in Asked(request)
    case request
    in Call(Rand.Int, call, (_, _))
      run.answer(call, answer)
      run.run().value_or(-1000)
    in _ then -2000
    end
  in Done(value) then value
  in Fault(_) then -3000
  end
end

original = sys.vm.Vm().activate_or_fault(choose, args: ())
case original.drive()
in Asked(_)
  total = 0
  index = 0
  while index < 100
    copy = case original.branch()
    in Ok(value) then value
    in Err(error) then panic(display(error))
    end
    total = total + finish(copy, index)
    index = index + 1
  end
  finish(original, 100)
  total
in Done(value) then value
in Fault(fault) then raise(fault)
end
"#;
    let answered_branch = r#"
def choose(): Int with Rand.Int
  sys.rand.int(0, 100)
end

original = sys.vm.Vm().activate_or_fault(choose, args: ())
case original.drive()
in Asked(request)
  case request
  in Call(Rand.Int, call, (_, _))
    total = 0
    index = 0
    while index < 100
      copy = case original.branch_answer(call, index)
      in Ok(value) then value
      in Err(error) then panic(display(error))
      end
      total = total + copy.run().value_or(-1000)
      index = index + 1
    end
    original.answer(call, 100)
    original.run().value_or(-2000)
    total
  in _ then -3000
  end
in Done(value) then value
in Fault(fault) then raise(fault)
end
"#;
    let reused = time_world(snapshot_reuse, &["Vm"], config(), "Done(4950)");
    let fresh = time_world(snapshot_fresh, &["Vm"], config(), "Done(4950)");
    let branched = time_world(branch, &["Vm"], config(), "Done(4950)");
    let answered = time_world(answered_branch, &["Vm"], config(), "Done(4950)");
    let reuse_ratio = branched.as_secs_f64() / reused.as_secs_f64();
    let fresh_ratio = branched.as_secs_f64() / fresh.as_secs_f64();
    assert!(
        fresh_ratio <= 1.0,
        "an in-memory branch must beat a fresh snapshot and restore"
    );
    println!(
        "LOOM\tvm_branch\t100\t{:.3}\t{:.3}\t{:.3}\t{reuse_ratio:.3}\t{fresh_ratio:.3}",
        reused.as_secs_f64() * 1e3,
        fresh.as_secs_f64() * 1e3,
        branched.as_secs_f64() * 1e3
    );
    let answered_ratio = answered.as_secs_f64() / branched.as_secs_f64();
    println!(
        "LOOM\tvm_branch_answer\t100\t{:.3}\t{answered_ratio:.3}",
        answered.as_secs_f64() * 1e3
    );
    assert!(
        answered_ratio <= 1.05,
        "answered branching took {answered_ratio:.3} times plain branching"
    );
}

#[test]
#[ignore]
fn bench_vm_machine_lifecycle() {
    let (source, expected) = multishot_queens_source(9);
    let adaptive = time_world(&source, &["Vm"], config(), &expected);
    let former_limit = time_world(
        &source,
        &["Vm"],
        VmConfig {
            max_children: 1_024,
            ..config()
        },
        &expected,
    );
    let ratio = adaptive.as_secs_f64() / former_limit.as_secs_f64();
    println!("LOOM\tcase\tsize\tadaptive_ms\tformer_limit_ms\tratio");
    println!(
        "LOOM\tvm_machine_lifecycle\t9\t{:.3}\t{:.3}\t{ratio:.3}",
        adaptive.as_secs_f64() * 1e3,
        former_limit.as_secs_f64() * 1e3,
    );
    assert!(
        ratio <= 1.20,
        "adaptive reclamation took {ratio:.3} times limit-driven reclamation"
    );
}
