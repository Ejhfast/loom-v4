use super::*;

// ---------------------------------------------------------------
// Group 2: the language operations.
// ---------------------------------------------------------------

#[test]
#[ignore]
fn bench_language_operations() {
    let base = baseline();
    println!("LOOM\tcase\titers\tns_per_op\ttotal_ms");
    println!(
        "LOOM\t_baseline\t1\t{:.1}\t{:.3}",
        base.as_nanos() as f64,
        base.as_secs_f64() * 1e3
    );

    // An integer while loop: the interpreter dispatch floor.
    report(
        "int_loop",
        1_000_000,
        "i = 0\ns = 0\nwhile i < 1000000\n  s = s + i\n  i = i + 1\nend\ns\n",
        base,
    );

    // A direct call to a top-level function.
    report(
        "direct_call",
        1_000_000,
        "def add1(n: Int): Int\n  n + 1\nend\n\
         i = 0\ns = 0\nwhile i < 1000000\n  s = add1(s)\n  i = i + 1\nend\ns\n",
        base,
    );

    // A virtual call through the dispatch row.
    report(
        "virtual_call",
        1_000_000,
        "class Adder\n  step: Int = 1\n  def bump(self, n: Int): Int\n    n + self.step\n  end\nend\n\
         a = Adder()\ni = 0\ns = 0\nwhile i < 1000000\n  s = a.bump(s)\n  i = i + 1\nend\ns\n",
        base,
    );

    // A field read and a field write on a mutable receiver.
    report(
        "field_rw",
        1_000_000,
        "class Cell\n  v: Int = 0\n  def step(mut self)\n    self.v = self.v + 1\n  end\nend\n\
         c = Cell()\ni = 0\nwhile i < 1000000\n  c.step()\n  i = i + 1\nend\nc.v\n",
        base,
    );

    // Closure creation plus a call.
    report(
        "closure_call",
        1_000_000,
        "i = 0\ns = 0\nwhile i < 1000000\n  f = { |x: Int|: Int x + 1 }\n  s = f(s)\n  i = i + 1\nend\ns\n",
        base,
    );

    // Object construction.
    report(
        "class_init",
        500_000,
        "class Point\n  x: Int = 0\n  y: Int = 0\n  def init(mut self, x: Int, y: Int)\n    \
         self.x = x\n    self.y = y\n  end\nend\n\
         i = 0\ns = 0\nwhile i < 500000\n  p = Point(i, i)\n  s = s + p.x\n  i = i + 1\nend\ns\n",
        base,
    );

    // List append.
    report(
        "list_push",
        500_000,
        "xs: [Int] = []\ni = 0\nwhile i < 500000\n  xs.push(i)\n  i = i + 1\nend\nxs.len()\n",
        base,
    );

    // List index on a built list.
    report(
        "list_index",
        1_000_000,
        "xs: [Int] = []\ni = 0\nwhile i < 1000\n  xs.push(i)\n  i = i + 1\nend\n\
         j = 0\ns = 0\nwhile j < 1000000\n  s = s + xs.at(j % 1000)\n  j = j + 1\nend\ns\n",
        base,
    );

    // Map insert with integer keys.
    report(
        "map_insert",
        200_000,
        "m: {Int: Int} = {}\ni = 0\nwhile i < 200000\n  m.put(i, i)\n  i = i + 1\nend\nm.len()\n",
        base,
    );

    // Map lookup on a built map.
    report(
        "map_lookup",
        1_000_000,
        "m: {Int: Int} = {}\ni = 0\nwhile i < 1000\n  m.put(i, i)\n  i = i + 1\nend\n\
         j = 0\ns = 0\nwhile j < 1000000\n  s = s + m.at(j % 1000)\n  j = j + 1\nend\ns\n",
        base,
    );

    // Each map removal leaves one tombstone. Reinsertion keeps the
    // live map size stable and exercises periodic compaction.
    report(
        "map_remove_reinsert",
        200_000,
        "m: {Int: Int} = {}\ni = 0\nwhile i < 1000\n  m.put(i, i)\n  i = i + 1\nend\n\
         j = 0\nwhile j < 200000\n  key = j % 1000\n  m.remove(key)\n  m.put(key, key)\n  j = j + 1\nend\nm.len()\n",
        base,
    );

    // String interpolation formats one integer into new short text.
    // Accumulation here would measure quadratic copying instead.
    report(
        "string_interp",
        200_000,
        "s = \"\"\ni = 0\nwhile i < 200000\n  s = \"v#{i}\"\n  i = i + 1\nend\ns\n",
        base,
    );

    // Mixed integer arithmetic: multiply, divide, and modulo.
    report(
        "arith_mix",
        1_000_000,
        "i = 1\ns = 0\nwhile i < 1000001\n  s = s + i * 3 / 2 % 7\n  i = i + 1\nend\ns\n",
        base,
    );

    // One integer bitwise operation in a hot loop.
    report(
        "int_bitwise",
        1_000_000,
        "i = 0\ns = 0\nwhile i < 1000000\n  s = s ^ i\n  i = i + 1\nend\ns\n",
        base,
    );

    // One binary64 addition in a hot loop.
    report(
        "float_add",
        1_000_000,
        "i = 0\ns = 0.0\nwhile i < 1000000\n  s = s + 1.25\n  i = i + 1\nend\ns\n",
        base,
    );

    // Bytewise XOR allocates one frozen 32-byte result.
    report(
        "bytes_xor_32",
        20_000,
        "left = b\"0123456789abcdef0123456789abcdef\"\n\
         right = b\"ffffffffffffffffffffffffffffffff\"\n\
         value = left\ni = 0\nwhile i < 20000\n  value = left ^ right\n  i = i + 1\nend\nvalue.len()\n",
        base,
    );

    // One taken branch and one untaken branch per iteration.
    report(
        "branch",
        1_000_000,
        "i = 0\ns = 0\nwhile i < 1000000\n  if i % 2 == 0\n    s = s + 1\n  else\n    s = s - 1\n  end\n  i = i + 1\nend\ns\n",
        base,
    );

    // Integer equality keeps its sealed instruction inside a hot loop.
    report(
        "int_eq",
        1_000_000,
        "i = 0\nsame = false\nwhile i < 1000000\n  same = i == i\n  i = i + 1\nend\nsame\n",
        base,
    );

    // Text equality keeps its native content instruction.
    report(
        "text_eq",
        1_000_000,
        "a = \"loom\"\nb = \"loom\"\ni = 0\nsame = false\nwhile i < 1000000\n  same = a == b\n  i = i + 1\nend\nsame\n",
        base,
    );

    // Generic equality measures one verified interface call.
    report(
        "partial_eq",
        1_000_000,
        "final class Token implements PartialEq\n  value: Int\n  def init(mut self, value: Int)\n    self.value = value\n  end\n  def __eq__(self, other: Token): Bool\n    self.value == other.value\n  end\nend\ndef same[T: PartialEq](a: T, b: T): Bool\n  a == b\nend\na = Token(7)\nb = Token(7)\ni = 0\nequal = false\nwhile i < 1000000\n  equal = same(a, b)\n  i = i + 1\nend\nequal\n",
        base,
    );

    // A generic interface call selects one default method.
    report(
        "interface_default",
        1_000_000,
        "interface Valued\n  def value(self): Int\n    7\n  end\nend\nfinal class Token implements Valued\nend\ndef read[T: Valued](value: T): Int\n  value.value()\nend\ntoken = Token()\ni = 0\nvalue = 0\nwhile i < 1000000\n  value = read(token)\n  i = i + 1\nend\nvalue\n",
        base,
    );

    // Conditional list equality compares all elements.
    report(
        "list_eq",
        200_000,
        "left = [1, 2, 3, 4, 5, 6, 7, 8]\nright = left.copy()\n\
         i = 0\nequal = false\nwhile i < 200000\n  equal = left == right\n  i = i + 1\nend\nequal\n",
        base,
    );

    // Conditional list hashing combines all elements.
    report(
        "list_hash",
        200_000,
        "values = [1, 2, 3, 4, 5, 6, 7, 8]\ni = 0\nhash = 0\n\
         while i < 200000\n  hash = hash_of(values)\n  i = i + 1\nend\nhash\n",
        base,
    );

    // Tuple hashing uses the ordinary conditional interface path.
    report(
        "tuple_hash",
        200_000,
        "value = (1, 2, 3, 4)\ni = 0\nhash = 0\nwhile i < 200000\n  \
         hash = hash_of(value)\n  i = i + 1\nend\nhash\n",
        base,
    );

    // Closure-free sorting copies and sorts sixteen integers.
    report(
        "list_sort",
        20_000,
        "source = [16, 7, 12, 3, 10, 1, 14, 5, 8, 15, 2, 11, 6, 13, 4, 9]\n\
         i = 0\nfirst = 0\nwhile i < 20000\n  values = source.copy()\n  values.sort()\n  first = values.at(0)\n  i = i + 1\nend\nfirst\n",
        base,
    );

    // Recursion: the call path with a growing activation stack.
    report(
        "recursion",
        1_000_000,
        "def down(n: Int): Int\n  if n <= 0\n    0\n  else\n    down(n - 1) + 1\n  end\nend\n\
         i = 0\ns = 0\nwhile i < 1000\n  s = s + down(1000)\n  i = i + 1\nend\ns\n",
        base,
    );

    // A virtual call that resolves on an inherited method.
    report(
        "inherit_call",
        1_000_000,
        "class Base\n  step: Int = 1\n  def bump(self, n: Int): Int\n    n + self.step\n  end\nend\n\
         class Derived < Base\nend\n\
         d = Derived()\ni = 0\ns = 0\nwhile i < 1000000\n  s = d.bump(s)\n  i = i + 1\nend\ns\n",
        base,
    );

    // A closure that captures a local, against the free closure above.
    report(
        "closure_capture",
        1_000_000,
        "k = 7\ni = 0\ns = 0\nwhile i < 1000000\n  f = { |x: Int|: Int x + k }\n  s = f(s)\n  i = i + 1\nend\ns\n",
        base,
    );

    // A generic call: the type application path.
    report(
        "generic_call",
        1_000_000,
        "def pick[T](a: T, b: T): T\n  a\nend\n\
         i = 0\ns = 0\nwhile i < 1000000\n  s = pick(s + 1, 0)\n  i = i + 1\nend\ns\n",
        base,
    );

    // Enum construction plus a `case` dispatch over two arms.
    report(
        "enum_case",
        1_000_000,
        "enum Step\n  Up(v: Int)\n  Down(v: Int)\nend\n\
         i = 0\ns = 0\nwhile i < 1000000\n  e: Step = Up(1)\n  \
         s = s + case e\n  in Up(v) then v\n  in Down(v) then 0 - v\n  end\n  i = i + 1\nend\ns\n",
        base,
    );

    // The non-faulting list access: a native op that builds a core
    // `Option`, then a `case` over it.
    report(
        "option_case",
        1_000_000,
        "xs: [Int] = []\ni = 0\nwhile i < 1000\n  xs.push(i)\n  i = i + 1\nend\n\
         j = 0\ns = 0\nwhile j < 1000000\n  \
         s = s + case xs.get(j % 1000)\n  in Some(v) then v\n  in None then 0\n  end\n  j = j + 1\nend\ns\n",
        base,
    );

    // A map with string keys, against the integer-key cases above.
    report(
        "map_str_lookup",
        500_000,
        "m: {String: Int} = {}\ni = 0\nwhile i < 1000\n  m.put(\"k#{i}\", i)\n  i = i + 1\nend\n\
         j = 0\ns = 0\nwhile j < 500000\n  s = s + m.at(\"k500\")\n  j = j + 1\nend\ns\n",
        base,
    );

    // A map with immutable byte keys uses the native byte hash path.
    report(
        "map_bytes_lookup",
        500_000,
        "key = Bytes(\"loom\")\nm: {Bytes: Int} = {}\nm.put(key, 7)\n\
         j = 0\ns = 0\nwhile j < 500000\n  s = s + m.at(key)\n  j = j + 1\nend\ns\n",
        base,
    );

    // A user key uses one hash call and one equality call per lookup.
    report(
        "map_hashable_lookup",
        500_000,
        "final class Key implements Hashable\n  value: Int\n  \
         def init(mut self, value: Int)\n    self.value = value\n  end\n  \
         def __eq__(self, other: Key): Bool\n    self.value == other.value\n  end\n  \
         def __hash__(self): Int\n    self.value\n  end\nend\n\
         key = Key(7).freeze()\nm = Map[Key, Int]()\nm.put(key, 9)\n\
         j = 0\ns = 0\nwhile j < 500000\n  s = s + m.at(key)\n  j = j + 1\nend\ns\n",
        base,
    );

    // The string builder uses the growable text path.
    report(
        "string_builder",
        500_000,
        "b = StringBuilder()\ni = 0\nwhile i < 500000\n  b.append(\"x\")\n  i = i + 1\nend\nb.build()\n",
        base,
    );

    // Scalar traversal uses one forward UTF-8 byte cursor.
    report(
        "text_each",
        600_000,
        "def ignore(value: Char): ()\n  ()\nend\n\
         text = \"aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫\"\n\
         i = 0\nwhile i < 10000\n  text.each(ignore)\n  i = i + 1\nend\ntext.len()\n",
        base,
    );

    // Split a document into fields. This case measures the design and
    // not the search: a Loom piece shares the source allocation, and
    // a CPython piece is a copy. The count is pieces, not iterations.
    report(
        "text_split",
        320_000,
        "row = \"alpha,beta,gamma,delta,epsilon,zeta,eta,theta,iota,kappa\"\n\
         total = 0\ni = 0\nwhile i < 32000\n  total = total + row.split(\",\").len()\n\
         \x20 i = i + 1\nend\ntotal\n",
        base,
    );

    // Split a line into a key and a value. This is the shape a
    // configuration or header parser writes, and it allocates one
    // Option and one tuple for each line.
    report(
        "text_split_once",
        200_000,
        "line = \"content-length: 4096\"\ntotal = 0\ni = 0\nwhile i < 200000\n\
         \x20 total = total + case line.split_once(\": \")\n\
         \x20 in Some((key, _)) then key.byte_len()\n  in None then 0\n  end\n\
         \x20 i = i + 1\nend\ntotal\n",
        base,
    );

    // Narrow one piece and keep it as a view. Loom copies nothing.
    report(
        "text_trim",
        500_000,
        "padded = \"   content-length   \"\ntotal = 0\ni = 0\nwhile i < 500000\n\
         \x20 total = total + padded.trim().byte_len()\n  i = i + 1\nend\ntotal\n",
        base,
    );

    // Decode bytes to text. Loom validates once and shares the
    // allocation. CPython allocates and copies.
    report(
        "bytes_decode",
        200_000,
        "b = ByteBuffer()\ni = 0\nwhile i < 512\n  b.append(97)\n  i = i + 1\nend\n\
         raw = b.finish()\ntotal = 0\nj = 0\nwhile j < 200000\n\
         \x20 total = total + case raw.utf8_view()\n  in Ok(text) then text.byte_len()\n\
         \x20 in Err(_) then 0\n  end\n  j = j + 1\nend\ntotal\n",
        base,
    );

    // The same decode over a large buffer. The Loom cost is one
    // validation plus one allocation and does not grow with the copy
    // CPython must make, so this pair locates the crossing point.
    report(
        "bytes_decode_large",
        20_000,
        "b = ByteBuffer()\ni = 0\nwhile i < 65536\n  b.append(97)\n  i = i + 1\nend\n\
         raw = b.finish()\ntotal = 0\nj = 0\nwhile j < 20000\n\
         \x20 total = total + case raw.utf8_view()\n  in Ok(text) then text.byte_len()\n\
         \x20 in Err(_) then 0\n  end\n  j = j + 1\nend\ntotal\n",
        base,
    );

    // Compare two strings. The ordering hooks reach one intrinsic.
    report(
        "text_compare",
        1_000_000,
        "a = \"content-length\"\nb = \"content-type\"\ntotal = 0\ni = 0\n\
         while i < 1000000\n  if a < b\n    total = total + 1\n  end\n  i = i + 1\nend\ntotal\n",
        base,
    );

    // The byte buffer.
    report(
        "byte_buffer",
        500_000,
        "b = ByteBuffer()\ni = 0\nwhile i < 500000\n  b.append(65)\n  i = i + 1\nend\nb.len()\n",
        base,
    );

    // The two cases below run the same workload inside a `World`.
    // The allocating case reports local heap work. The integer case
    // reports the activation loop cost alone.
    report_world(
        "world_class_init",
        500_000,
        "class Point\n  x: Int = 0\n  y: Int = 0\n  def init(mut self, x: Int, y: Int)\n    \
         self.x = x\n    self.y = y\n  end\nend\n\
         i = 0\ns = 0\nwhile i < 500000\n  p = Point(i, i)\n  s = s + p.x\n  i = i + 1\nend\ns\n",
        "Done(124999750000)",
    );
    report_world(
        "world_int_loop",
        1_000_000,
        "i = 0\ns = 0\nwhile i < 1000000\n  s = s + i\n  i = i + 1\nend\ns\n",
        "Done(499999500000)",
    );
    report_world_with(
        "direct_clock",
        1_000_000,
        "i = 0\ns = 0\nwhile i < 1000000\n  s = s + sys.clock.now()\n  i = i + 1\nend\ns\n",
        &["Clock"],
        "Done(501000500000)",
    );
}
