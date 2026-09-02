use super::*;

// ---------------------------------------------------------------
// Group 1: representative JIT programs.
// ---------------------------------------------------------------

const JIT_JSON_PARSE_SOURCE: &str = r#"
use std.json.Json
use std.json.parse

source = "{\"name\":\"loom\",\"values\":[1,2,3,4],\"ready\":true}"
round = 0
total = 0
while round < 2000
  case parse(source)
  in Ok(Json.Object(fields)) then total = total + fields.len()
  in _ then total = total - 1000
  end
  round = round + 1
end
total
"#;

const JIT_WORDCOUNT_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/programs/wordcount.lm"
));
const JIT_CSV_REPORT_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/programs/csv_report.lm"
));
const JIT_JSON_PIPELINE_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/programs/json_pipeline.lm"
));
const JIT_JSON_PARSE_LARGE_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/programs/json_parse_large.lm"
));

#[test]
#[ignore]
fn bench_jit_cold_start_and_cache_pressure() {
    println!(
        "LOOM_JIT_COLD\tcase\tinterpreter_ms\tauto_ms\tauto_speedup\tcompiled_regions\tcode_bytes"
    );
    report_jit_cold(
        "jit_cold_int_loop",
        "i = 0\ns = 0\nwhile i < 1000000\n  s = s + i\n  i = i + 1\nend\ns\n",
    );
    report_jit_cold("jit_cold_json_parse", JIT_JSON_PARSE_SOURCE);
    let many = many_hot_functions_source(300, 1000);
    report_jit_cold("jit_many_hot_functions", &many);
    report_jit_representative("jit_many_hot_functions_warm", &many);
}

