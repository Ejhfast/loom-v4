//! Network effects, TCP handles, and deterministic host behavior.

use lm_testkit::compile_to_bytes;
use lm_vm::{load_bytes, RecordingHost, VmConfig, World};

const LOOPBACK: &str = r#"
def loopback(port: Int): Result[SocketAddress, NetError]
  bytes = ByteBuffer()
  bytes.append(127)
  bytes.append(0)
  bytes.append(0)
  bytes.append(1)
  Tcp().address(IpAddress.V4(bytes.finish()), port, 0, 0)
end
"#;

fn run(source: &str, grants: &[&str]) -> (String, usize) {
    let bytes = compile_to_bytes("network.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    for grant in grants {
        world.allow(grant).expect("the network grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    (world.show_outcome(&outcome), world.resource_count(0))
}

#[test]
fn compiled_tcp_round_trips_through_the_virtual_host() {
    let source = format!(
        r#"{LOOPBACK}
def exchange(): String with Tcp
  address = case loopback(0)
  in Ok(value)  then value
  in Err(error)
    return error.message()
  end
  listener = case Tcp().listen(address, 8)
  in Ok(value)  then value
  in Err(error)
    return error.message()
  end
  bound = case listener.local_address()
  in Ok(value)  then value
  in Err(error)
    return error.message()
  end
  client = case Tcp().connect(bound)
  in Ok(value)  then value
  in Err(error)
    return error.message()
  end
  accepted = case listener.accept()
  in Ok(value)  then value
  in Err(error)
    return error.message()
  end
  case client.write_all(Bytes("hello"))
  in Err(error)
    return error.message()
  in Ok(_)      then ()
  end
  answer = case accepted.first.read_exact(5)
  in Ok(bytes)  then bytes.text()
  in Err(error) then error.message()
  end
  client.close()
  accepted.first.close()
  listener.close()
  answer
end

exchange()
"#
    );
    let (outcome, resources) = run(&source, &["Tcp"]);
    assert_eq!(outcome, "Done(\"hello\")");
    assert_eq!(resources, 0);
}

#[test]
fn write_all_handles_a_partial_virtual_write() {
    let source = format!(
        r#"{LOOPBACK}
def transfer(): Int with Tcp
  address = case loopback(0)
  in Ok(value)  then value
  in Err(_)
    return -1
  end
  listener = case Tcp().listen(address, 8)
  in Ok(value)  then value
  in Err(_)
    return -2
  end
  bound = case listener.local_address()
  in Ok(value)  then value
  in Err(_)
    return -3
  end
  client = case Tcp().connect(bound)
  in Ok(value)  then value
  in Err(_)
    return -4
  end
  server = case listener.accept()
  in Ok(value)  then value.first
  in Err(_)
    return -5
  end
  payload = ByteBuffer()
  index = 0
  while index < 9000
    payload.append(120)
    index = index + 1
  end
  case client.write_all(payload.finish())
  in Err(_)
    return -6
  in Ok(_)  then ()
  end
  count = case server.read_exact(9000)
  in Ok(bytes) then bytes.len()
  in Err(_)    then -7
  end
  client.close()
  server.close()
  listener.close()
  count
end

transfer()
"#
    );
    assert_eq!(run(&source, &["Tcp"]).0, "Done(9000)");
}

#[test]
fn the_public_address_factory_rejects_invalid_values() {
    let source = r#"
bytes = ByteBuffer()
bytes.append(127)
bytes.append(0)
bytes.append(1)
case Tcp().address(IpAddress.V4(bytes.finish()), 80, 0, 0)
in Ok(_)                    then "accepted"
in Err(InvalidInput(value)) then value
in Err(_)                   then "wrong error"
end
"#;
    assert_eq!(
        run(source, &[]).0,
        "Done(\"an IPv4 address needs four bytes\")"
    );
}

#[test]
fn user_code_cannot_bypass_socket_address_validation() {
    let source = r#"
SocketAddress(IpAddress.V4(Bytes("bad")), 80, 0, 0)
"#;
    let error =
        compile_to_bytes("invalid-address.lm", source).expect_err("the direct constructor rejects");
    assert!(
        error.contains("use `Tcp().address` to construct a SocketAddress"),
        "{error}"
    );
}

#[test]
fn a_live_listener_blocks_a_self_snapshot() {
    let source = format!(
        r#"{LOOPBACK}
def capture(): String with Tcp.Listener, Vm
  address = case loopback(0)
  in Ok(value) then value
  in Err(_)
    return "address failed"
  end
  listener = case Tcp().listen(address, 8)
  in Ok(value) then value
  in Err(_)
    return "listen failed"
  end
  kind = case sys.vm.snapshot_self()
  in Ok(_) then "captured"
  in Err(ResourceActive(_, value)) then value
  in Err(_) then "wrong error"
  end
  listener.close()
  kind
end

capture()
"#
    );
    assert_eq!(
        run(&source, &["Tcp.Listener", "Vm"]).0,
        "Done(\"TCP listener\")"
    );
}

#[test]
fn a_second_tcp_close_returns_closed() {
    let source = format!(
        r#"{LOOPBACK}
def close_twice(): String with Tcp.Listener
  address = case loopback(0)
  in Ok(value) then value
  in Err(error)
    return error.message()
  end
  listener = case Tcp().listen(address, 4)
  in Ok(value) then value
  in Err(error)
    return error.message()
  end
  case listener.close()
  in Err(error)
    return error.message()
  in Ok(_) then ()
  end
  case listener.close()
  in Err(Closed) then "closed"
  in Err(error) then error.message()
  in Ok(_) then "accepted"
  end
end

close_twice()
"#
    );
    let (outcome, resources) = run(&source, &["Tcp.Listener"]);
    assert_eq!(outcome, "Done(\"closed\")");
    assert_eq!(resources, 0);
}

#[test]
fn a_driver_can_mint_and_service_a_tcp_stream() {
    let source = format!(
        r#"{LOOPBACK}
def client(address: SocketAddress): Int with Tcp.Connect, Tcp.Write, Tcp.Close
  case Tcp().connect(address)
  in Err(_) then -1
  in Ok(stream)
    written = case stream.write(Bytes("hello"))
    in Ok(value) then value
    in Err(_)    then -2
    end
    stream.close()
    written
  end
end

def service(child: Run[Int], mine: ResourceHandle): Int with Vm
  loop do
    case child.drive()
    in Asked(request)
      case request
      in Call(Tcp.Write, call, (stream, bytes))
        if child.resource(stream).same_resource(mine)
          child.answer(call, Ok(bytes.len()))
        else
          child.dispatch(request)
        end
      in Call(Tcp.Close, call, (stream,))
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
        return -3
      end
      return value
    in Fault(_)
      return -4
    end
  end
end

case loopback(8080)
in Err(_) then -5
in Ok(address)
  child = sys.vm.Vm().activate_or_fault(client, args: (address,))
  case child.drive()
  in Asked(request)
    case request
    in Call(Tcp.Connect, call, (peer,))
      mine = child.serve_tcp_stream(call, peer)
      service(child, mine)
    in _ then -6
    end
  in Done(_)  then -7
  in Fault(_) then -8
  end
end
"#
    );
    assert_eq!(run(&source, &["Vm"]).0, "Done(5)");
}

#[test]
fn a_driver_can_mint_and_close_a_tcp_listener() {
    let source = format!(
        r#"{LOOPBACK}
def server(address: SocketAddress): Bool with Tcp.Listen, Tcp.Close
  case Tcp().listen(address, 4)
  in Err(_) then false
  in Ok(listener)
    listener.close().is_ok()
  end
end

def finish(child: Run[Bool], mine: ResourceHandle): Bool with Vm
  loop do
    case child.drive()
    in Asked(request)
      case request
      in Call(Tcp.Close, call, (listener,))
        if child.resource(listener).same_resource(mine)
          child.answer(call, Ok(()))
        else
          child.dispatch(request)
        end
      in _
        child.dispatch(request)
      end
    in Done(value)
      return value and not mine.is_open()
    in Fault(_)
      return false
    end
  end
end

case loopback(8081)
in Err(_) then false
in Ok(address)
  child = sys.vm.Vm().activate_or_fault(server, args: (address,))
  case child.drive()
  in Asked(request)
    case request
    in Call(Tcp.Listen, call, (_, _))
      mine = child.serve_tcp_listener(call)
      finish(child, mine)
    in _ then false
    end
  in Done(_)  then false
  in Fault(_) then false
  end
end
"#
    );
    assert_eq!(run(&source, &["Vm"]).0, "Done(true)");
}
