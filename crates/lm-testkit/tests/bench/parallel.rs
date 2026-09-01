use super::*;

#[test]
#[ignore]
fn bench_parallel_multishot_queens() {
    let source = std::fs::read_to_string(
        lm_testkit::repo_root().join("examples/14-vm-as-multishot-search/07-parallel-n-queens.lm"),
    )
    .expect("the multishot benchmark source reads")
    .replace("parallel_solutions(5)", "parallel_solutions(7)");
    let (direct_source, direct_expected) = iterable_queens_source(7, false);
    let direct = time_world(&direct_source, &[], config(), &direct_expected);
    let deterministic = time_world(&source, &["Vm", "Wait"], config(), "Done(40)");
    let parallel = time_parallel_world_with(&source, 4, &["Vm", "Wait"], "Done(40)");
    let speedup = deterministic.as_secs_f64() / parallel.as_secs_f64();
    let overhead = deterministic.as_secs_f64() / direct.as_secs_f64();
    println!(
        "LOOM\tcase\tsize\tworkers\tdirect_ms\tdeterministic_ms\tparallel_ms\tspeedup\toverhead"
    );
    println!(
        "LOOM\tparallel_multishot_queens\t7\t4\t{:.3}\t{:.3}\t{:.3}\t{speedup:.3}\t{overhead:.3}",
        direct.as_secs_f64() * 1e3,
        deterministic.as_secs_f64() * 1e3,
        parallel.as_secs_f64() * 1e3
    );
}

#[test]
#[ignore]
fn bench_parallel_cpu_scaling() {
    println!("LOOM\tcase\ttasks\tworkers\tserial_ms\tparallel_ms\tspeedup");
    for (tasks, workers, gate) in [(2, 2, 1.7), (4, 4, 3.0)] {
        let (source, expected) = parallel_cpu_source(tasks, 1_000_000);
        let serial = time_parallel_world(&source, 1, &expected);
        let parallel = time_parallel_world(&source, workers, &expected);
        let speedup = serial.as_secs_f64() / parallel.as_secs_f64();
        println!(
            "LOOM\tparallel_cpu\t{tasks}\t{workers}\t{:.3}\t{:.3}\t{speedup:.3}",
            serial.as_secs_f64() * 1e3,
            parallel.as_secs_f64() * 1e3
        );
        assert!(
            speedup >= gate,
            "{tasks} tasks reached {speedup:.3}x, below the {gate:.1}x gate"
        );
    }
}

#[test]
#[ignore]
fn bench_parallel_allocating_scaling() {
    let (source, expected) = parallel_allocating_source(8, 250_000);
    let serial = time_parallel_world(&source, 1, &expected);
    println!("LOOM\tcase\ttasks\tworkers\tserial_ms\tparallel_ms\tspeedup");
    for (workers, gate) in [(4, 3.0), (8, 5.0)] {
        let parallel = time_parallel_world(&source, workers, &expected);
        let speedup = serial.as_secs_f64() / parallel.as_secs_f64();
        println!(
            "LOOM\tparallel_allocating\t8\t{workers}\t{:.3}\t{:.3}\t{speedup:.3}",
            serial.as_secs_f64() * 1e3,
            parallel.as_secs_f64() * 1e3
        );
        assert!(
            speedup >= gate,
            "eight allocating tasks reached {speedup:.3}x on {workers} workers"
        );
    }
}

#[test]
#[ignore]
fn bench_parallel_allocation_churn() {
    let (source, expected) = parallel_churn_source(8, 250_000);
    let serial = time_parallel_world(&source, 1, &expected);
    let parallel = time_parallel_world(&source, 8, &expected);
    let speedup = serial.as_secs_f64() / parallel.as_secs_f64();
    println!("LOOM\tcase\ttasks\tworkers\tserial_ms\tparallel_ms\tspeedup");
    println!(
        "LOOM\tparallel_allocation_churn\t8\t8\t{:.3}\t{:.3}\t{speedup:.3}",
        serial.as_secs_f64() * 1e3,
        parallel.as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tparallel_counters\tcase\tworkers\tproc_slices\tcontinuations\trotations\trecalls\tquiescence\tcollection_quiescence\tinstructions\theap_growth\tnative_calls\tcollections\tclose_hits\tclose_misses\tderive_hits\tderive_misses"
    );
    report_parallel_counters("allocation_churn", &source, 1, &expected);
    report_parallel_counters("allocation_churn", &source, 8, &expected);
    let (steady_source, steady_expected) = parallel_allocating_source(8, 250_000);
    report_parallel_counters("steady_allocation", &steady_source, 1, &steady_expected);
    report_parallel_counters("steady_allocation", &steady_source, 8, &steady_expected);
    assert!(
        speedup >= 5.0,
        "eight churn tasks reached {speedup:.3}x on eight workers"
    );
}

#[test]
#[ignore]
fn bench_parallel_split_queens() {
    let (source, expected) = parallel_queens_source(12);
    let serial = time_parallel_world(&source, 1, &expected);
    let parallel = time_parallel_world(&source, 12, &expected);
    let speedup = serial.as_secs_f64() / parallel.as_secs_f64();
    println!("LOOM\tcase\ttasks\tworkers\tserial_ms\tparallel_ms\tspeedup");
    println!(
        "LOOM\tparallel_split_queens\t12\t12\t{:.3}\t{:.3}\t{speedup:.3}",
        serial.as_secs_f64() * 1e3,
        parallel.as_secs_f64() * 1e3
    );
}

