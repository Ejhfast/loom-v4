//! Concrete byte stream interface conformances.

use lm_testkit::{compile_to_bytes, run_world};
use lm_vm::VmConfig;

#[test]
fn every_byte_stream_satisfies_its_generic_contract() {
    let source = r#"
def read_once[effect e, R: ByteReader with e](reader: R): Result[Bytes, R.Error] with e
  reader.read(16)
end

def write_once[effect e, W: ByteWriter with e](writer: W): Result[Int, W.Error] with e
  writer.write(b"data")
end

def prove(
  file: FileHandle,
  tcp: TcpStream,
  tls: TlsStream,
  pipe_reader: PipeReader,
  pipe_writer: PipeWriter
) with Fs.Read, Fs.Write, Tcp.Read, Tcp.Write, Tls.Read, Tls.Write, Pipe.Read, Pipe.Write
  read_once(file)
  write_once(file)
  read_once(tcp)
  write_once(tcp)
  read_once(tls)
  write_once(tls)
  read_once(pipe_reader)
  write_once(pipe_writer)
end

1
"#;

    compile_to_bytes("stream-contracts.lm", source).expect("all stream conformances verify");
}

#[test]
fn a_byte_stream_error_must_implement_error() {
    let source = r#"
final class BadWriter implements ByteWriter
  type Error = Int

  def write(self, bytes: Bytes): Result[Int, Int]
    Ok(bytes.len())
  end
end

BadWriter()
"#;

    let error = compile_to_bytes("bad-stream-error.lm", source)
        .expect_err("the invalid stream error rejects");
    assert!(
        error.contains("associated type `Error` does not conform to `Error`"),
        "{error}"
    );
}

#[test]
fn standard_write_all_uses_file_and_pipe_conformances() {
    let source = r#"
use std.io.write_all_to

def go(): String with Fs, Pipe
  file = sys.fs.open(Path("saved.bin", PathStyle.Posix), CreateTruncate).expect("the file opens")
  write_all_to(file, b"file-data").expect("the file writes")
  file.close().expect("the file closes")

  pair = Pipe().open().expect("the pipe opens")
  reader = pair[0]
  writer = pair[1]
  write_all_to(writer, b"pipe-data").expect("the pipe writes")
  writer.close().expect("the writer closes")
  bytes = reader.read(32).expect("the pipe reads")
  reader.close().expect("the reader closes")
  bytes.text()
end

go()
"#;

    let (outcome, host) = run_world(
        "stream-write-all.lm",
        source,
        &["Fs", "Pipe"],
        VmConfig::default(),
    )
    .expect("the stream program runs");

    assert_eq!(outcome, "Done(\"pipe-data\")");
    assert_eq!(host.borrow().file("saved.bin"), Some(&b"file-data"[..]));
}
