//! Standard lexical paths and RFC 3986 references.

use lm_testkit::run_text;
use lm_vm::VmConfig;

fn run(source: &str) -> String {
    run_text("standard-path-url.lm", source, VmConfig::default()).expect("the program runs")
}

#[test]
fn posix_paths_normalize_and_split_lexically() {
    let source = r#"
use std.path.PathStyle
use std.path.join
use std.path.normalize

path = normalize("/srv/./loom/../data/file.tar.gz", PathStyle.Posix).expect("valid path")
joined = join("a/b", "../c", PathStyle.Posix).expect("valid join")
(
  path.value,
  path.is_absolute(),
  path.parent().expect("a parent exists").value,
  path.file_name().expect("a name exists"),
  path.extension().expect("an extension exists"),
  path.stem().expect("a stem exists"),
  joined.value
)
"#;

    assert_eq!(
        run(source),
        "Done((\"/srv/data/file.tar.gz\", true, \"/srv/data\", \"file.tar.gz\", \"gz\", \"file.tar\", \"a/c\"))"
    );
}

#[test]
fn windows_paths_preserve_roots_and_use_one_separator() {
    let source = r#"
use std.path.PathStyle
use std.path.join
use std.path.normalize

drive = normalize("C:/work\\loom/../data", PathStyle.Windows).expect("valid drive path")
unc = normalize("\\\\server\\share\\a\\..\\b", PathStyle.Windows).expect("valid UNC path")
joined = join("C:\\work", "src/main.lm", PathStyle.Windows).expect("valid join")
(drive.value, drive.is_absolute(), unc.value, joined.value)
"#;

    assert_eq!(
        run(source),
        "Done((\"C:\\\\work\\\\data\", true, \"\\\\\\\\server\\\\share\\\\b\", \"C:\\\\work\\\\src\\\\main.lm\"))"
    );
}

#[test]
fn filesystem_helpers_accept_typed_paths() {
    let source = r#"
use std.fs.read_path_sorted
use std.path.PathStyle
use std.path.normalize

def go(): Int with Fs.ReadDir
  path = normalize("/loom/.", PathStyle.Posix).expect("valid path")
  read_path_sorted(path, 4).expect("the directory exists").len()
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