#[test]
#[ignore]
fn bench_parallel_par_map_queens() {
    let (manual, expected) = manual_par_map_queens_source(13);
    let (library, library_expected) = iterable_queens_source(13, true);
    assert_eq!(library_expected, expected);
    println!("LOOM\tcase\tworkers\tmanual_ms\tpar_map_ms\tratio");
    for workers in [4, 12] {
        let manual_time = time_parallel_world(&manual, workers, &expected);
        let library_time = time_parallel_world(&library, workers, &expected);
        let ratio = library_time.as_secs_f64() / manual_time.as_secs_f64();
        println!(
            "LOOM\tpar_map_queens\t{workers}\t{:.3}\t{:.3}\t{ratio:.3}",
            manual_time.as_secs_f64() * 1e3,
            library_time.as_secs_f64() * 1e3
        );
        assert!(
            ratio <= 1.08,
            "par_map took {ratio:.3} times the manual implementation"
        );
    }

    let (sequential, sequential_expected) = iterable_queens_source(13, false);
    let map_time = time_world(&sequential, &[], config(), &sequential_expected);
    let par_map_time = time_world(&library, &["Proc"], config(), &expected);
    let ratio = par_map_time.as_secs_f64() / map_time.as_secs_f64();
    println!(
        "LOOM\tpar_map_deterministic\t1\t{:.3}\t{:.3}\t{ratio:.3}",
        map_time.as_secs_f64() * 1e3,
        par_map_time.as_secs_f64() * 1e3
    );
    assert!(
        ratio <= 1.08,
        "deterministic par_map took {ratio:.3} times sequential map"
    );
}

#[test]
#[ignore]
fn bench_parallel_messages() {
    println!(
        "LOOM\tgroup\tcase\tmessages\tworkers\tdeterministic_ms\t\
         deterministic_p95_ms\tparallel_ms\tparallel_p95_ms\tratio"
    );

    let mut deterministic_total = Duration::ZERO;
    let mut parallel_total = Duration::ZERO;
    let mut measured = 0;
    let mut record = |result: (Duration, Duration)| {
        deterministic_total += result.0;
        parallel_total += result.1;
        measured += 1;
    };

    let (ping, ping_expected, ping_messages) = parallel_ping_source(1, 2_000);
    if selected("ping_pong") {
        record(report_message_case(
            "ping_pong",
            ping_messages,
            &ping,
            &ping_expected,
        ));
    }

    let stream = r#"
class StreamSink < Proc[Int]
  def on_spawn(self): Int with Proc
    total = 0
    loop do
      case self.receive()
      in Msg(value)
        total = total + value
      in Closed
        return total
      end
    end
  end
end

sink = StreamSink.spawn()
i = 0
while i < 500
  sink.send(1)
  i = i + 1
end
sink.close()
sink.done()
"#;
    if selected("stream") {
        record(report_message_case("stream", 500, stream, "Done(Ok(500))"));
    }

    let (pairs, pairs_expected, pair_messages) = parallel_ping_source(4, 500);
    if selected("independent_pairs") {
        record(report_message_case(
            "independent_pairs",
            pair_messages,
            &pairs,
            &pairs_expected,
        ));
    }

    let many_senders = r#"
class ManySink < Proc[Int]
  def on_spawn(self): Int with Proc
    total = 0
    loop do
      case self.receive()
      in Msg(value)
        total = total + value
      in Closed
        return total
      end
    end
  end
end

class ManySender < Proc
  sink: Handle[Int, Int]

  def init(mut self, sink: Handle[Int, Int])
    self.sink = sink
  end

  def on_spawn(self): Int with Proc
    i = 0
    while i < 100
      self.sink.send(1)
      i = i + 1
    end
    i
  end
end

sink = ManySink.spawn()
s0 = ManySender.spawn(sink)
s1 = ManySender.spawn(sink)
s2 = ManySender.spawn(sink)
s3 = ManySender.spawn(sink)
s4 = ManySender.spawn(sink)
s5 = ManySender.spawn(sink)
s6 = ManySender.spawn(sink)
s7 = ManySender.spawn(sink)
s0.done()
s1.done()
s2.done()
s3.done()
s4.done()
s5.done()
s6.done()
s7.done()
sink.close()
sink.done()
"#;
    if selected("many_senders") {
        record(report_message_case(
            "many_senders",
            800,
            many_senders,
            "Done(Ok(800))",
        ));
    }

    let allocated = r#"
class PayloadSink < Proc[[Int]]
  def on_spawn(self): Int with Proc
    total = 0
    loop do
      case self.receive()
      in Msg(values)
        total = total + values.len()
      in Closed
        return total
      end
    end
  end
end

payload = list_repeated[Int](7, 32).freeze()
sink = PayloadSink.spawn()
i = 0
while i < 200
  sink.send(payload)
  i = i + 1
end
sink.close()
sink.done()
"#;
    if selected("allocated_stream") {
        record(report_message_case(
            "allocated_stream",
            200,
            allocated,
            "Done(Ok(6400))",
        ));
    }

    if measured > 0 {
        let aggregate = deterministic_total.as_secs_f64() / parallel_total.as_secs_f64();
        assert!(
            aggregate >= 0.95,
            "message throughput reached {aggregate:.3}x in aggregate"
        );
    }
}
