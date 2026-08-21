//! `Choose.Pick`: the search choice point.
//!
//! The operation states a number of candidates and answers one index.
//! A driver therefore reads one integer and writes one integer, and it
//! never reads a guest value. The candidates stay in the searched
//! machine, so one driver serves every searched program.
//!
//! No host answers the operation. A table denies by default, so a
//! program that performs it with no driver faults.

use lm_testkit::run_allowed;

/// A driver reads the candidate count and answers an index.
#[test]
fn a_driver_answers_a_choice_point_with_an_index() {
    let source = "def take(vm: Run[Int], choice: Int): Int with Vm
  loop do
    case vm.drive()
    in Asked(q)
      case q
      in Call(Choose.Pick, call, (n,))
        if n <= 0
          return -1
        end
        vm.answer(call, choice)
      in _
        return -2
      end
    in Done(value)
      return value
    in Fault(_)
      return -3
    end
  end
end

def program(): Int with Choose
  # No candidate list exists. The choice point names a count alone.
  10 * sys.choose.pick(4) + sys.choose.pick(4)
end

first = take(sys.vm.Vm().activate_or_fault(program, args: ()), 0)
third = take(sys.vm.Vm().activate_or_fault(program, args: ()), 2)
(first, third)
";
    assert_eq!(
        run_allowed("choose.lm", source, &["Vm"]).unwrap(),
        "Done((0, 22))"
    );
}

/// The candidates never cross the boundary. The searched program
/// reads its own list with the index the driver answered, so the
/// candidate type never reaches the driver.
#[test]
fn the_candidates_stay_in_the_searched_machine() {
    let source = "def take(vm: Run[String], choice: Int): String with Vm
  loop do
    case vm.drive()
    in Asked(q)
      case q
      in Call(Choose.Pick, call, (_,))
        vm.answer(call, choice)
      in _
        return \"other\"
      end
    in Done(value)
      return value
    in Fault(_)
      return \"fault\"
    end
  end
end

def pick[T](xs: [T]): T with Choose.Pick
  xs.at(sys.choose.pick(xs.len()))
end

def program(): String with Choose
  names: [String] = [\"ada\", \"grace\", \"alan\"]
  pick[String](names)
end

take(sys.vm.Vm().activate_or_fault(program, args: ()), 1)
";
    assert_eq!(
        run_allowed("choose-values.lm", source, &["Vm"]).unwrap(),
        "Done(\"grace\")"
    );
}

/// No host answers a choice point, so a table denies it by default.
#[test]
fn a_choice_point_with_no_driver_is_denied() {
    let source = "sys.choose.pick(3)\n";
    assert_eq!(
        run_allowed("lonely.lm", source, &[]).unwrap(),
        "Fault(PolicyDenied)"
    );
}

/// A pending choice point holds no host state, so it never blocks a
/// capture. This is the property multi-shot search rests on: the
/// driver copies the world at the choice point and restores one world
/// for each candidate.
#[test]
fn a_pending_choice_point_never_blocks_a_capture() {
    let source = "def branch(vm: Run[Int]): Int with Vm
  case vm.drive()
  in Asked(_)
    case vm.snapshot()
    in Ok(snap)
      # Two worlds from one copy, each taking its own candidate.
      one = answer_with(snap, 0)
      two = answer_with(snap, 1)
      10 * one + two
    in Err(_)
      -1
    end
  in Done(_)  then -2
  in Fault(_) then -3
  end
end

def answer_with(snap: RunSnapshot[Int], choice: Int): Int with Vm
  case sys.vm.Vm().restore(snap)
  in Ok(restored)
    case restored.drive()
    in Asked(q)
      case q
      in Call(Choose.Pick, call, (_,))
        restored.answer(call, choice)
        case restored.run()
        in Done(value) then value
        in Fault(_)    then -1
        end
      in _ then -2
      end
    in Done(value) then value
    in Fault(_)    then -3
    end
  in Err(_) then -4
  end
end

def program(): Int with Choose
  sys.choose.pick(2) + 5
end

branch(sys.vm.Vm().activate_or_fault(program, args: ()))
";
    // Candidate 0 answers 5 and candidate 1 answers 6.
    assert_eq!(
        run_allowed("choose-branch.lm", source, &["Vm"]).unwrap(),
        "Done(56)"
    );
}
