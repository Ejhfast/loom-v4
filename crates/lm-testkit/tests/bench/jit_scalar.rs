use super::*;

// ---------------------------------------------------------------
// Group 0: guarded scalar JIT regions.
// ---------------------------------------------------------------

#[test]
#[ignore]
fn bench_jit_hot_scalar_loop() {
    println!(
        "LOOM_JIT\tcase\tinterpreter_ms\tnative_cold_ms\tnative_warm_ms\tspeedup\tentries\tguards\tcalls\talloc_sites\tallocations"
    );
    report_jit(
        "jit_int_loop",
        "i = 0\ns = 0\nwhile i < 1000000\n  s = s + i\n  i = i + 1\nend\ns\n",
        0,
    );
}

#[test]
#[ignore]
fn bench_jit_inline_leaf_scheduler() {
    report_jit_representative(
        "jit_inline_leaf_scheduler",
        concat!(
            "def step(value: Int): Int\n",
            "  mixed = value * 1664525 + 1013904223\n",
            "  mixed & 1048575\n",
            "end\n",
            "i = 0\nvalue = 1\n",
            "while i < 3000000\n",
            "  value = step(value)\n",
            "  i = i + 1\n",
            "end\nvalue\n",
        ),
    );
}

#[test]
#[ignore]
fn bench_jit_scalar_regions() {
    println!(
        "LOOM_JIT\tcase\tinterpreter_ms\tnative_cold_ms\tnative_warm_ms\tspeedup\tentries\tguards\tcalls\talloc_sites\tallocations"
    );
    report_jit(
        "jit_int_loop",
        "i = 0\ns = 0\nwhile i < 1000000\n  s = s + i\n  i = i + 1\nend\ns\n",
        0,
    );
    report_jit(
        "jit_float_add",
        "i = 0\ns = 0.0\nwhile i < 1000000\n  s = s + 1.25\n  i = i + 1\nend\ns\n",
        0,
    );
    report_jit(
        "jit_int_eq",
        "i = 0\nsame = false\nwhile i < 1000000\n  same = i == i\n  i = i + 1\nend\nsame\n",
        0,
    );
    report_jit(
        "jit_value_eq",
        concat!(
            "enum Pair\n  Value(left: Int, right: (Int, String))\nend\n",
            "left: Pair = Value(1, (2, \"loom\"))\n",
            "right: Pair = Value(1, (2, \"loom\"))\n",
            "i = 0\nsame = false\n",
            "while i < 200000\n",
            "  same = left == right\n",
            "  i = i + 1\n",
            "end\nsame\n",
        ),
        0,
    );
    report_jit(
        "jit_text_bytes_compare",
        concat!(
            "text = \"alpha\"\nlater = \"omega\"\n",
            "bytes = b\"alpha\"\nlater_bytes = b\"omega\"\n",
            "i = 0\nvalid = false\nhash = 0\n",
            "while i < 100000\n",
            "  valid = text < later and bytes < later_bytes\n",
            "  hash = hash_of(text) ^ hash_of(bytes)\n",
            "  i = i + 1\n",
            "end\n(valid, hash)\n",
        ),
        0,
    );
    report_jit(
        "jit_graph_operations",
        concat!(
            "items = list_repeated[Int](7, 32)\nitems.freeze()\n",
            "expected = items.digest()\ni = 0\nsame = false\n",
            "while i < 20000\n",
            "  items.freeze()\n",
            "  same = expected == items.digest()\n",
            "  i = i + 1\n",
            "end\nsame\n",
        ),
        0,
    );
    report_jit(
        "jit_expression_stack",
        concat!(
            "i = 0\ns = 0\n",
            "while i < 1000000\n",
            "  s = s + i * 2 - 1\n",
            "  i = i + 1\n",
            "end\ns\n",
        ),
        0,
    );
    report_jit(
        "jit_factorial",
        concat!(
            "def factorial(n: Int): Int\n",
            "  if n <= 1 then 1 else n * factorial(n - 1) end\n",
            "end\n",
            "i = 0\ns = 0\n",
            "while i < 10000\n",
            "  s = s + factorial(12)\n",
            "  i = i + 1\n",
            "end\ns\n",
        ),
        0,
    );
    report_jit(
        "jit_fibonacci",
        concat!(
            "def fib(n: Int): Int\n",
            "  if n <= 1 then n else fib(n - 1) + fib(n - 2) end\n",
            "end\n",
            "fib(25)\n",
        ),
        0,
    );
    const DEEP_RECURSION: &str = concat!(
        "def down(n: Int): Int\n",
        "  if n <= 0 then 0 else down(n - 1) + 1 end\n",
        "end\n",
        "i = 0\ns = 0\n",
        "while i < 1000\n",
        "  s = s + down(1000)\n  i = i + 1\n",
        "end\ns\n",
    );
    report_jit("jit_deep_recursion", DEEP_RECURSION, 0);
    report_jit(
        "jit_int_div",
        concat!(
            "i = 1\nd = 3\ns = 0\n",
            "while i < 1000000\n",
            "  q = i / d\n  s = s + q\n  d = d + 2\n",
            "  if d > 1009\n    d = 3\n  end\n",
            "  i = i + 1\nend\ns\n",
        ),
        0,
    );
    report_jit(
        "jit_int_rem",
        concat!(
            "i = 1\nd = 3\ns = 0\n",
            "while i < 1000000\n",
            "  r = i % d\n  s = s + r\n  d = d + 2\n",
            "  if d > 1009\n    d = 3\n  end\n",
            "  i = i + 1\nend\ns\n",
        ),
        0,
    );
    report_jit(
        "jit_direct_call",
        concat!(
            "def add1(value: Int): Int\n  next = value + 1\n  next\nend\n",
            "i = 0\nwhile i < 1000000\n  i = add1(i)\nend\ni\n",
        ),
        1,
    );
    report_jit(
        "jit_call_branch",
        concat!(
            "def add1(value: Int): Int\n",
            "  if value < 0 then value - 1 else value + 1 end\n",
            "end\n",
            "i = 0\nwhile i < 1000000\n  i = add1(i)\nend\ni\n",
        ),
        1,
    );
    report_jit_after_setup(
        "jit_field_read",
        concat!(
            "class Pair\n",
            "  left: Int\n",
            "  def init(mut self, left: Int)\n    self.left = left\n  end\n",
            "end\n",
            "pair = Pair(7)\ni = 0\ns = 0\n",
            "while i < 1000000\n",
            "  value = pair.left\n  s = s + value\n  i = i + 1\n",
            "end\ns\n",
        ),
        32,
    );
    report_jit_after_setup(
        "jit_field_write",
        concat!(
            "class Cell\n  value: Int = 0\nend\n",
            "def step(mut cell: Cell)\n  cell.value = cell.value + 1\nend\n",
            "cell = Cell()\ni = 0\n",
            "while i < 1000000\n  step(cell)\n  i = i + 1\nend\n",
            "cell.value\n",
        ),
        32,
    );
    report_jit_after_setup(
        "jit_tuple_read",
        concat!(
            "pair = (7, 11)\ni = 0\nsum = 0\n",
            "while i < 1000000\n  sum = sum + pair[0]\n  i = i + 1\nend\n",
            "sum + pair[1]\n",
        ),
        32,
    );
    report_jit_after_setup(
        "jit_list_read",
        concat!(
            "items = [0, 1, 2, 3, 4, 5, 6, 7]\ni = 0\nsum = 0\n",
            "while i < 1000000\n",
            "  sum = sum + items.at(i % 8)\n  i = i + 1\n",
            "end\nsum + items.len()\n",
        ),
        48,
    );
    report_jit(
        "jit_list_parameter_read",
        concat!(
            "def sum_items(items: [Int], count: Int): Int\n",
            "  index = 0\n  total = 0\n",
            "  while index < count\n",
            "    total = total + items.at(index % 8)\n",
            "    index = index + 1\n",
            "  end\n  total\nend\n",
            "sum_items([0, 1, 2, 3, 4, 5, 6, 7], 1000000)\n",
        ),
        1,
    );
    report_jit_after_setup(
        "jit_list_replace",
        concat!(
            "items = [0, 1, 2, 3, 4, 5, 6, 7]\ni = 0\n",
            "while i < 1000000\n",
            "  items.set(i % 8, i)\n  i = i + 1\n",
            "end\nitems.at(7)\n",
        ),
        48,
    );
    report_jit_after_setup(
        "jit_list_get",
        concat!(
            "items = [0, 1, 2, 3, 4, 5, 6, 7]\ni = 0\nsum = 0\n",
            "while i < 1000000\n",
            "  case items.get(i % 10)\n",
            "  in Some(value) then sum = sum + value\n",
            "  in None then sum = sum + 1\n",
            "  end\n",
            "  i = i + 1\n",
            "end\nsum\n",
        ),
        64,
    );
    report_jit_after_setup(
        "jit_map_lookup",
        concat!(
            "table = {\"a\": 3, \"b\": 5}\ni = 0\nsum = 0\n",
            "while i < 1000000\n",
            "  if table.has(\"a\")\n",
            "    sum = sum + table.at(\"a\")\n",
            "  end\n",
            "  i = i + 1\n",
            "end\nsum\n",
        ),
        64,
    );
    report_jit_after_setup(
        "jit_int_map_lookup",
        concat!(
            "table: {Int: Int} = {3: 5, 7: 11}\ni = 0\nsum = 0\n",
            "while i < 1000000\n",
            "  if table.has(3)\n",
            "    sum = sum + table.at(7)\n",
            "  end\n",
            "  i = i + 1\n",
            "end\nsum\n",
        ),
        64,
    );
    report_jit_after_setup(
        "jit_bytes_map_lookup",
        concat!(
            "key = Bytes(\"loom\")\ntable: {Bytes: Int} = {key: 5}\ni = 0\nsum = 0\n",
            "while i < 1000000\n",
            "  sum = sum + table.at(key)\n",
            "  i = i + 1\n",
            "end\nsum\n",
        ),
        64,
    );
    report_jit_after_setup(
        "jit_int_map_get",
        concat!(
            "table: {Int: Int} = {3: 5}\ni = 0\nsum = 0\n",
            "while i < 1000000\n",
            "  case table.get(3)\n",
            "  in Some(value) then sum = sum + value\n",
            "  in None then ()\n",
            "  end\n",
            "  i = i + 1\n",
            "end\nsum\n",
        ),
        64,
    );
    report_jit_after_setup(
        "jit_int_map_replace",
        concat!(
            "table: {Int: Int} = {3: 0}\ni = 1\nsum = 0\n",
            "while i < 1000000\n",
            "  case table.put(3, i)\n",
            "  in Some(previous) then sum = sum + previous\n",
            "  in None then ()\n",
            "  end\n",
            "  i = i + 1\n",
            "end\nsum + table.at(3)\n",
        ),
        64,
    );
    report_jit(
        "jit_map_insert",
        concat!(
            "table: {Int: Int} = {}\ni = 0\n",
            "while i < 50000\n",
            "  table.put(i, i)\n",
            "  i = i + 1\n",
            "end\ntable.len()\n",
        ),
        0,
    );
    report_jit_after_setup(
        "jit_map_remove_reinsert",
        concat!(
            "table: {Int: Int} = {}\ni = 0\n",
            "while i < 1000\n  table.put(i, i)\n  i = i + 1\nend\n",
            "i = 0\nwhile i < 200000\n",
            "  key = i % 1000\n  table.remove(key)\n  table.put(key, key)\n",
            "  i = i + 1\nend\ntable.len()\n",
        ),
        64,
    );
    report_jit(
        "jit_map_iteration",
        concat!(
            "table: {Int: Int} = {}\ni = 0\n",
            "while i < 1000\n  table.put(i, i)\n  i = i + 1\nend\n",
            "round = 0\nsum = 0\nwhile round < 1000\n",
            "  for _, value in table\n    sum = sum + value\n  end\n",
            "  round = round + 1\nend\nsum\n",
        ),
        0,
    );
    report_jit(
        "jit_map_mutations",
        concat!(
            "final class Key implements Hashable\n  value: Int\n",
            "  def init(mut self, value: Int)\n    self.value = value\n  end\n",
            "  def __eq__(self, other: Key): Bool\n    self.value == other.value\n  end\n",
            "  def __hash__(self): Int\n    self.value % 2\n  end\nend\n",
            "first = Key(1).freeze()\nsame = Key(1).freeze()\n",
            "collision = Key(3).freeze()\nraw = Map[Key, Int]()\n",
            "raw.put(first, 1)\nraw.put(collision, 3)\n",
            "direct = {\"a\": 1, \"b\": 2}\ni = 0\ntotal = 0\n",
            "while i < 100000\n",
            "  raw.put(same, i)\n  total = total + raw.at(same)\n",
            "  raw.remove(collision)\n  raw.put(collision, i + 1)\n",
            "  direct.put(\"a\", i)\n  total = total + direct.at(\"a\")\n",
            "  direct.remove(\"b\")\n  direct.put(\"b\", i + 1)\n",
            "  i = i + 1\nend\ntotal\n",
        ),
        0,
    );
    report_jit(
        "jit_list_push",
        concat!(
            "items: [Int] = []\ni = 0\n",
            "while i < 100000\n",
            "  items.push(i)\n",
            "  i = i + 1\n",
            "end\nitems.len()\n",
        ),
        0,
    );
    report_jit(
        "jit_list_mutations",
        concat!(
            "items: [Int] = []\ni = 0\ntotal = 0\n",
            "while i < 100000\n",
            "  items.insert(0, i)\n",
            "  items.insert(items.len(), i + 1)\n",
            "  total = total + items.remove(0)\n",
            "  total = total + items.swap_remove(0)\n",
            "  items.push(i)\n",
            "  items.truncate(0)\n",
            "  case items.pop()\n",
            "  in Some(_) then total = total - 1000\n",
            "  in None then total = total + 1\n",
            "  end\n",
            "  i = i + 1\n",
            "end\ntotal\n",
        ),
        0,
    );
    report_jit(
        "jit_list_reserve",
        concat!(
            "items = [1]\nitems.reserve(64)\ni = 0\n",
            "while i < 1000000\n",
            "  items.reserve(0)\n",
            "  i = i + 1\n",
            "end\nitems.capacity()\n",
        ),
        0,
    );
    report_jit(
        "jit_allocation",
        concat!(
            "class Token\nend\n",
            "i = 0\nwhile i < 100000\n",
            "  token = Token()\n  i = i + 1\n",
            "end\ni\n",
        ),
        0,
    );
    report_jit(
        "jit_generic_allocation",
        concat!(
            "class Token[T]\nend\n",
            "def make[T](): Token[T]\n  Token[T]()\nend\n",
            "i = 0\nwhile i < 100000\n",
            "  token = make[Int]()\n  i = i + 1\n",
            "end\ni\n",
        ),
        0,
    );
    println!(
        "LOOM_JIT_EFFECT\tcase\tinterpreter_ms\tnative_cold_ms\tnative_warm_ms\tnative_speedup\tauto_ms\tauto_speedup\teffect_sites\teffect_exits\tentries"
    );
    report_jit_effect(
        "jit_effect_mixed",
        concat!(
            "def go(): Int with Clock.Now\n",
            "  outer = 0\n  total = 0\n  observed = 0\n",
            "  while outer < 100\n",
            "    inner = 0\n",
            "    while inner < 10000\n",
            "      total = total + 1\n",
            "      inner = inner + 1\n",
            "    end\n",
            "    observed = sys.clock.now()\n",
            "    outer = outer + 1\n",
            "  end\n",
            "  total\n",
            "end\n",
            "go()\n",
        ),
        900,
    );
    report_jit_effect(
        "jit_effect_boundary",
        concat!(
            "def go(): Int with Clock.Now\n",
            "  i = 0\n  observed = 0\n",
            "  while i < 20000\n",
            "    observed = sys.clock.now()\n",
            "    i = i + 1\n",
            "  end\n",
            "  i\n",
            "end\n",
            "go()\n",
        ),
        180_000,
    );
    report_jit_effect(
        "jit_print_boundary",
        concat!(
            "def go(): Int with Io.Write\n",
            "  i = 0\n",
            "  while i < 20000\n",
            "    print(\"x\").expect(\"the output writes\")\n",
            "    i = i + 1\n",
            "  end\n",
            "  i\n",
            "end\n",
            "go()\n",
        ),
        180_000,
    );
    report_jit_effect(
        "jit_guest_drive_boundary",
        concat!(
            "def child(): Int with Clock.Now\n",
            "  i = 0\n",
            "  while i < 2000\n",
            "    observed = sys.clock.now()\n",
            "    i = i + 1\n",
            "  end\n",
            "  i\n",
            "end\n\n",
            "def drive_child(): Int with Vm\n",
            "  run = sys.vm.Vm().activate_or_fault(child, args: ())\n",
            "  answered = 0\n",
            "  loop do\n",
            "    case run.drive()\n",
            "    in Asked(request)\n",
            "      case request\n",
            "      in Call(Clock.Now, call, ())\n",
            "        run.answer(call, answered)\n",
            "        answered = answered + 1\n",
            "      in _ then return -1\n",
            "      end\n",
            "    in Done(value) then return value\n",
            "    in Fault(_) then return -2\n",
            "    end\n",
            "  end\n",
            "end\n",
            "drive_child()\n",
        ),
        36_000,
    );
    report_jit_sliced(
        "jit_int_loop_sliced",
        "i = 0\ns = 0\nwhile i < 1000000\n  s = s + i\n  i = i + 1\nend\ns\n",
        4096,
    );
    report_jit_scheduled(
        "jit_int_loop_scheduled",
        "i = 0\ns = 0\nwhile i < 1000000\n  s = s + i\n  i = i + 1\nend\ns\n",
    );
    report_jit_scheduled("jit_deep_recursion_scheduled", DEEP_RECURSION);
    report_guard_upper_bound();
    report_auto_mixed();
}