#[test]
#[ignore]
fn bench_jit_representative_programs() {
    println!(
        "LOOM_JIT_PROGRAM\tcase\tinterpreter_ms\tauto_ms\tnative_ms\tauto_speedup\tnative_speedup\tauto_coverage\tnative_coverage\tauto_compiles\tauto_demotions\tauto_unsupported\tnative_unsupported\tauto_interpreter_exits\tnative_interpreter_exits\tauto_env_exits\tnative_env_exits\tnative_env_fallbacks"
    );
    report_jit_slot_calls(
        "jit_slot_call",
        concat!(
            "final class Box\n  value: Int = 3\nend\n",
            "def identity[T](value: T): T\n  value\nend\n",
            "index = 0\ntotal = 0\n",
            "while index < 1000000\n",
            "  box = Box()\n",
            "  total = total + identity(box.value)\n",
            "  index = index + 1\n",
            "end\ntotal\n",
        ),
    );
    report_jit_representative(
        "jit_deep_recursion",
        concat!(
            "def down(n: Int): Int\n",
            "  if n <= 0 then 0 else down(n - 1) + 1 end\n",
            "end\n",
            "i = 0\ns = 0\n",
            "while i < 1000\n",
            "  s = s + down(1000)\n  i = i + 1\n",
            "end\ns\n",
        ),
    );
    report_jit_representative(
        "jit_call_branch",
        concat!(
            "def add1(value: Int): Int\n",
            "  if value < 0 then value - 1 else value + 1 end\n",
            "end\n",
            "i = 0\nwhile i < 1000000\n  i = add1(i)\nend\ni\n",
        ),
    );
    report_jit_representative(
        "jit_virtual_call",
        concat!(
            "class Base\n",
            "  def step(self, value: Int): Int\n    value + 1\n  end\n",
            "end\n",
            "class Child < Base\n",
            "  def step(self, value: Int): Int\n    value + 2\n  end\n",
            "end\n",
            "def run(value: Base): Int\n",
            "  index = 0\n  total = 0\n",
            "  while index < 1000000\n",
            "    total = total + value.step(index)\n",
            "    index = index + 1\n",
            "  end\n  total\n",
            "end\n",
            "run(Child())\n",
        ),
    );
    report_jit_representative(
        "jit_interface_call",
        concat!(
            "interface Valued\n",
            "  def value(self): Int\n    7\n  end\n",
            "end\n",
            "final class DefaultValue implements Valued\nend\n",
            "final class OverrideValue implements Valued\n",
            "  def value(self): Int\n    11\n  end\n",
            "end\n",
            "def read[T: Valued](value: T): Int\n  value.value()\nend\n",
            "left = DefaultValue()\nright = OverrideValue()\n",
            "index = 0\ntotal = 0\n",
            "while index < 500000\n",
            "  total = total + read(left) + read(right)\n",
            "  index = index + 1\n",
            "end\ntotal\n",
        ),
    );
    report_jit_representative(
        "jit_generic_virtual_call",
        concat!(
            "class Counter\n",
            "  def keep[U](self, other: U): Int\n    7\n  end\n",
            "end\n",
            "counter = Counter()\n",
            "index = 0\ntotal = 0\n",
            "while index < 1000000\n",
            "  total = total + counter.keep(index)\n",
            "  index = index + 1\n",
            "end\ntotal\n",
        ),
    );
    report_jit_representative(
        "jit_generic_call",
        concat!(
            "def identity[T](value: T): T\n  value\nend\n",
            "def outer[T](value: T): T\n  identity(value)\nend\n",
            "i = 0\ns = 0\n",
            "while i < 1000000\n",
            "  s = s + outer(i)\n  i = i + 1\n",
            "end\ns\n",
        ),
    );
    report_jit_representative(
        "jit_closure_call",
        concat!(
            "base = 7\n",
            "stored = do |value: Int|: Int base + value end\n",
            "i = 0\ntotal = 0\n",
            "while i < 1000000\n",
            "  total = total + stored(i)\n",
            "  i = i + 1\n",
            "end\ntotal\n",
        ),
    );
    report_jit_representative(
        "jit_quick_exit",
        concat!(
            "def append_one(mut items: [Int]): Int\n",
            "  items.push(1)\n",
            "  items.len()\n",
            "end\n",
            "items: [Int] = []\n",
            "i = 0\n",
            "while i < 50000\n",
            "  append_one(items)\n",
            "  i = i + 1\n",
            "end\n",
            "items.len()\n",
        ),
    );
    report_jit_representative(
        "jit_numeric_surface",
        concat!(
            "i = 0\ntotal = 0\n",
            "while i < 1000000\n",
            "  total = total + (i & 7)\n",
            "  i = i + 1\n",
            "end\ntotal\n",
        ),
    );
    report_jit_representative(
        "jit_option_values",
        concat!(
            "def read(value: Option[Int]): Int\n",
            "  case value\n",
            "  in Some(found) then found\n",
            "  in None then 0\n",
            "  end\n",
            "end\n",
            "i = 0\ntotal = 0\n",
            "while i < 1000000\n",
            "  value: Option[Int] = if i % 2 == 0 then Some(i) else None end\n",
            "  total = total + read(value)\n",
            "  i = i + 1\n",
            "end\ntotal\n",
        ),
    );
    report_jit_representative(
        "jit_literal_loads",
        concat!(
            "i = 0\ntext = \"\"\nbytes = b\"\"\n",
            "while i < 1000000\n",
            "  text = \"hello\"\n",
            "  bytes = b\"\\x01\\x02\"\n",
            "  i = i + 1\n",
            "end\n",
            "if text.byte_len() == 5 and bytes.len() == 2 then i else 0 end\n",
        ),
    );
    report_jit_representative(
        "jit_interpreter_site",
        concat!(
            "items: [Int] = []\ni = 0\n",
            "while i < 50000\n",
            "  items.push(i)\n",
            "  i = i + 1\n",
            "end\nitems.len()\n",
        ),
    );
    report_jit_representative(
        "jit_class_init",
        concat!(
            "class Point\n  x: Int = 0\n  y: Int = 0\n",
            "  def init(mut self, x: Int, y: Int)\n",
            "    self.x = x\n    self.y = y\n  end\nend\n",
            "i = 0\ns = 0\nwhile i < 500000\n",
            "  p = Point(i, i)\n  s = s + p.x\n  i = i + 1\n",
            "end\ns\n",
        ),
    );
    report_jit_representative(
        "jit_class_guard",
        concat!(
            "class Shape\nend\n",
            "class Circle < Shape\n  radius: Int = 3\nend\n",
            "class LargeCircle < Circle\nend\n",
            "def radius(shape: Shape): Int\n",
            "  if shape is Circle then (shape as Circle).radius else 0 end\n",
            "end\n",
            "shape: Shape = LargeCircle()\ni = 0\ntotal = 0\n",
            "while i < 1000000\n",
            "  total = total + radius(shape)\n",
            "  i = i + 1\n",
            "end\ntotal\n",
        ),
    );
    report_jit_representative(
        "jit_list_sort",
        concat!(
            "source = [16, 7, 12, 3, 10, 1, 14, 5, 8, 15, 2, 11, 6, 13, 4, 9]\n",
            "i = 0\nfirst = 0\nwhile i < 20000\n",
            "  values = source.copy()\n  values.sort()\n",
            "  first = values.at(0)\n  i = i + 1\n",
            "end\nfirst\n",
        ),
    );
    report_jit_representative(
        "jit_list_iteration",
        concat!(
            "items = [1, 2, 3, 4, 5, 6, 7, 8]\n",
            "round = 0\ntotal = 0\n",
            "while round < 100000\n",
            "  total = total + items.capacity()\n",
            "  for item in items\n",
            "    total = total + item\n",
            "  end\n",
            "  round = round + 1\n",
            "end\ntotal\n",
        ),
    );
    report_jit_representative(
        "jit_text_metadata",
        concat!(
            "def measure_string(value: String): Int\n",
            "  value.byte_len() * 10 + value.len()\n",
            "end\n",
            "def measure_view(value: Substring): Int\n",
            "  value.byte_len() * 10 + value.len()\n",
            "end\n",
            "text = \"aé猫z\"\n",
            "view = text.slice(1, 2).expect(\"the text slice exists\")\n",
            "i = 0\ntotal = 0\nhash = 0\n",
            "while i < 1000000\n",
            "  total = total + measure_string(text) + measure_view(view)\n",
            "  hash = hash_combine(hash, i)\n",
            "  i = i + 1\n",
            "end\n",
            "(total, hash)\n",
        ),
    );
    report_jit_representative(
        "jit_text_scalar_read",
        concat!(
            "text = \"aé猫z\"\n",
            "round = 0\ntotal = 0\n",
            "while round < 250000\n",
            "  index = 0\n",
            "  while index < text.len()\n",
            "    total = total + text.at(index).expect(\"the scalar exists\").codepoint()\n",
            "    index = index + 1\n",
            "  end\n",
            "  round = round + 1\n",
            "end\ntotal\n",
        ),
    );
    report_jit_representative(
        "jit_bytes_read",
        concat!(
            "def scan(bytes: Bytes): Int\n",
            "  total = 0\n  round = 0\n",
            "  while round < 250000\n",
            "    index = 0\n",
            "    while index < bytes.len()\n",
            "      total = total + bytes.at(index)\n",
            "      index = index + 1\n",
            "    end\n",
            "    round = round + 1\n",
            "  end\n",
            "  total\n",
            "end\n",
            "scan(Bytes(\"loom\"))\n",
        ),
    );
    report_jit_representative("jit_json_parse", JIT_JSON_PARSE_SOURCE);
    report_jit_representative(
        "jit_json_stringify",
        r#"
use std.json.Json
use std.json.stringify

fields = Map[String, Json]()
fields.put("name", Json.Text("loom"))
fields.put("ready", Json.Boolean(true))
values: [Json] = [Json.Number(1.0), Json.Number(2.0), Json.Number(3.0)]
fields.put("values", Json.ListValue(values))
document = Json.Object(fields)
round = 0
total = 0
while round < 2000
  case stringify(document)
  in Ok(text) then total = total + text.len()
  in Err(_) then total = total - 1000
  end
  round = round + 1
end
total
"#,
    );
    report_jit_representative(
        "jit_http_parse",
        r#"
use std.http.Http

http = Http()
limits = http.default_limits()
wire = Bytes("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Loom: ready\r\n\r\nworld")
round = 0
total = 0
while round < 2000
  case http.parse_response(wire, "GET", limits)
  in Ok(response) then total = total + response.status + response.body.len()
  in Err(_) then total = total - 1000
  end
  round = round + 1
end
total
"#,
    );
    report_jit_representative(
        "jit_http_serialize",
        r#"
use std.http.Http
use std.http.HttpHeader
use std.http.HttpRequest

http = Http()
limits = http.default_limits()
request = HttpRequest(
  "POST",
  "/echo",
  [HttpHeader("Content-Type", Bytes("text/plain"))],
  Bytes("hello")
)
round = 0
total = 0
while round < 2000
  case http.serialize_request("example.test", 80, request, limits)
  in Ok(wire) then total = total + wire.len()
  in Err(_) then total = total - 1000
  end
  round = round + 1
end
total
"#,
    );
}

