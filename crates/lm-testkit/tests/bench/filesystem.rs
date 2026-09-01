use super::*;

// ---------------------------------------------------------------
// Group 5: filesystem effects.
//
// Each case uses `CliHost`.
// This host matches `lm run`.
// File operations use worker threads.
// Each case warms the page cache before measurement.
// ---------------------------------------------------------------

/// The scratch directory for one filesystem case.
struct FsTree {
    root: std::path::PathBuf,
}

impl FsTree {
    fn new(label: &str) -> FsTree {
        let root = std::env::temp_dir().join(format!("lm-fs-bench-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("the scratch directory is created");
        FsTree { root }
    }

    /// Write one file of `bytes` filler and return its path as text.
    fn file(&self, name: &str, bytes: usize) -> String {
        let path = self.root.join(name);
        std::fs::write(&path, vec![b'x'; bytes]).expect("the scratch file is written");
        path.display().to_string()
    }

    fn path(&self, name: &str) -> String {
        self.root.join(name).display().to_string()
    }
}

impl Drop for FsTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Time one filesystem program under the command-line host.
fn time_fs(source: &str, expected: &str) -> Duration {
    let bytes = lm_testkit::compile_to_bytes("fs-bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let mut runs: Vec<Duration> = Vec::with_capacity(ROUNDS);
    for round in 0..=ROUNDS {
        let (arena, namespace) =
            lm_testkit::publish_artifact_bytes(&bytes).expect("the benchmark artifact must load");
        let host = Box::new(lm_host::CliHost::new(1));
        let mut world = lm_vm::World::new(arena, namespace, config(), host);
        world.allow("Fs").expect("the Fs grant exists");
        let start = Instant::now();
        let outcome = lm_proc::run_world(&mut world);
        let elapsed = start.elapsed();
        assert_eq!(
            world.show_outcome(&outcome),
            expected,
            "the case answered wrong"
        );
        if round > 0 {
            runs.push(elapsed);
        }
    }
    median(runs)
}

/// Time one filesystem program against the in-memory host.
///
/// `RecordingHost` defers a reply to a later poll exactly as
/// `CliHost` does, and it makes no system call and starts no worker
/// thread. The difference between the two hosts is therefore the cost
/// of the call and the thread, and what remains is the effect
/// boundary itself.
fn time_fs_memory(source: &str, file: &str, bytes: usize, expected: &str) -> Duration {
    let artifact = lm_testkit::compile_to_bytes("fs-bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let mut runs: Vec<Duration> = Vec::with_capacity(ROUNDS);
    for round in 0..=ROUNDS {
        let (arena, namespace) = lm_testkit::publish_artifact_bytes(&artifact)
            .expect("the benchmark artifact must load");
        let host = Rc::new(RefCell::new(lm_vm::RecordingHost::new(1)));
        host.borrow_mut().set_file(file, vec![b'x'; bytes]);
        let mut world = lm_vm::World::new(arena, namespace, config(), Box::new(host));
        world.allow("Fs").expect("the Fs grant exists");
        let start = Instant::now();
        let outcome = lm_proc::run_world(&mut world);
        let elapsed = start.elapsed();
        assert_eq!(
            world.show_outcome(&outcome),
            expected,
            "the case answered wrong"
        );
        if round > 0 {
            runs.push(elapsed);
        }
    }
    median(runs)
}

/// Report one case as throughput in mebibytes per second.
fn report_fs_throughput(name: &str, bytes: u64, source: &str, expected: &str) {
    let total = time_fs(source, expected);
    let mib = bytes as f64 / (1024.0 * 1024.0);
    println!(
        "LOOM\t{name}\t{bytes}\t{:.0}\t{:.3}",
        mib / total.as_secs_f64(),
        total.as_secs_f64() * 1e3
    );
}

/// Drop the page cache for one file. `posix_fadvise` needs no root.
///
/// A benchmark that writes a file and reads it back measures the page
/// cache and not the filesystem. The cold case evicts first, so the
/// device takes part.
fn evict_page_cache(path: &str) {
    let script = format!(
        "import os\nfd = os.open({path:?}, os.O_RDONLY)\n\
         os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)\nos.close(fd)\n"
    );
    let status = std::process::Command::new("python3")
        .arg("-c")
        .arg(script)
        .status()
        .expect("the eviction helper runs");
    assert!(status.success(), "the eviction helper failed");
}

/// Report one throughput case that evicts the page cache each round.
fn report_fs_cold(name: &str, bytes: u64, path: &str, source: &str, expected: &str) {
    let artifact = lm_testkit::compile_to_bytes("fs-bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let mut runs: Vec<Duration> = Vec::with_capacity(ROUNDS);
    for round in 0..=3 {
        evict_page_cache(path);
        let (arena, namespace) = lm_testkit::publish_artifact_bytes(&artifact)
            .expect("the benchmark artifact must load");
        let host = Box::new(lm_host::CliHost::new(1));
        let mut world = lm_vm::World::new(arena, namespace, config(), host);
        world.allow("Fs").expect("the Fs grant exists");
        let start = Instant::now();
        let outcome = lm_proc::run_world(&mut world);
        let elapsed = start.elapsed();
        assert_eq!(
            world.show_outcome(&outcome),
            expected,
            "the case answered wrong"
        );
        if round > 0 {
            runs.push(elapsed);
        }
    }
    let total = median(runs);
    println!(
        "LOOM\t{name}\t{bytes}\t{:.0}\t{:.3}",
        bytes as f64 / (1024.0 * 1024.0) / total.as_secs_f64(),
        total.as_secs_f64() * 1e3
    );
}

fn report_fs_memory(
    name: &str,
    iterations: u64,
    source: &str,
    file: &str,
    bytes: usize,
    expected: &str,
) {
    let total = time_fs_memory(source, file, bytes, expected);
    println!(
        "LOOM\t{name}\t{iterations}\t{:.0}\t{:.3}",
        total.as_nanos() as f64 / iterations as f64,
        total.as_secs_f64() * 1e3
    );
}

fn report_fs(name: &str, iterations: u64, source: &str, expected: &str) {
    let total = time_fs(source, expected);
    let per = total.as_nanos() as f64 / iterations as f64;
    println!(
        "LOOM\t{name}\t{iterations}\t{:.0}\t{:.3}",
        per,
        total.as_secs_f64() * 1e3
    );
}

/// The buffered line reader under test.
const READER: &str = r#"# A buffered line reader written in ordinary Loom code.
#
# The buffer is one Bytes value. A line is a slice of it, so a hit
# copies nothing and crosses no effect boundary. Only a refill
# performs `Fs.Read`, and the row says so.

class BufReader
  file: FileHandle
  buffer: Bytes
  eof: Bool

  def init(mut self, file: FileHandle)
    self.file = file
    self.buffer = "".bytes()
    self.eof = false
  end

  def read_line(mut self): Option[Bytes] with Fs.Read
    nl = "\n".bytes()
    out: Option[Bytes] = None
    going = true
    while going
      case self.buffer.find(nl)
      in Some(at)
        line = case self.buffer.slice(0, at) in Ok(b) then b in Err(_) then self.buffer end
        tail = self.buffer.len() - at - 1
        self.buffer = case self.buffer.slice(at + 1, tail) in Ok(b) then b in Err(_) then self.buffer end
        out = Some(line)
        going = false
      in None
        if self.eof
          if not self.buffer.is_empty()
            out = Some(self.buffer)
            self.buffer = "".bytes()
          end
          going = false
        else
          case self.file.read(65536)
          in Ok(chunk)
            if chunk.is_empty()
              self.eof = true
            else
              self.buffer = self.buffer + chunk
            end
          in Err(_)
            self.eof = true
          end
        end
      end
    end
    out
  end
end

def count_lines(path: String): Int with Fs.Open, Fs.Read, Fs.Close
  case sys.fs.open(path, ReadOnly)
  in Ok(f)
    r = BufReader(f)
    n = 0
    going = true
    while going
      case r.read_line()
      in Some(_)
        n = n + 1
      in None
        going = false
      end
    end
    f.close()
    n
  in Err(_) then -1
  end
end

"#;

#[test]
#[ignore]
fn bench_filesystem_operations() {
    println!("LOOM\tcase\titers\tns_per_op\ttotal_ms");
    let tree = FsTree::new("read");
    let data = tree.file("data.bin", 8 * 1024 * 1024);

    // The handle lifecycle alone: one open and one close.
    report_fs(
        "fs_open_close",
        2_000,
        &format!(
            "def go(): Int with Fs.Open, Fs.Close\n\
             \x20 n = 0\n  i = 0\n  while i < 2000\n\
             \x20   n = n + case sys.fs.open(\"{data}\", ReadOnly)\n\
             \x20   in Ok(f)\n      case f.close() in Ok(_) then 1 in Err(_) then 0 end\n\
             \x20   in Err(_) then 0\n    end\n    i = i + 1\n  end\n  n\nend\ngo()\n"
        ),
        "Done(2000)",
    );

    // One read of 1 KiB from an open handle. The file is large
    // enough that no read reaches its end.
    report_fs(
        "fs_read_1k",
        2_000,
        &format!(
            "def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 case sys.fs.open(\"{data}\", ReadOnly)\n\
             \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 2000\n\
             \x20     n = n + case f.read(1024) in Ok(b) then b.len() in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n"
        ),
        "Done(2048000)",
    );

    // The same read at 64 KiB. The call does more work and the
    // boundary costs the same, so the ratio moves.
    report_fs(
        "fs_read_64k",
        100,
        &format!(
            "def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 case sys.fs.open(\"{data}\", ReadOnly)\n\
             \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 100\n\
             \x20     n = n + case f.read(65536) in Ok(b) then b.len() in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n"
        ),
        "Done(6553600)",
    );

    // Read one whole small file: open, one read, close. This is the
    // shape a program writes most often.
    let small = tree.file("small.txt", 4096);
    report_fs(
        "fs_read_file",
        1_000,
        &format!(
            "def once(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 case sys.fs.open(\"{small}\", ReadOnly)\n\
             \x20 in Ok(f)\n    n = case f.read(8192) in Ok(b) then b.len() in Err(_) then 0 end\n\
             \x20   f.close()\n    n\n  in Err(_) then 0\n  end\nend\n\
             def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 n = 0\n  i = 0\n  while i < 1000\n    n = n + once()\n    i = i + 1\n  end\n  n\nend\ngo()\n"
        ),
        "Done(4096000)",
    );

    // One write of 1 KiB to an open handle.
    let out = tree.path("out.bin");
    report_fs(
        "fs_write_1k",
        2_000,
        &format!(
            "def go(): Int with Fs.Open, Fs.Write, Fs.Close\n\
             \x20 case sys.fs.open(\"{out}\", CreateTruncate)\n\
             \x20 in Ok(f)\n    chunk = \"{}\".bytes()\n    n = 0\n    i = 0\n    while i < 2000\n\
             \x20     n = n + case f.write(chunk) in Ok(w) then w in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n",
            "y".repeat(1024)
        ),
        "Done(2048000)",
    );

    // The same 1 KiB read against the in-memory host. No system call
    // and no worker thread run, so this is the effect boundary alone.
    report_fs_memory(
        "fs_read_1k_memory",
        2_000,
        "def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
         \x20 case sys.fs.open(\"mem.bin\", ReadOnly)\n\
         \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 2000\n\
         \x20     n = n + case f.read(1024) in Ok(b) then b.len() in Err(_) then 0 end\n\
         \x20     i = i + 1\n    end\n    f.close()\n    n\n\
         \x20 in Err(_) then 0\n  end\nend\ngo()\n",
        "mem.bin",
        8 * 1024 * 1024,
        "Done(2048000)",
    );

    // A buffered line reader written in ordinary Loom code. The
    // buffer is one Bytes value and a line is a slice of it, so a
    // buffer hit copies nothing and crosses no effect boundary. Only
    // a refill performs `Fs.Read`.
    let lines_path = tree.path("lines.txt");
    {
        let body = b"a short protocol line\n".repeat(200_000);
        std::fs::write(&lines_path, body).expect("the line file is written");
    }
    report_fs(
        "fs_read_lines",
        200_000,
        &format!("{}count_lines(\"{lines_path}\")\n", READER),
        "Done(200000)",
    );

    // The same reader with the line slice removed: one slice for
    // each line instead of two. The difference names what producing
    // one line value costs.
    report_fs(
        "fs_read_lines_advance",
        200_000,
        &format!(
            "{}count_lines(\"{lines_path}\")\n",
            READER.replace(
                "line = case self.buffer.slice(0, at) in Ok(b) then b in Err(_) then self.buffer end",
                "line = self.buffer"
            )
        ),
        "Done(200000)",
    );

    // Sequential throughput over a 64 MiB file, at two chunk sizes.
    // The unit is mebibytes per second, not nanoseconds.
    let big = tree.file("big.bin", 64 * 1024 * 1024);
    println!("LOOM\tcase\tbytes\tmib_per_s\ttotal_ms");
    report_fs_throughput(
        "fs_tput_read_64k",
        64 * 1024 * 1024,
        &format!(
            "def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 case sys.fs.open(\"{big}\", ReadOnly)\n\
             \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 1024\n\
             \x20     n = n + case f.read(65536) in Ok(b) then b.len() in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n"
        ),
        "Done(67108864)",
    );
    report_fs_throughput(
        "fs_tput_read_1m",
        64 * 1024 * 1024,
        &format!(
            "def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 case sys.fs.open(\"{big}\", ReadOnly)\n\
             \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 64\n\
             \x20     n = n + case f.read(1048576) in Ok(b) then b.len() in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n"
        ),
        "Done(67108864)",
    );
    let sink = tree.path("sink.bin");
    report_fs_throughput(
        "fs_tput_write_64k",
        64 * 1024 * 1024,
        &format!(
            "def go(): Int with Fs.Open, Fs.Read, Fs.Write, Fs.Close\n\
             \x20 chunk = case sys.fs.open(\"{big}\", ReadOnly)\n\
             \x20 in Ok(src)\n    c = case src.read(65536) in Ok(b) then b in Err(_) then \"\".bytes() end\n\
             \x20   src.close()\n    c\n  in Err(_) then \"\".bytes()\n  end\n\
             \x20 case sys.fs.open(\"{sink}\", CreateTruncate)\n\
             \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 1024\n\
             \x20     n = n + case f.write(chunk) in Ok(w) then w in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n"
        ),
        "Done(67108864)",
    );

    // The same read with the page cache evicted first. This is the
    // cost of loading a file, and the warm case above is the cost of
    // copying one out of memory.
    report_fs_cold(
        "fs_tput_read_cold",
        64 * 1024 * 1024,
        &big,
        &format!(
            "def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 case sys.fs.open(\"{big}\", ReadOnly)\n\
             \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 64\n\
             \x20     n = n + case f.read(1048576) in Ok(b) then b.len() in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n"
        ),
        "Done(67108864)",
    );

    // The handle lifecycle against the in-memory host.
    report_fs_memory(
        "fs_open_close_memory",
        2_000,
        "def go(): Int with Fs.Open, Fs.Close\n\
         \x20 n = 0\n  i = 0\n  while i < 2000\n\
         \x20   n = n + case sys.fs.open(\"mem.bin\", ReadOnly)\n\
         \x20   in Ok(f)\n      case f.close() in Ok(_) then 1 in Err(_) then 0 end\n\
         \x20   in Err(_) then 0\n    end\n    i = i + 1\n  end\n  n\nend\ngo()\n",
        "mem.bin",
        4096,
        "Done(2000)",
    );
}
