//! Bounded HTTP message tests.

use lm_testkit::{compile_to_bytes, run_text};
use lm_vm::{load_bytes, RecordingHost, VmConfig, World};

const HTTP_USES: &str = r#"use std.http.Http
use std.http.HttpError
use std.http.HttpHeader
use std.http.HttpLimits
use std.http.HttpRequest
use std.http.HttpResponse

"#;

const TLS_USES: &str = r#"use std.tls.TlsClientConfig
use std.tls.TlsRoots
use std.tls.TlsVersion

"#;

fn with_http(source: &str) -> String {
    format!("{HTTP_USES}{source}")
}

fn with_https(source: &str) -> String {
    format!("{HTTP_USES}{TLS_USES}{source}")
}

fn run(source: &str) -> String {
    run_text("http.lm", &with_http(source), VmConfig::default()).expect("the HTTP program runs")
}

fn run_https(source: &str) -> String {
    run_text("https.lm", &with_https(source), VmConfig::default()).expect("the HTTPS program runs")
}

fn run_network(source: &str) -> (String, usize) {
    let bytes =
        compile_to_bytes("http_network.lm", &with_http(source)).expect("the HTTP program compiles");
    let loaded = load_bytes(&bytes).expect("the HTTP program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Tcp").expect("the TCP grant exists");
    let outcome = lm_proc::run_world(&mut world);
    (world.show_outcome(&outcome), world.resource_count(0))
}

#[test]
fn request_serialization_has_explicit_framing() {
    let source = r#"
request = HttpRequest(
  "POST",
  "/items",
  [HttpHeader("X-Test", Bytes("yes"))],
  Bytes("data")
)
case Http().serialize_request("example.test", 8080, request, Http().default_limits())
in Ok(wire) then wire.text()
in Err(error) then display(error)
end
"#;
    assert_eq!(
        run(source),
        "Done(\"POST /items HTTP/1.1\\r\\nHost: example.test:8080\\r\\nConnection: close\\r\\nX-Test: yes\\r\\nContent-Length: 4\\r\\n\\r\\ndata\")"
    );
}

#[test]
fn response_serialization_accepts_a_reason_with_spaces() {
    let source = r#"
response = HttpResponse(404, [HttpHeader("X-Test", Bytes("no"))], Bytes("lost"))
case Http().serialize_response(response, "Not Found", Http().default_limits())
in Ok(wire) then wire.text()
in Err(error) then display(error)
end
"#;
    assert_eq!(
        run(source),
        "Done(\"HTTP/1.1 404 Not Found\\r\\nConnection: close\\r\\nX-Test: no\\r\\nContent-Length: 4\\r\\n\\r\\nlost\")"
    );
}

#[test]
fn response_parsing_supports_all_initial_body_frames() {
    let source = r#"
http = Http()
limits = http.default_limits()
fixed = case http.parse_response(
  Bytes("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-A: one\r\n\r\nhello"),
  "GET",
  limits
)
in Ok(value)
  header = case value.header("x-a")
  in Some(bytes) then bytes.text()
  in None        then "missing"
  end
  (value.status, header, value.body.text())
in Err(error) then (0, "missing", display(error))
end
chunked = case http.parse_response(
  Bytes("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3;part=yes\r\nabc\r\n2\r\nde\r\n0\r\nX-End: yes\r\n\r\n"),
  "GET",
  limits
)
in Ok(value) then value.body.text()
in Err(error) then display(error)
end
ended = case http.parse_response(
  Bytes("HTTP/1.0 200 OK\r\n\r\nrest"),
  "GET",
  limits
)
in Ok(value) then value.body.text()
in Err(error) then display(error)
end
(fixed, chunked, ended)
"#;
    assert_eq!(
        run(source),
        "Done(((200, \"one\", \"hello\"), \"abcde\", \"rest\"))"
    );
}

#[test]
fn request_parsing_preserves_ordered_headers_and_body() {
    let source = r#"
case Http().parse_request(
  Bytes("PUT /thing HTTP/1.1\r\nHost: local\r\nX-A: one\r\nX-A: two\r\nContent-Length: 3\r\n\r\nabc"),
  Http().default_limits()
)
in Ok(value)
  header = case value.header("X-A")
  in Some(bytes) then bytes.text()
  in None        then "missing"
  end
  (
    value.method,
    value.target,
    value.headers.len(),
    header,
    value.body.text()
  )
in Err(error) then (display(error), "", 0, "", "")
end
"#;
    assert_eq!(
        run(source),
        "Done((\"PUT\", \"/thing\", 4, \"one\", \"abc\"))"
    );
}

#[test]
fn parsers_reject_conflicting_or_ambiguous_framing() {
    let source = r#"
http = Http()
limits = http.default_limits()
conflict = case http.parse_response(
  Bytes("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n"),
  "GET",
  limits
)
in Ok(_) then "accepted"
in Err(error) then display(error)
end
duplicate = case http.parse_request(
  Bytes("GET / HTTP/1.1\r\nHost: one\r\nHost: two\r\n\r\n"),
  limits
)
in Ok(_) then "accepted"
in Err(error) then display(error)
end
signed = case http.parse_response(
  Bytes("HTTP/1.1 200 OK\r\nContent-Length: +1\r\n\r\nx"),
  "GET",
  limits
)
in Ok(_) then "accepted"
in Err(error) then display(error)
end
(conflict, duplicate, signed)
"#;
    assert_eq!(
        run(source),
        "Done((\"the HTTP body framing conflicts\", \"the HTTP Host header count is invalid\", \"the HTTP content length is invalid\"))"
    );
}