#[test]
#[ignore]
fn bench_jit_application_programs() {
    println!(
        "LOOM_JIT_PROGRAM\tcase\tinterpreter_ms\tauto_ms\tnative_ms\tauto_speedup\tnative_speedup\tauto_coverage\tnative_coverage\tauto_compiles\tauto_demotions\tauto_unsupported\tnative_unsupported\tauto_interpreter_exits\tnative_interpreter_exits\tauto_env_exits\tnative_env_exits\tnative_env_fallbacks"
    );
    report_jit_representative("jit_app_wordcount", JIT_WORDCOUNT_SOURCE);
    report_jit_representative("jit_app_csv_report", JIT_CSV_REPORT_SOURCE);
    report_jit_representative("jit_app_json_pipeline", JIT_JSON_PIPELINE_SOURCE);
    report_jit_representative("jit_app_json_parse_large", JIT_JSON_PARSE_LARGE_SOURCE);
    report_jit_representative("jit_app_scalar_nodiv", JIT_SCALAR_NODIV_SOURCE);
    report_jit_representative("jit_app_image_luma", JIT_IMAGE_LUMA_SOURCE);
    report_jit_representative("jit_app_top_level_loop", JIT_TOP_LEVEL_LOOP_SOURCE);
    report_jit_representative("jit_app_matmul", JIT_MATMUL_SOURCE);
    report_jit_representative("jit_app_sort_search", JIT_SORT_SEARCH_SOURCE);
    report_jit_representative("jit_app_expr_interpreter", JIT_EXPR_INTERPRETER_SOURCE);
    report_jit_representative("jit_app_particles", JIT_PARTICLES_SOURCE);
    report_jit_representative("jit_app_graph_bfs", JIT_GRAPH_BFS_SOURCE);
    report_jit_representative("jit_app_pipeline_style", JIT_PIPELINE_STYLE_SOURCE);
    report_jit_representative("jit_app_many_functions", JIT_MANY_FUNCTIONS_SOURCE);
    report_jit_representative("jit_app_gcx_churn_low", JIT_GCX_CHURN_LOW_SOURCE);
    report_jit_representative("jit_app_gcx_churn_high", JIT_GCX_CHURN_HIGH_SOURCE);
    report_jit_representative("jit_app_gcx_retained", JIT_GCX_RETAINED_SOURCE);
    report_jit_representative("jit_app_gcx_alloc_burst", JIT_GCX_ALLOC_BURST_SOURCE);
}

