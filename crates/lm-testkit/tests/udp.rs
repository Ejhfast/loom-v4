//! UDP effects, datagram waits, and resource behavior.

use lm_testkit::{compile_text, compile_to_bytes, repo_root};
use lm_vm::{load_bytes, RecordingHost, VmConfig, World};

const LOOPBACK: &str = r#"
def loopback(port: Int): SocketAddress
  bytes = ByteBuffer()
  bytes.append(127)
  bytes.append(0)
  bytes.append(0)
  bytes.append(1)
  Tcp().address(IpAddress.V4(bytes.finish()), port, 0, 0).expect("the address is valid")
end
"#;

fn run(source: &str, grants: &[&str], real: bool) -> (String, usize) {
    let bytes = compile_to_bytes("udp.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let host: Box<dyn lm_vm::Host> = if real {
        Box::new(lm_host::CliHost::new(1))
    } else {
        Box::new(RecordingHost::new(1))
    };
    let mut world = World::new(&loaded, VmConfig::default(), host);
    for grant in grants {
        world.allow(grant).expect("the UDP grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    (world.show_outcome(&outcome), world.resource_count(0))
}

#[test]
fn udp_round_trips_complete_datagrams_through_both_hosts() {
    let source = format!(
        r#"{LOOPBACK}
def exchange(): (String, Bool, Int) with Udp
  first = Udp().bind(loopback(0)).expect("the first socket binds")
  second = Udp().bind(loopback(0)).expect("the second socket binds")
  first_address = first.local_address().expect("the first address exists")
  second_address = second.local_address().expect("the second address exists")
  first.send_to(second_address, b"hello").expect("the datagram sends")
  datagram = second.recv_from().expect("the datagram arrives")
  first.send_to(second_address, b"").expect("the empty datagram sends")
  empty = second.recv_from().expect("the empty datagram arrives")
  answer = (
    datagram.data.text(),
    datagram.peer.port == first_address.port,
    empty.data.len()
  )
  first.close().expect("the first socket closes")
  second.close().expect("the second socket closes")
  answer
end
exchange()
"#
    );

    for real in [false, true] {
        let (outcome, resources) = run(&source, &["Udp"], real);
        assert_eq!(outcome, "Done((\"hello\", true, 0))");
        assert_eq!(resources, 0);
    }
}

#[test]
fn a_losing_receive_wait_preserves_one_complete_datagram() {
    let source = format!(
        r#"{LOOPBACK}
def choose_datagram(): (String, String, String) with Udp, Clock, Wait
  sender = Udp().bind(loopback(0)).expect("the sender binds")
  receiver = Udp().bind(loopback(0)).expect("the receiver binds")
  address = receiver.local_address().expect("the receiver address exists")
  sender.send_to(address, b"kept").expect("the first datagram sends")
  loser = select
  in sys.clock.sleep.wait(0) -> _
    "timer"
  in receiver.recv_from_wait() -> _
    "UDP"
  end
  kept = receiver.recv_from().expect("the first datagram remains")
  sender.send_to(address, b"winner").expect("the second datagram sends")
  winner = select
  in receiver.recv_from_wait() -> reply
    reply.expect("the UDP source completes").data.text()
  in sys.clock.sleep.wait(0) -> _
    "timer"
  end
  sender.close().expect("the sender closes")
  receiver.close().expect("the receiver closes")
  (loser, kept.data.text(), winner)
end
choose_datagram()
"#
    );

    assert_eq!(
        run(&source, &["Udp", "Clock", "Wait"], false).0,
        "Done((\"timer\", \"kept\", \"winner\"))"
    );
}

#[test]
fn a_live_udp_socket_blocks_capture_until_close() {
    let source = format!(
        r#"{LOOPBACK}
def capture(): (String, Bool, String) with Udp, Vm
  socket = Udp().bind(loopback(0)).expect("the socket binds")
  alias = socket
  blocker = case sys.vm.snapshot_self()
  in Err(SnapshotError.ResourceActive(_, name)) then name
  in Err(error) then display(error)
  in Ok(_) then "no blocker"
  end
  socket.close().expect("the socket closes")
  closed = case alias.recv_from()
  in Err(NetError.Closed) then "closed"
  in Err(error) then display(error)
  in Ok(_) then "open"
  end
  captured = sys.vm.snapshot_self().is_ok()
  (blocker, captured, closed)
end
capture()
"#
    );

    let (outcome, resources) = run(&source, &["Udp", "Vm"], false);
    assert_eq!(outcome, "Done((\"UDP socket\", true, \"closed\"))");
    assert_eq!(resources, 0);
}

#[test]
fn the_udp_socket_effect_set_does_not_grant_bind() {
    let source = format!(
        r#"{LOOPBACK}
Udp().bind(loopback(0))
"#
    );

    assert_eq!(
        run(&source, &["Udp.Socket"], false).0,
        "Fault(PolicyDenied)"
    );
}

#[test]
fn a_machine_fault_closes_its_udp_socket() {
    let source = format!(
        r#"{LOOPBACK}
def stop(): Never with Udp
  Udp().bind(loopback(0)).expect("the socket binds")
  panic("stop")
end
stop()
"#
    );

    let (outcome, resources) = run(&source, &["Udp"], false);
    assert_eq!(outcome, "Fault(UserPanic)");
    assert_eq!(resources, 0);
}

#[test]
fn the_checker_rejects_a_wrong_udp_bind_argument() {
    let error =
        compile_to_bytes("bad-udp.lm", "Udp().bind(1)\n").expect_err("an integer address rejects");
    assert!(
        error.contains("expected SocketAddress, found Int"),
        "{error}"
    );
}

#[test]
fn udp_rejects_a_datagram_above_the_portable_limit() {
    let source = format!(
        r#"{LOOPBACK}
def send_large(): String with Udp
  socket = Udp().bind(loopback(0)).expect("the socket binds")
  buffer = ByteBuffer()
  for _ in Range(0, 65536)
    buffer.append(0)
  end
  answer = case socket.send_to(loopback(9), buffer.finish())
  in Err(NetError.LimitExceeded(_)) then "limited"
  in Err(error) then display(error)
  in Ok(_) then "sent"
  end
  socket.close().expect("the socket closes")
  answer
end
send_large()
"#
    );

    for real in [false, true] {
        assert_eq!(run(&source, &["Udp"], real).0, "Done(\"limited\")");
    }
}

#[test]
fn the_verifier_rejects_a_forged_udp_datagram_role() {
    let mut module = compile_text("udp-role.lm", "1\n").expect("the program compiles");
    let role = lm_bytecode::corepin::role_index("UdpDatagram").expect("the role exists");
    let class = module.core_roles[role];
    module.classes[class as usize].fields.pop();

    let error = lm_verify::verify_module(&module).expect_err("the forged role rejects");
    assert!(
        error
            .message
            .contains("the UdpDatagram role does not name its final value class"),
        "{error}"
    );
}

#[test]
fn the_udp_example_has_checked_output() {
    let path = repo_root().join("examples/12-network-effects/09-udp-datagrams.lm");
    let source = std::fs::read_to_string(path).expect("the example reads");
    assert_eq!(run(&source, &["Udp"], false).0, "Done((\"one packet\", 0))");
}