#[test]
#[ignore]
fn bench_jit_builder_construction() {
    println!(
        "LOOM_JIT\tcase\tinterpreter_ms\tnative_cold_ms\tnative_warm_ms\tspeedup\tentries\tguards\tcalls\talloc_sites\tallocations"
    );
    report_jit(
        "jit_string_builder",
        concat!(
            "builder = StringBuilder()\ni = 0\n",
            "while i < 200000\n",
            "  builder.append(\"x\")\n  i = i + 1\n",
            "end\nbuilder.build().len()\n",
        ),
        0,
    );
    report_jit(
        "jit_string_builder_int",
        concat!(
            "builder = StringBuilder()\ni = 0\n",
            "while i < 200000\n",
            "  builder.append_int(i)\n  i = i + 1\n",
            "end\nbuilder.build().len()\n",
        ),
        0,
    );
    report_jit(
        "jit_string_builder_char",
        concat!(
            "builder = StringBuilder()\ni = 0\n",
            "while i < 200000\n",
            "  builder.push_char('é')\n  i = i + 1\n",
            "end\nbuilder.build().len()\n",
        ),
        0,
    );
    report_jit(
        "jit_byte_buffer",
        concat!(
            "buffer = ByteBuffer()\ni = 0\n",
            "while i < 200000\n",
            "  buffer.append(i % 256)\n  i = i + 1\n",
            "end\nbuffer.build().len()\n",
        ),
        0,
    );
    report_jit(
        "jit_byte_construction",
        concat!(
            "left = b\"\\x0f\\xf0\"\nright = b\"\\x33\\x55\"\n",
            "i = 0\ntotal = 0\n",
            "while i < 20000\n",
            "  joined = left + right\n",
            "  total = total + (left & right).len() + (left | right).len()\n",
            "  total = total + (left ^ right).len() + (~joined).len()\n",
            "  i = i + 1\n",
            "end\ntotal\n",
        ),
        0,
    );
}

