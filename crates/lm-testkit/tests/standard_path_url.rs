//! Standard lexical paths and RFC 3986 references.

use lm_testkit::run_text;
use lm_vm::VmConfig;

fn run(source: &str) -> String {
    run_text("standard-path-url.lm", source, VmConfig::default()).expect("the program runs")
}

#[test]
fn posix_paths_normalize_and_split_lexically() {
    let source = r#"
use std.path.extension
use std.path.file_name
use std.path.is_absolute
use std.path.join
use std.path.normalize
use std.path.parent
use std.path.stem

path = normalize(Path("/srv/./loom/../data/file.tar.gz", PathStyle.Posix)).expect("valid path")
joined = join(Path("a/b", PathStyle.Posix), "../c").expect("valid join")
(
  path.value,
  is_absolute(path),
  parent(path).expect("a parent exists").value,
  file_name(path).expect("a name exists"),
  extension(path).expect("an extension exists"),
  stem(path).expect("a stem exists"),
  joined.value
)
"#;

    assert_eq!(
        run(source),
        "Done((\"/srv/data/file.tar.gz\", true, \"/srv/data\", \"file.tar.gz\", \"gz\", \"file.tar\", \"a/b/../c\"))"
    );
}

#[test]
fn windows_paths_preserve_roots_and_child_spelling() {
    let source = r#"
use std.path.join
use std.path.is_absolute
use std.path.normalize

drive = normalize(Path("C:/work\\loom/../data", PathStyle.Windows)).expect("valid drive path")
unc = normalize(Path("\\\\server\\share\\a\\..\\b", PathStyle.Windows)).expect("valid UNC path")
joined = join(Path("C:\\work", PathStyle.Windows), "src/main.lm").expect("valid join")
(drive.value, is_absolute(drive), unc.value, joined.value)
"#;

    assert_eq!(
        run(source),
        "Done((\"C:\\\\work\\\\data\", true, \"\\\\\\\\server\\\\share\\\\b\", \"C:\\\\work\\\\src/main.lm\"))"
    );
}

#[test]
fn filesystem_helpers_accept_typed_paths() {
    let source = r#"
use std.fs.read_dir_sorted

def go(): Int with Fs.ReadDir
  path = Path("/loom", PathStyle.Posix)
  read_dir_sorted(path, 4).expect("the directory exists").len()
end
go()
"#;

    assert_eq!(
        lm_testkit::run_allowed("typed-path.lm", source, &["Fs.ReadDir"])
            .expect("the program runs"),
        "Done(0)"
    );
}

#[test]
fn urls_parse_authority_components_and_percent_encoding() {
    let source = r#"
use std.url.parse_url
use std.url.percent_decode
use std.url.percent_encode_component

url = parse_url("HTTPS://user@example.com:8443/a%20b?q=x/y#part").expect("valid URL")
authority = url.authority().expect("the URL has an authority")
(
  url.scheme(),
  authority.userinfo,
  authority.host,
  authority.port,
  url.path(),
  url.query().expect("the URL has a query"),
  url.fragment().expect("the URL has a fragment"),
  percent_encode_component("a b/é"),
  percent_decode("a%20b%2Fc").expect("valid encoding")
)
"#;

    assert_eq!(
        run(source),
        "Done((\"https\", \"user\", \"example.com\", 8443, \"/a%20b\", \"q=x/y\", \"part\", \"a%20b%2F%C3%A9\", \"a b/c\"))"
    );
}

#[test]
fn url_resolution_follows_rfc_3986_examples() {
    let source = r#"
use std.url.parse_reference
use std.url.parse_url
use std.url.resolve

base = parse_url("http://a/b/c/d;p?q").expect("valid base")
(
  display(resolve(base, parse_reference("g").expect("valid reference")).expect("valid resolution")),
  display(resolve(base, parse_reference("../g").expect("valid reference")).expect("valid resolution")),
  display(resolve(base, parse_reference("?y").expect("valid reference")).expect("valid resolution")),
  display(resolve(base, parse_reference("//g/x").expect("valid reference")).expect("valid resolution"))
)
"#;

    assert_eq!(
        run(source),
        "Done((\"http://a/b/c/g\", \"http://a/b/g\", \"http://a/b/c/d;p?y\", \"http://g/x\"))"
    );
}

#[test]
fn url_parsing_rejects_bad_ports_and_encodings() {
    let source = r#"
use std.url.UrlError
use std.url.parse_reference
use std.url.parse_url
use std.url.percent_decode

def failed[T](value: Result[T, UrlError]): Bool
  case value
  in Err(_) then true
  in Ok(_) then false
  end
end

(
  failed(parse_url("/relative")),
  failed(parse_url("https://example.com:70000/")),
  failed(parse_reference("/bad%2")),
  failed(percent_decode("%ff"))
)
"#;

    assert_eq!(run(source), "Done((true, true, true, true))");
}
