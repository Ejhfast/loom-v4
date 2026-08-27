//! TLS effects, resources, and deterministic host behavior.

use lm_testkit::compile_to_bytes;
use lm_testkit::publish_artifact_bytes;
use lm_vm::{Object, RecordingHost, VmConfig, World};

const LOOPBACK: &str = r#"
use std.tls.Tls
use std.tls.TlsClientConfig
use std.tls.TlsRoots
use std.tls.TlsServerConfig
use std.tls.TlsVersion

def loopback(port: Int): Result[SocketAddress, NetError]
  bytes = ByteBuffer()
  bytes.append(127).append(0).append(0).append(1)
  Tcp().address(IpAddress.V4(bytes.finish()), port, 0, 0)
end

def test_tls_config(): TlsClientConfig
  TlsClientConfig(
    "localhost",
    TlsRoots.Custom([Bytes("test root")]),
    [Bytes("http/1.1")],
    TlsVersion.Tls13,
    65536
  ).freeze()
end
"#;

const HTTP_USES: &str = r#"use std.http.Http
use std.http.HttpLimits

"#;

const SERVER_USES: &str = r#"def test_tls_server_config(): TlsServerConfig
  TlsServerConfig(
    [Bytes("test certificate")],
    Bytes("test private key"),
    [Bytes("loom/1")],
    TlsVersion.Tls13,
    65536
  ).freeze()
end
"#;

fn run(source: &str, grants: &[&str]) -> (String, usize) {
    run_with_config(source, grants, VmConfig::default())
}

