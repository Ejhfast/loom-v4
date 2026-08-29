//! The shipped terminal editor example.
//!
//! The editor is a package, so the admission corpus in `admission.rs`
//! skips it. These tests build the package and drive the built
//! artifact with scripted keys, as a terminal sends them.

use lm_compiler::build_package;
use lm_testkit::{publish_artifact_bytes, repo_root};
use lm_vm::{RecordingHost, VmConfig, World};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;

/// The grants the editor documents in its README.
const GRANTS: [&str; 6] = ["Args", "Clock", "Fs", "Io", "Tty", "Wait"];

const FILE_NAME: &str = "notes.txt";

/// Build the shipped package once for this test binary.
fn editor_artifact() -> &'static Vec<u8> {
    static ARTIFACT: OnceLock<Vec<u8>> = OnceLock::new();
    ARTIFACT.get_or_init(|| {
        let report = build_package(
            &repo_root().join("examples/16-text-editor"),
            &repo_root().join("target/test-text-editor"),
        )
        .expect("the editor package builds");
        let path = report
            .artifact
            .expect("the package builds one root artifact");
        std::fs::read(path).expect("the editor artifact reads")
    })
}

/// Run the editor over one key script. The editor also leaves its
/// loop when the input ends, so a script needs no quit key.
fn run_editor(keys: &[u8], contents: &[u8]) -> Rc<RefCell<RecordingHost>> {
    let (arena, namespace) =
        publish_artifact_bytes(editor_artifact()).expect("the editor artifact loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    {
        let mut inner = host.borrow_mut();
        inner.input_bytes = keys.to_vec();
        inner.arguments = vec![FILE_NAME.to_string()];
        inner.set_terminal_size(60, 12);
        inner.set_file(FILE_NAME, contents.to_vec());
    }
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(Rc::clone(&host)),
    );
    for grant in GRANTS {
        world.allow(grant).expect("the editor grant exists");
    }
    let outcome = world.run_root();
    assert!(
        matches!(outcome, lm_vm::Outcome::Done(_)),
        "the editor faulted: {}",
        world.show_outcome(&outcome)
    );
    assert!(
        !host.borrow().raw_mode_active(),
        "the editor left raw mode open"
    );
    host
}

/// Down, Ctrl-E, `!`, Enter, `gamma`, Ctrl-S, Ctrl-Q.
///
/// The script moves, edits, splits one line, saves, and quits. The
/// saved bytes prove that each step reached the document.
#[test]
fn the_editor_edits_and_saves_a_file() {
    let host = run_editor(b"\x1b[B\x05!\rgamma\x13\x11", b"alpha\nbeta\n");
    let saved = host
        .borrow()
        .file(FILE_NAME)
        .expect("the editor saved the file")
        .to_vec();
    assert_eq!(
        String::from_utf8(saved).expect("the saved file is UTF-8"),
        "alpha\nbeta!\ngamma\n"
    );
}

/// Ctrl-F then `beta`.
///
/// The last frame must invert the match, because the cursor stays in
/// the prompt line and the inverted text is the only sign of it.
#[test]
fn the_editor_marks_the_search_match() {
    let host = run_editor(b"\x06beta", b"alpha\nbeta\ngamma\n");
    let frames = host.borrow().written_bytes.clone();
    assert!(
        contains(&frames, b"Find: beta"),
        "the editor drew no search prompt"
    );
    assert!(
        contains(&frames, b"\x1b[7mbeta\x1b[0m"),
        "no frame inverted the search match"
    );
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