const JIT_SCALAR_NODIV_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/programs/scalar_nodiv.lm"
));
const JIT_IMAGE_LUMA_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/programs/image_luma.lm"
));
const JIT_TOP_LEVEL_LOOP_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/programs/top_level_loop.lm"
));
const JIT_MATMUL_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/programs/matmul.lm"
));
const JIT_SORT_SEARCH_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/programs/sort_search.lm"
));
const JIT_EXPR_INTERPRETER_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/programs/expr_interpreter.lm"
));
const JIT_PARTICLES_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/programs/particles.lm"
));
const JIT_GRAPH_BFS_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/programs/graph_bfs.lm"
));
const JIT_PIPELINE_STYLE_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/programs/pipeline_style.lm"
));
const JIT_MANY_FUNCTIONS_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/programs/many_functions.lm"
));
const JIT_GCX_CHURN_LOW_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/programs/gcx_churn_low.lm"
));
const JIT_GCX_CHURN_HIGH_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/programs/gcx_churn_high.lm"
));
const JIT_GCX_RETAINED_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/programs/gcx_retained.lm"
));
const JIT_GCX_ALLOC_BURST_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/programs/gcx_alloc_burst.lm"
));

const JIT_PROBE_ALLOC_CASE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/probes/alloc_case.lm"
));
const JIT_PROBE_MAP_STR: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/probes/map_str.lm"
));
const JIT_PROBE_SPLIT_BULK: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/probes/split_bulk.lm"
));
const JIT_PROBE_SPLIT_FIELDS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/probes/split_fields.lm"
));
const JIT_PROBE_HELPER_FLOOR: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/probes/helper_floor.lm"
));
const JIT_PROBE_TOSTRING: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/probes/tostring.lm"
));
const JIT_PROBE_BYTESCAN: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/jit/probes/bytescan.lm"
));

#[test]
#[ignore]
fn bench_jit_probe_programs() {
    println!(
        "LOOM_JIT_PROGRAM\tcase\tinterpreter_ms\tauto_ms\tnative_ms\tauto_speedup\tnative_speedup\tauto_coverage\tnative_coverage\tauto_compiles\tauto_demotions\tauto_unsupported\tnative_unsupported\tauto_interpreter_exits\tnative_interpreter_exits\tauto_env_exits\tnative_env_exits\tnative_env_fallbacks"
    );
    report_jit_representative("jit_probe_alloc_case", JIT_PROBE_ALLOC_CASE);
    report_jit_representative("jit_probe_map_str", JIT_PROBE_MAP_STR);
    report_jit_representative("jit_probe_split_bulk", JIT_PROBE_SPLIT_BULK);
    report_jit_representative("jit_probe_split_fields", JIT_PROBE_SPLIT_FIELDS);
    report_jit_representative("jit_probe_helper_floor", JIT_PROBE_HELPER_FLOOR);
    report_jit_representative("jit_probe_tostring", JIT_PROBE_TOSTRING);
    report_jit_representative("jit_probe_bytescan", JIT_PROBE_BYTESCAN);
}