fn run_with_config(source: &str, grants: &[&str], config: VmConfig) -> (String, usize) {
    let bytes = compile_to_bytes("tls.lm", source).expect("the TLS program compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the TLS program loads");
    let mut world = World::new(arena, namespace, config, Box::new(RecordingHost::new(1)));
    for grant in grants {
        world.allow(grant).expect("the TLS grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    (world.show_outcome(&outcome), world.resource_count(0))
}

#[test]
fn compiled_tls_round_trips_through_the_virtual_host() {
    let source = format!(
        r#"{LOOPBACK}
def exchange(): String with Tcp, Tls
  address = case loopback(0)
  in Ok(value) then value
  in Err(error)
    return display(error)
  end
  listener = case Tcp().listen(address, 4)
  in Ok(value) then value
  in Err(error)
    return display(error)
  end
  bound = case listener.local_address()
  in Ok(value) then value
  in Err(error)
    return display(error)
  end
  client = case Tcp().connect(bound)
  in Ok(value) then value
  in Err(error)
    return display(error)
  end
  server = case listener.accept()
  in Ok(value) then value[0]
  in Err(error)
    return display(error)
  end
  secure = case Tls().handshake(client, test_tls_config())
  in Ok(value) then value
  in Err(error)
    return display(error)
  end
  case secure.write_all(Bytes("hello"))
  in Ok(_) then ()
  in Err(error)
    return display(error)
  end
  received = case server.read_exact(5)
  in Ok(value) then value.text()
  in Err(error) then display(error)
  end
  secure.shutdown()
  secure.close()
  server.close()
  listener.close()
  received
end

exchange()
"#
    );
    let (outcome, resources) = run(&source, &["Tcp", "Tls"]);
    assert_eq!(outcome, "Done(\"hello\")");
    assert_eq!(resources, 0);

    let tight = VmConfig {
        max_resources: 4,
        ..VmConfig::default()
    };
    let (outcome, resources) = run_with_config(&source, &["Tcp", "Tls"], tight);
    assert_eq!(outcome, "Done(\"hello\")");
    assert_eq!(resources, 0);
}

#[test]
fn compiled_tls_server_round_trips_through_the_virtual_host() {
    let source = format!(
        r#"{LOOPBACK}{SERVER_USES}
def exchange(): String with Tcp, Tls.Server
  address = loopback(0).expect("the loopback address is valid")
  listener = Tcp().listen(address, 4).expect("the listener opens")
  bound = listener.local_address().expect("the listener has an address")
  client = Tcp().connect(bound).expect("the client connects")
  server = listener.accept().expect("the server accepts")[0]
  client.write_all(Bytes("hello")).expect("the client writes")
  secure = Tls().server_handshake(server, test_tls_server_config()).expect("the handshake works")
  received = secure.read_exact(5).expect("the server reads").text()
  secure.close().expect("the TLS stream closes")
  client.close().expect("the client closes")
  listener.close().expect("the listener closes")
  received
end

exchange()
"#
    );
    let (outcome, resources) = run(&source, &["Tcp", "Tls.Server"]);
    assert_eq!(outcome, "Done(\"hello\")");
    assert_eq!(resources, 0);
}

#[test]
fn a_tls_server_error_consumes_the_submitted_tcp_stream() {
    let source = format!(
        r#"{LOOPBACK}
def exchange(): (Bool, Bool) with Tcp, Tls.ServerHandshake
  address = loopback(0).expect("the loopback address is valid")
  listener = Tcp().listen(address, 4).expect("the listener opens")
  bound = listener.local_address().expect("the listener has an address")
  client = Tcp().connect(bound).expect("the client connects")
  server = listener.accept().expect("the server accepts")[0]
  invalid = TlsServerConfig([], Bytes("key"), [], TlsVersion.Tls13, 65536).freeze()
  rejected = case Tls().server_handshake(server, invalid)
  in Err(TlsError.InvalidConfig(_)) then true
  in _ then false
  end
  consumed = case server.close()
  in Err(NetError.Closed) then true
  in _ then false
  end
  client.close().expect("the client closes")
  listener.close().expect("the listener closes")
  (rejected, consumed)
end

exchange()
"#
    );
    let (outcome, resources) = run(&source, &["Tcp", "Tls.ServerHandshake"]);
    assert_eq!(outcome, "Done((true, true))");
    assert_eq!(resources, 0);
}

#[test]
fn tls_response_reading_uses_the_shared_http_parser() {
    let source = format!(
        r#"{HTTP_USES}{LOOPBACK}
def exchange(): String with Tcp, Tls
  address = case loopback(0)
  in Ok(value) then value
  in Err(error)
    return display(error)
  end
  listener = case Tcp().listen(address, 4)
  in Ok(value) then value
  in Err(error)
    return display(error)
  end
  bound = case listener.local_address()
  in Ok(value) then value
  in Err(error)
    return display(error)
  end
  client = case Tcp().connect(bound)
  in Ok(value) then value
  in Err(error)
    return display(error)
  end
  server = case listener.accept()
  in Ok(value) then value[0]
  in Err(error)
    return display(error)
  end
  secure = case Tls().handshake(client, test_tls_config())
  in Ok(value) then value
  in Err(error)
    return display(error)
  end
  wire = Bytes("HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\npong")
  case server.write_all(wire)
  in Ok(_) then ()
  in Err(error)
    return display(error)
  end
  response = case Http().read_tls_response(
    secure,
    "GET",
    HttpLimits(256, 16, 128, 16, 512, 3)
  )
  in Ok(value) then value
  in Err(error)
    return display(error)
  end
  secure.close()
  server.close()
  listener.close()
  response.body.text()
end

exchange()
"#
    );
    let (outcome, resources) = run(&source, &["Tcp", "Tls"]);
    assert_eq!(outcome, "Done(\"pong\")");
    assert_eq!(resources, 0);
}

#[test]
fn a_live_tls_stream_blocks_a_self_snapshot() {
    let source = format!(
        r#"{LOOPBACK}
def capture(): String with Tcp, Tls, Vm
  address = case loopback(0)
  in Ok(value) then value
  in Err(_)
    return "address failed"
  end
  listener = case Tcp().listen(address, 4)
  in Ok(value) then value
  in Err(_)
    return "listen failed"
  end
  bound = case listener.local_address()
  in Ok(value) then value
  in Err(_)
    return "address failed"
  end
  client = case Tcp().connect(bound)
  in Ok(value) then value
  in Err(_)
    return "connect failed"
  end
  server = case listener.accept()
  in Ok(value) then value[0]
  in Err(_)
    return "accept failed"
  end
  secure = case Tls().handshake(client, test_tls_config())
  in Ok(value) then value
  in Err(_)
    return "handshake failed"
  end
  server.close()
  listener.close()
  kind = case sys.vm.snapshot_self()
  in Ok(_) then "captured"
  in Err(ResourceActive(_, value)) then value
  in Err(_) then "wrong error"
  end
  secure.close()
  kind
end

capture()
"#
    );
    assert_eq!(
        run(&source, &["Tcp", "Tls", "Vm"]).0,
        "Done(\"TLS stream\")"
    );
}

#[test]
fn a_restored_closed_tls_stream_stays_closed() {
    let source = format!(
        r#"{LOOPBACK}
def capture(): String with Tcp, Tls, Vm
  address = case loopback(0)
  in Ok(value) then value
  in Err(_)
    return "address failed"
  end
  listener = case Tcp().listen(address, 4)
  in Ok(value) then value
  in Err(_)
    return "listen failed"
  end
  bound = case listener.local_address()
  in Ok(value) then value
  in Err(_)
    return "address failed"
  end
  client = case Tcp().connect(bound)
  in Ok(value) then value
  in Err(_)
    return "connect failed"
  end
  server = case listener.accept()
  in Ok(value) then value[0]
  in Err(_)
    return "accept failed"
  end
  secure = case Tls().handshake(client, test_tls_config())
  in Ok(value) then value
  in Err(_)
    return "handshake failed"
  end
  secure.close()
  server.close()
  listener.close()
  case sys.vm.snapshot_self()
  in Err(_)
    return "capture failed"
  in Ok(_) then ()
  end
  case secure.read(1)
  in Err(Closed) then "closed"
  in Err(_) then "wrong error"
  in Ok(_) then "read succeeded"
  end
end

capture()
"#
    );
    let bytes = compile_to_bytes("closed_tls.lm", &source).expect("the TLS program compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the TLS program loads");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    for grant in ["Tcp", "Tls", "Vm"] {
        world.allow(grant).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(\"closed\")");
    let image = world
        .last_snapshot()
        .expect("the program captured a snapshot")
        .clone();
    assert!(image.world().machines.iter().any(|machine| {
        machine
            .objects
            .iter()
            .any(|entry| matches!(entry.object, Object::NativeTlsStream { resource: 0 }))
    }));

    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the TLS program reloads");
    let mut fresh = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let target = fresh.new_child(0).expect("the restore target exists");
    let restored = fresh
        .restore_image(0, target, &image)
        .expect("the TLS snapshot restores");
    let barrier = fresh.next_gate();
    let recaptured = fresh
        .capture_snapshot(barrier, restored, false)
        .expect("the restored closed TLS stream captures");
    assert!(recaptured.world().machines.iter().any(|machine| {
        machine
            .objects
            .iter()
            .any(|entry| matches!(entry.object, Object::NativeTlsStream { resource: 0 }))
    }));
}

#[test]
fn a_driver_can_upgrade_and_service_a_tls_stream() {
    let source = format!(
        r#"{LOOPBACK}
def client(address: SocketAddress): Int with Tcp.Connect, Tls.Handshake, Tls.Write, Tls.Close
  stream = case Tcp().connect(address)
  in Ok(value) then value
  in Err(_)
    return -1
  end
  secure = case Tls().handshake(stream, test_tls_config())
  in Ok(value) then value
  in Err(_)
    return -2
  end
  written = case secure.write(Bytes("hello"))
  in Ok(value) then value
  in Err(_)
    return -3
  end
  secure.close()
  written
end

def finish(child: Run[Int], mine: ResourceHandle): Int with Vm
  loop do
    case child.drive()
    in Asked(request)
      case request
      in Call(Tls.Write, call, (stream, bytes))
        if child.resource(stream).same_resource(mine)
          child.answer(call, Ok(bytes.len()))
        else
          child.dispatch(request)
        end
      in Call(Tls.Close, call, (stream,))
        if child.resource(stream).same_resource(mine)
          child.answer(call, Ok(()))
        else
          child.dispatch(request)
        end
      in _
        child.dispatch(request)
      end
    in Done(value)
      if mine.is_open()
        return -4
      end
      return value
    in Fault(_)
      return -5
    end
  end
end

def upgrade(child: Run[Int], tcp: ResourceHandle): Int with Vm
  case child.drive()
  in Asked(request)
    case request
    in Call(Tls.Handshake, call, (stream, _, _, _, _, _, _))
      if not child.resource(stream).same_resource(tcp)
        return -6
      end
      secure = child.serve_tls_stream(call)
      if tcp.is_open()
        return -7
      end
      finish(child, secure)
    in _ then -8
    end
  in Done(_)  then -9
  in Fault(_) then -10
  end
end

case loopback(8443)
in Err(_) then -11
in Ok(address)
  child = sys.vm.Vm().activate_or_fault(client, args: (address,))
  case child.drive()
  in Asked(request)
    case request
    in Call(Tcp.Connect, call, (peer,))
      tcp = child.serve_tcp_stream(call, peer)
      upgrade(child, tcp)
    in _ then -12
    end
  in Done(_)  then -13
  in Fault(_) then -14
  end
end
"#
    );
    assert_eq!(run(&source, &["Vm"]).0, "Done(5)");
}

#[test]
fn a_driver_can_service_a_tls_server_handshake() {
    let source = format!(
        r#"{LOOPBACK}{SERVER_USES}
def server(address: SocketAddress): Int with Tcp.Connect, Tls.ServerHandshake, Tls.Close
  stream = Tcp().connect(address).expect("the stream opens")
  secure = Tls().server_handshake(stream, test_tls_server_config()).expect("the handshake works")
  secure.close().expect("the TLS stream closes")
  7
end

def finish_server(child: Run[Int], secure: ResourceHandle): Int with Vm
  case child.drive()
  in Asked(request)
    case request
    in Call(Tls.Close, call, (stream,))
      if not child.resource(stream).same_resource(secure)
        return -1
      end
      child.answer(call, Ok(()))
      case child.run()
      in Ok(value) then value
      in _ then -2
      end
    in _ then -3
    end
  in _ then -4
  end
end

def upgrade_server(child: Run[Int], tcp: ResourceHandle): Int with Vm
  case child.drive()
  in Asked(request)
    case request
    in Call(Tls.ServerHandshake, call, (stream, _, _, _, _, _))
      if not child.resource(stream).same_resource(tcp)
        return -5
      end
      secure = child.serve_tls_stream(call)
      if tcp.is_open()
        return -6
      end
      finish_server(child, secure)
    in _ then -7
    end
  in _ then -8
  end
end

address = loopback(8443).expect("the loopback address is valid")
child = sys.vm.Vm().activate_or_fault(server, args: (address,))
case child.drive()
in Asked(request)
  case request
  in Call(Tcp.Connect, call, (peer,))
    tcp = child.serve_tcp_stream(call, peer)
    upgrade_server(child, tcp)
  in _ then -9
  end
in _ then -10
end
"#
    );
    assert_eq!(run(&source, &["Vm"]).0, "Done(7)");
}
