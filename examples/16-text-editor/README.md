# A text editor for the terminal

This package is a complete demo application. It opens a file, edits it
in raw terminal mode, and writes it back.

## Run it

Build the workspace first, then start the editor with one file path:

```sh
nix-shell --run "cargo build --release -p lm-cli"
./target/release/lm run --allow Args,Clock,Fs,Io,Tty,Wait \
  examples/16-text-editor -- notes.txt
```

The path argument is optional. Without it the editor starts one empty
buffer and asks for a name at the first save.

A missing file also starts an empty buffer. The first save creates it.

## Keys

| Key | Action |
| --- | --- |
| Arrows | Move the cursor |
| Home, End | Go to the line start or the line end |
| Ctrl-A, Ctrl-E | Go to the line start or the line end |
| PageUp, PageDown | Move one screen |
| Enter | Split the line |
| Backspace | Delete backward, or join the previous line |
| Delete | Delete forward, or join the next line |
| Tab | Insert spaces up to the next tab stop |
| Ctrl-S | Save |
| Ctrl-F | Find |
| Ctrl-Q | Quit |
| Ctrl-L | Show the key help again |

Find is incremental. Each typed character moves to the next match. The
editor inverts the current match, so you see which one it selected.

The arrow keys step through every match, not one match for each line.
Down and Right move forward. Up and Left move back. The scan wraps at
the end of the file. Enter keeps the position. Escape returns the
cursor to its position before the search.

Ctrl-Q asks once before it discards unsaved changes.

## Grants

The editor performs these effect groups:

- `Args` reads the file path from the command line.
- `Tty` reports the terminal size and enters raw mode.
- `Io` reads keys and writes frames.
- `Fs` reads and writes the file.
- `Clock` and `Wait` support one short timer. The timer separates a
  single Escape key from an arrow key.

The editor restores the terminal before it returns. It also restores
the terminal after a termination signal, because raw mode installs one
signal guardian in the host.

## The modules

| Module | Content |
| --- | --- |
| `src/text.lm` | Scalar indexes, tab expansion, and screen columns |
| `src/document.lm` | The lines and their edit operations |
| `src/editor.lm` | The state, the modes, and the key dispatch |
| `src/screen.lm` | One frame becomes one byte string |
| `src/main.lm` | Arguments, the file, and the event loop |

`src/text.lm` and `src/document.lm` hold no effects. Every effect stays
in `src/editor.lm` and `src/main.lm`, and each row names its operations.

The editor decodes keys with `std.term.decode_key`. That function reads
one byte prefix and returns one `TermKey`, so the editor needs no
terminal parser of its own.

## Design notes

The editor draws the complete frame into one `ByteBuffer` and writes it
with one call. A partial frame never reaches the screen.

The event loop draws one frame for each pause in the input. A burst of
keys, such as a paste or a key repeat, applies before the next frame.
A 300-key burst therefore costs one frame, not 300.

The editor measures a line in Unicode scalar values, not in bytes. It
reads the terminal size for each frame, so a window resize needs no
signal.

## Limits

The editor treats each Unicode scalar value as one column. A wide
character, such as a CJK ideograph, therefore moves the cursor by one
column instead of two.

The editor holds the file in memory as one list of lines. It has no
undo and no syntax colors.

The save writes the file in place. It does not write a temporary file
first. `std.fs.durable_replace` supplies that stronger contract.

## Run the tests

The package carries unit tests for its pure parts in `src/suite.lm`.
The test runner finds each `Test` class and runs every test method in
its own child VM. The tests never enter the program artifact.

```sh
./target/release/lm test examples/16-text-editor
```

The suite also drives complete editing sessions. Each session runs the
real editor loop in a child VM. The test answers every operation from
the loop. It feeds one key per read and reports one terminal size. It
keeps each drawn frame and refuses the save request. The test then
checks the frame after each key. The run needs no terminal and no file.
It gives the same frames every time.
