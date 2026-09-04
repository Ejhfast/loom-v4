use super::*;

#[test]
fn reflection_resumes_native_execution() {
    let source = r#"
use std.test

found = 0
for declaration in codeof(test).declarations()
  case declaration
  in Class[type C](class_descriptor)
    class_descriptor.name()
    found = found + 1
  in _ then ()
  end
end
i = 0
while i < 10000
  found = found + 1
  i = i + 1
end
found
"#;
    let (outcome, metrics, _) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(outcome, Outcome::Done(lm_value::Value::Int(10002)));
    assert!(metrics.native_retired_instructions > 10_000);
}