#[test]
#[ignore]
fn bench_jit_text_and_conversion_operations() {
    println!(
        "LOOM_JIT\tcase\tinterpreter_ms\tnative_cold_ms\tnative_warm_ms\tspeedup\tentries\tguards\tcalls\talloc_sites\tallocations"
    );
    report_jit(
        "jit_text_search",
        concat!(
            "text: Text = \"alpha,beta,gamma\"\ni = 0\ntotal = 0\n",
            "while i < 200000\n",
            "  if text.starts_with(\"alpha\") then total = total + 1 end\n",
            "  if text.ends_with(\"gamma\") then total = total + 1 end\n",
            "  if text.contains(\"beta\") then\n",
            "    case text.find(\"beta\")\n",
            "    in Some(index) then total = total + index\n",
            "    in None then total = total - 1\n",
            "    end\n",
            "  end\n",
            "  i = i + 1\n",
            "end\ntotal\n",
        ),
        0,
    );
    report_jit(
        "jit_text_transform",
        concat!(
            "text: Text = \"  Alpha,beta  \"\ni = 0\ntotal = 0\n",
            "while i < 20000\n",
            "  mapped = text.trim().to_lower_ascii().replace(\",\", \"|\")\n",
            "  total = total + mapped.len()\n",
            "  i = i + 1\n",
            "end\ntotal\n",
        ),
        0,
    );
    report_jit(
        "jit_numeric_conversion",
        concat!(
            "i = 0\ntotal = 0\n",
            "while i < 50000\n",
            "  case \"7f\".parse_int(16)\n",
            "  in Ok(value) then total = total + value\n",
            "  in Err(_) then total = total - 1\n",
            "  end\n",
            "  case \"12.5\".parse_float()\n",
            "  in Ok(value) then total = total + value.fixed(1).len()\n",
            "  in Err(_) then total = total - 1\n",
            "  end\n",
            "  i = i + 1\n",
            "end\ntotal\n",
        ),
        0,
    );
}