#[test]
fn parsers_enforce_message_limits() {
    let source = r#"
limits = HttpLimits(64, 1, 3, 1, 64, 8)
header = case Http().parse_response(
  Bytes("HTTP/1.1 200 OK\r\nX-A: a\r\nX-B: b\r\n\r\n"),
  "GET",
  limits
)
in Ok(_) then "accepted"
in Err(error) then display(error)
end
body = case Http().parse_response(
  Bytes("HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndata"),
  "GET",
  limits
)
in Ok(_) then "accepted"
in Err(error) then display(error)
end
(header, body)
"#;
    assert_eq!(
        run(source),
        "Done((\"the HTTP message has too many header fields\", \"the HTTP response body is too large\"))"
    );
}

#[test]
fn generated_fields_and_status_lines_follow_the_limits() {
    let source = r#"
http = Http()
request = HttpRequest("GET", "/", [], Bytes())
request_limits = HttpLimits(256, 2, 16, 4, 512, 16)
request_result = case http.serialize_request("local", 80, request, request_limits)
in Ok(_) then "accepted"
in Err(error) then display(error)
end
response = HttpResponse(200, [], Bytes())
response_limits = HttpLimits(256, 1, 16, 4, 512, 16)
response_result = case http.serialize_response(response, "OK", response_limits)
in Ok(_) then "accepted"
in Err(error) then display(error)
end
missing_reason = case http.parse_response(
  Bytes("HTTP/1.1 200\r\nContent-Length: 0\r\n\r\n"),
  "GET",
  http.default_limits()
)
in Ok(_) then "accepted"
in Err(error) then display(error)
end
informational = case http.parse_response(
  Bytes("HTTP/1.1 100 Continue\r\n\r\n"),
  "GET",
  http.default_limits()
)
in Ok(_) then "accepted"
in Err(error) then display(error)
end
unknown_range = case http.parse_response(
  Bytes("HTTP/1.1 600 Future\r\nContent-Length: 0\r\n\r\n"),
  "GET",
  http.default_limits()
)
in Ok(_) then "accepted"
in Err(error) then display(error)
end
(request_result, response_result, missing_reason, informational, unknown_range)
"#;
    assert_eq!(
        run(source),
        "Done((\"the request has too many header fields\", \"the response has too many header fields\", \"the HTTP status line is malformed\", \"informational HTTP responses are unsupported\", \"the HTTP status code is invalid\"))"
    );
}

#[test]
fn stream_helpers_handle_split_headers_and_bodies() {
    let source = r#"
def loopback(port: Int): Result[SocketAddress, NetError]
  bytes = ByteBuffer()
  bytes.append(127).append(0).append(0).append(1)
  Tcp().address(IpAddress.V4(bytes.finish()), port, 0, 0)
end

def exchange(): String with Tcp
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
  limits = HttpLimits(256, 16, 128, 16, 512, 3)
  request = HttpRequest("POST", "/echo", [], Bytes("hello"))
  wire = case Http().serialize_request("local", 80, request, limits)
  in Ok(value) then value
  in Err(error)
    return display(error)
  end
  case client.write_all(wire)
  in Err(error)
    return display(error)
  in Ok(_) then ()
  end
  received = case Http().read_request(server, limits)
  in Ok(value) then value
  in Err(error)
    return display(error)
  end
  raw = Bytes("HTTP/1.1 201 Made\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\nX-End: yes\r\n\r\n")
  case server.write_all(raw)
  in Err(error)
    return display(error)
  in Ok(_) then ()
  end
  response = case Http().read_response(client, request.method, limits)
  in Ok(value) then value
  in Err(error)
    return display(error)
  end
  client.close()
  server.close()
  listener.close()
  "{received.method} {received.target} {received.body.text()} {response.status} {response.body.text()}"
end

exchange()
"#;
    let (outcome, resources) = run_network(source);
    assert_eq!(outcome, "Done(\"POST /echo hello 201 ok\")");
    assert_eq!(resources, 0);
}

#[test]
fn the_cleartext_client_uses_the_transparent_effect_set() {
    let source = r#"
def fetch(): Result[HttpResponse, HttpError] with Http.CleartextClient
  request = HttpRequest("GET", "/", [], Bytes())
  Http().send("localhost", 80, request, Http().default_limits())
end

0
"#;
    compile_to_bytes("http_effect_set.lm", &with_http(source)).expect("the HTTP effect set checks");
}

#[test]
fn the_secure_client_uses_the_transparent_effect_set() {
    let source = r#"
def fetch(config: TlsClientConfig): Result[HttpResponse, HttpError] with Http.Client
  request = HttpRequest("GET", "/", [], Bytes())
  Http().send_secure("localhost", 443, request, config, Http().default_limits())
end

0
"#;
    compile_to_bytes("https_effect_set.lm", &with_https(source))
        .expect("the HTTPS effect set checks");
}

#[test]
fn the_secure_client_rejects_another_alpn_before_network_access() {
    let source = r#"
config = TlsClientConfig(
  "localhost",
  TlsRoots.Custom([Bytes("root")]),
  [Bytes("h2"), Bytes("http/1.1")],
  TlsVersion.Tls13,
  65536
).freeze()
request = HttpRequest("GET", "/", [], Bytes())
case Http().send_secure("localhost", 443, request, config, Http().default_limits())
in Ok(_) then "accepted"
in Err(error) then display(error)
end
"#;
    assert_eq!(
        run_https(source),
        "Done(\"the HTTP client can offer only HTTP/1.1 with ALPN\")"
    );
}
